//! Parse bounded HTML metadata without performing network requests.
//!
//! Returned URLs are untrusted. Network clients must validate DNS, addresses,
//! redirects and response sizes independently before fetching any resource.
use std::cell::{Cell, RefCell};

use html5ever::tokenizer::{
    BufferQueue, CharacterTokens, EndTag, StartTag, TagToken, Token, TokenSink, TokenSinkResult,
    Tokenizer,
};
use url::Url;

const MAX_HTML_BYTES: usize = 512 * 1024;
const MAX_TITLE_CHARS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkMetadata {
    pub title: Option<String>,
    pub image_url: Option<Url>,
    pub favicon_url: Option<Url>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataError {
    InvalidBase,
    TooLarge,
}

#[derive(Default)]
struct Sink {
    title: RefCell<String>,
    og_title: RefCell<Option<String>>,
    image: RefCell<Option<String>>,
    icon: RefCell<Option<String>>,
    in_title: Cell<bool>,
    in_head: Cell<bool>,
    head_seen: Cell<bool>,
}

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        match token {
            TagToken(tag) if tag.kind == StartTag => {
                let name = tag.name.as_ref();
                if name == "head" {
                    self.head_seen.set(true);
                    self.in_head.set(true);
                }
                if self.head_seen.get() && !self.in_head.get() {
                    return TokenSinkResult::Continue;
                }
                if name == "title" {
                    self.in_title.set(true);
                }
                let attr = |key: &str| {
                    tag.attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == key)
                        .map(|a| a.value.to_string())
                };
                if name == "meta" {
                    let property = attr("property")
                        .or_else(|| attr("name"))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if let Some(value) = attr("content").filter(|v| !v.trim().is_empty()) {
                        match property.as_str() {
                            "og:title" if self.og_title.borrow().is_none() => {
                                *self.og_title.borrow_mut() = Some(value)
                            }
                            "og:image" | "og:image:url" if self.image.borrow().is_none() => {
                                *self.image.borrow_mut() = Some(value)
                            }
                            _ => {}
                        }
                    }
                } else if name == "link" {
                    let rel = attr("rel").unwrap_or_default().to_ascii_lowercase();
                    if rel.split_ascii_whitespace().any(|part| part == "icon")
                        && self.icon.borrow().is_none()
                    {
                        *self.icon.borrow_mut() = attr("href").filter(|v| !v.trim().is_empty());
                    }
                }
            }
            TagToken(tag) if tag.kind == EndTag => {
                if tag.name.as_ref() == "title" {
                    self.in_title.set(false);
                }
                if tag.name.as_ref() == "head" {
                    self.in_head.set(false);
                }
            }
            CharacterTokens(value) if self.in_title.get() => {
                let mut title = self.title.borrow_mut();
                if title.len() < 4096 {
                    title.push_str(&value);
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn safe_relative_url(base: &Url, value: &str) -> Option<Url> {
    let mut url = base.join(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

pub fn parse_html(base: &str, html: &str) -> Result<LinkMetadata, MetadataError> {
    if html.len() > MAX_HTML_BYTES {
        return Err(MetadataError::TooLarge);
    }
    let base = Url::parse(base).map_err(|_| MetadataError::InvalidBase)?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(MetadataError::InvalidBase);
    }
    let tokenizer = Tokenizer::new(Sink::default(), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let sink = tokenizer.sink;
    let title = sink
        .og_title
        .into_inner()
        .unwrap_or_else(|| sink.title.into_inner());
    let title: String = title
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_TITLE_CHARS)
        .collect();
    Ok(LinkMetadata {
        title: (!title.is_empty()).then_some(title),
        image_url: sink
            .image
            .into_inner()
            .and_then(|value| safe_relative_url(&base, &value)),
        favicon_url: sink
            .icon
            .into_inner()
            .and_then(|value| safe_relative_url(&base, &value))
            .or_else(|| base.join("/favicon.ico").ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_og_title_image_and_favicon_with_real_html_parsing() {
        let html = r#"<head><title>Normal &amp; title</title><meta content='Headline &amp; Co' property='og:title'><meta content='/social.png' property='og:image'><link href='../icon.png' rel='shortcut icon'></head>"#;
        let m = parse_html("https://example.com/path/article", html).unwrap();
        assert_eq!(m.title.as_deref(), Some("Headline & Co"));
        assert_eq!(
            m.image_url.unwrap().as_str(),
            "https://example.com/social.png"
        );
        assert_eq!(
            m.favicon_url.unwrap().as_str(),
            "https://example.com/icon.png"
        );
    }

    #[test]
    fn rejects_unusable_urls_and_falls_back_to_favicon() {
        let m = parse_html("https://example.com/a", "<head><meta property='og:image' content='file:///secret'><link rel=icon href='javascript:alert(1)'></head>").unwrap();
        assert!(m.image_url.is_none());
        assert_eq!(
            m.favicon_url.unwrap().as_str(),
            "https://example.com/favicon.ico"
        );
        assert_eq!(
            parse_html("https://user:pass@example.com", ""),
            Err(MetadataError::InvalidBase)
        );
        assert_eq!(parse_html("file:///a", ""), Err(MetadataError::InvalidBase));
        assert_eq!(
            parse_html("https://example.com", &"a".repeat(MAX_HTML_BYTES + 1)),
            Err(MetadataError::TooLarge)
        );
    }

    #[test]
    fn ignores_metadata_after_head() {
        let m = parse_html("https://example.com", "<head><title>Safe</title></head><body><meta property='og:title' content='wrong'></body>").unwrap();
        assert_eq!(m.title.as_deref(), Some("Safe"));
    }
}
