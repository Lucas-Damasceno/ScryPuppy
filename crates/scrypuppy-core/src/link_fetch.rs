//! Bounded network retrieval for link previews. Run this on a background worker.
//! Never log URLs: they can contain private query parameters.
use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use reqwest::{
    blocking::Client,
    header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use url::{Host, Url};

use crate::link_metadata::{parse_html, LinkMetadata};

const MAX_PAGE_BYTES: usize = 512 * 1024;
const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const MAX_RESOLVERS: usize = 4;
static ACTIVE_RESOLVERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    InvalidUrl,
    ForbiddenAddress,
    Dns,
    Timeout,
    Http,
    TooLarge,
    UnsupportedContent,
    InvalidHtml,
}

#[derive(Debug, Clone)]
pub struct RemotePreview {
    pub metadata: LinkMetadata,
    pub image: Option<Vec<u8>>,
    pub favicon: Option<Vec<u8>>,
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        || (a == 203 && b == 0 && c == 113))
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Permit only globally routed unicast, excluding documentation, benchmarking,
    // protocol-assignment and 6to4 ranges which can embed non-public IPv4.
    (segments[0] & 0xe000) == 0x2000
        && segments[0] != 0x2002
        && !(segments[0] == 0x2001 && (segments[1] <= 0x01ff || segments[1] == 0x0db8))
        && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => public_ipv4(v),
        IpAddr::V6(v) => public_ipv6(v),
    }
}

fn validate_url(url: &Url) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::InvalidUrl);
    }
    match url.host().ok_or(FetchError::InvalidUrl)? {
        Host::Ipv4(ip) if !public_address(IpAddr::V4(ip)) => Err(FetchError::ForbiddenAddress),
        Host::Ipv6(ip) if !public_address(IpAddr::V6(ip)) => Err(FetchError::ForbiddenAddress),
        Host::Domain(name)
            if name.eq_ignore_ascii_case("localhost")
                || name.to_ascii_lowercase().ends_with(".localhost") =>
        {
            Err(FetchError::ForbiddenAddress)
        }
        _ => Ok(()),
    }
}

fn resolve(host: String, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
    if ACTIVE_RESOLVERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_RESOLVERS).then_some(n + 1)
        })
        .is_err()
    {
        return Err(FetchError::Dns);
    }
    let (send, recv) = mpsc::sync_channel(1);
    if thread::Builder::new()
        .name("scrypuppy-preview-dns".into())
        .spawn(move || {
            let result = (host.as_str(), port)
                .to_socket_addrs()
                .map(|v| v.collect::<Vec<_>>());
            ACTIVE_RESOLVERS.fetch_sub(1, Ordering::AcqRel);
            let _ = send.send(result);
        })
        .is_err()
    {
        ACTIVE_RESOLVERS.fetch_sub(1, Ordering::AcqRel);
        return Err(FetchError::Dns);
    }
    let addrs = recv
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| FetchError::Timeout)?
        .map_err(|_| FetchError::Dns)?;
    validate_addresses(addrs)
}

fn validate_addresses(addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, FetchError> {
    if addrs.is_empty() {
        return Err(FetchError::Dns);
    }
    if addrs.iter().any(|addr| !public_address(addr.ip())) {
        return Err(FetchError::ForbiddenAddress);
    }
    Ok(addrs)
}

fn redirect_target(current: &Url, location: &str) -> Result<Url, FetchError> {
    let next = current.join(location).map_err(|_| FetchError::InvalidUrl)?;
    validate_url(&next)?;
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(FetchError::ForbiddenAddress);
    }
    Ok(next)
}

fn fetch(url: &Url, max_bytes: usize) -> Result<(Url, String, Vec<u8>), FetchError> {
    let mut current = url.clone();
    for hop in 0..=MAX_REDIRECTS {
        validate_url(&current)?;
        let port = current
            .port_or_known_default()
            .ok_or(FetchError::InvalidUrl)?;
        let builder = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(8))
            .connect_timeout(Duration::from_secs(3));
        let builder = if let Some(Host::Domain(host)) = current.host() {
            let addresses = resolve(host.to_string(), port)?;
            builder.resolve_to_addrs(host, &addresses)
        } else {
            // Literal addresses have already been checked by validate_url.
            builder
        };
        let client = builder.build().map_err(|_| FetchError::Http)?;
        let mut response = client.get(current.clone()).send().map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Http
            }
        })?;
        if response.status().is_redirection() {
            if hop == MAX_REDIRECTS {
                return Err(FetchError::Http);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(FetchError::Http)?;
            current = redirect_target(&current, location)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(FetchError::Http);
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|size| size > max_bytes as u64)
        {
            return Err(FetchError::TooLarge);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| FetchError::Http)?;
        if bytes.len() > max_bytes {
            return Err(FetchError::TooLarge);
        }
        return Ok((current, content_type, bytes));
    }
    Err(FetchError::Http)
}

fn fetch_image(url: &Url) -> Option<Vec<u8>> {
    let (_, _, bytes) = fetch(url, MAX_IMAGE_BYTES).ok()?;
    match image::guess_format(&bytes).ok()? {
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Ico => Some(bytes),
        _ => None,
    }
}

/// Fetch a public page and its advertised preview images. No ambient proxy or
/// cookies are used. Every DNS answer and redirect is checked and pinned.
pub fn fetch_link(raw_url: &str) -> Result<RemotePreview, FetchError> {
    let url = Url::parse(raw_url).map_err(|_| FetchError::InvalidUrl)?;
    validate_url(&url)?;
    let (final_url, content_type, html) = fetch(&url, MAX_PAGE_BYTES)?;
    if !(content_type.starts_with("text/html") || content_type.starts_with("application/xhtml+xml"))
    {
        return Err(FetchError::UnsupportedContent);
    }
    let html = std::str::from_utf8(&html).map_err(|_| FetchError::InvalidHtml)?;
    let metadata = parse_html(final_url.as_str(), html).map_err(|_| FetchError::InvalidHtml)?;
    let image = metadata.image_url.as_ref().and_then(fetch_image);
    let favicon = metadata.favicon_url.as_ref().and_then(fetch_image);
    Ok(RemotePreview {
        metadata,
        image,
        favicon,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_private_and_special_address_ranges() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "198.18.0.1",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
        ] {
            assert!(!public_address(value.parse().unwrap()), "{value}");
        }
        assert!(public_address("8.8.8.8".parse().unwrap()));
        assert!(public_address("2606:4700:4700::1111".parse().unwrap()));
    }
    #[test]
    fn rejects_credentials_non_http_and_local_hosts_without_resolving() {
        for value in [
            "http://localhost/",
            "http://x.localhost/",
            "http://127.1/",
            "http://[::1]/",
            "http://user:pass@example.com",
            "file:///etc/passwd",
        ] {
            assert!(
                validate_url(&Url::parse(value).unwrap()).is_err(),
                "{value}"
            );
        }
        assert!(validate_url(&Url::parse("https://example.com/").unwrap()).is_ok());
    }

    #[test]
    fn validates_redirects_and_rejects_tls_downgrade() {
        let base = Url::parse("https://example.com/path").unwrap();
        assert!(redirect_target(&base, "http://example.com/a").is_err());
        assert!(redirect_target(&base, "http://127.0.0.1/private").is_err());
        assert_eq!(
            redirect_target(&base, "/safe").unwrap().as_str(),
            "https://example.com/safe"
        );
    }

    #[test]
    fn mixed_dns_answers_are_rejected_before_any_connection() {
        let good = "8.8.8.8:443".parse().unwrap();
        let private = "127.0.0.1:443".parse().unwrap();
        assert_eq!(
            validate_addresses(vec![good, private]),
            Err(FetchError::ForbiddenAddress)
        );
        assert_eq!(validate_addresses(vec![]), Err(FetchError::Dns));
        assert_eq!(validate_addresses(vec![good]), Ok(vec![good]));
    }
}
