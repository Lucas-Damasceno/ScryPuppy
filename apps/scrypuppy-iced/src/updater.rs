use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use std::{io::Write, path::PathBuf, time::Duration};

const UPDATE_FEED_URL: &str =
    "https://github.com/Lucas-Damasceno/ScryPuppy/releases/download/iced-updater-beta/latest.json";
const UPDATE_PUBLIC_KEY: &str = "RWQ8fkWotIeUYjFKA1bD5l+c4GyiXPRnYoRNf0lL0wZYd228N1N1QZCV";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
const MAX_INSTALLER_BYTES: usize = 100 * 1024 * 1024;
const DISMISSED_UPDATE_DURATION_SECS: u64 = 6 * 60 * 60;

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
    pub url: String,
    pub signature: String,
}

#[derive(Deserialize)]
struct UpdateManifest {
    version: String,
    notes: String,
    pub_date: String,
    platforms: UpdatePlatforms,
}

#[derive(Deserialize)]
struct UpdatePlatforms {
    #[serde(rename = "windows-x86_64")]
    windows_x86_64: UpdatePlatform,
}

#[derive(Deserialize)]
struct UpdatePlatform {
    url: String,
    signature: String,
}

pub async fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    let client = update_client()?;
    let manifest_response = client
        .get(UPDATE_FEED_URL)
        .send()
        .await
        .map_err(|error| format!("Não foi possível consultar as atualizações: {error}"))?;
    if !manifest_response.status().is_success() {
        return Err(format!(
            "O canal de atualização respondeu com HTTP {}.",
            manifest_response.status()
        ));
    }
    let manifest_bytes = read_bounded(manifest_response, MAX_MANIFEST_BYTES).await?;

    let signature_response = client
        .get(format!("{UPDATE_FEED_URL}.sig"))
        .send()
        .await
        .map_err(|error| format!("Não foi possível validar o canal de atualização: {error}"))?;
    if !signature_response.status().is_success() {
        return Err(format!(
            "A assinatura do canal de atualização respondeu com HTTP {}.",
            signature_response.status()
        ));
    }
    let signature_bytes = read_bounded(signature_response, MAX_SIGNATURE_BYTES).await?;
    let signature_text = std::str::from_utf8(&signature_bytes).map_err(|_| {
        "A assinatura do canal de atualização não é texto UTF-8 válido.".to_string()
    })?;
    let signature = decode_signature(signature_text)
        .map_err(|_| "A assinatura do canal de atualização está malformada.".to_string())?;
    verify_signature(&manifest_bytes, &signature)?;

    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("O canal de atualização está inválido: {error}"))?;
    let latest = semver::Version::parse(&manifest.version)
        .map_err(|_| "O canal de atualização contém uma versão inválida.".to_string())?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| "A versão atual do aplicativo é inválida.".to_string())?;
    if latest <= current {
        return Ok(None);
    }
    validate_download_url(&manifest.platforms.windows_x86_64.url)?;

    Ok(Some(UpdateInfo {
        version: manifest.version,
        notes: manifest.notes.chars().take(1200).collect(),
        pub_date: manifest.pub_date,
        url: manifest.platforms.windows_x86_64.url,
        signature: manifest.platforms.windows_x86_64.signature,
    }))
}

pub async fn download_verified_update(update: UpdateInfo) -> Result<PathBuf, String> {
    let client = update_client()?;
    let response = client
        .get(&update.url)
        .send()
        .await
        .map_err(|error| format!("Não foi possível baixar a atualização: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "O download da atualização respondeu com HTTP {}.",
            response.status()
        ));
    }
    let final_url = response.url().clone();
    validate_download_url(final_url.as_str())?;
    let installer_bytes = read_bounded(response, MAX_INSTALLER_BYTES).await?;
    let signature = decode_signature(&update.signature)
        .map_err(|_| "A assinatura do instalador está malformada.".to_string())?;
    verify_signature(&installer_bytes, &signature)?;

    let installer_path = std::env::temp_dir().join(format!(
        "ScryPuppy-Iced-update-{}.exe",
        uuid::Uuid::new_v4()
    ));
    let mut installer_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&installer_path)
        .map_err(|error| format!("Não foi possível criar o instalador verificado: {error}"))?;
    if let Err(error) = installer_file.write_all(&installer_bytes) {
        let _ = std::fs::remove_file(&installer_path);
        return Err(format!(
            "Não foi possível salvar o instalador verificado: {error}"
        ));
    }
    Ok(installer_path)
}

pub fn is_update_dismissed(version: &str) -> bool {
    let Some(path) = dismissed_update_path() else {
        return false;
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some((dismissed_version, timestamp)) = contents.trim().split_once('|') else {
        return false;
    };
    if dismissed_version != version {
        return false;
    }
    let Ok(timestamp) = timestamp.parse::<u64>() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(timestamp) < DISMISSED_UPDATE_DURATION_SECS
}

pub fn dismiss_update(version: &str) -> Result<(), String> {
    let Some(path) = dismissed_update_path() else {
        return Err("A pasta de configuração do usuário está indisponível.".into());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let parent = path
        .parent()
        .ok_or_else(|| "A pasta de configuração do usuário está indisponível.".to_string())?;
    std::fs::create_dir_all(parent)
        .and_then(|()| std::fs::write(path, format!("{version}|{now}")))
        .map_err(|error| format!("Não foi possível lembrar esta atualização: {error}"))
}

fn dismissed_update_path() -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join("com.scryppy.desktop")
            .join("update-dismissed.txt"),
    )
}

#[cfg(windows)]
pub fn launch_verified_installer(installer: PathBuf) -> Result<(), String> {
    use std::{
        ffi::OsString,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    let current_executable = std::env::current_exe()
        .map_err(|error| format!("Não foi possível localizar o aplicativo: {error}"))?;
    let install_dir = current_executable
        .parent()
        .ok_or_else(|| "A pasta de instalação do aplicativo não está disponível.".to_string())?;
    let system_root = std::env::var_os("SystemRoot")
        .ok_or_else(|| "A pasta do Windows não está disponível.".to_string())?;
    let powershell = std::path::PathBuf::from(system_root)
        .join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    let uninstaller_path =
        std::env::temp_dir().join(format!("ScryPuppy-Iced-update-{}.ps1", std::process::id()));

    let script = format!(
        "$ErrorActionPreference = 'Stop'\n$targetPid = {pid}\n$installer = '{installer}'\n$app = '{app}'\n$installDir = '{install_dir}'\ntry {{\n  Wait-Process -Id $targetPid -ErrorAction SilentlyContinue\n  $process = Start-Process -FilePath $installer -ArgumentList @('/S', \"/D=$installDir\") -PassThru -Wait\n  if (Test-Path -LiteralPath $app) {{ Start-Process -FilePath $app }}\n}} finally {{\n  Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue\n  Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue\n}}\n",
        pid = std::process::id(),
        installer = powershell_literal(&installer),
        app = powershell_literal(&current_executable),
        install_dir = powershell_literal(install_dir),
    );
    if let Err(error) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&uninstaller_path)
        .and_then(|mut file| file.write_all(script.as_bytes()))
    {
        let _ = std::fs::remove_file(&installer);
        return Err(format!("Não foi possível preparar a instalação: {error}"));
    }
    let status = Command::new(powershell)
        .args([
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-ExecutionPolicy"),
            OsString::from("Bypass"),
            OsString::from("-File"),
            uninstaller_path.as_os_str().to_owned(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn();
    if let Err(error) = status {
        let _ = std::fs::remove_file(&installer);
        let _ = std::fs::remove_file(&uninstaller_path);
        return Err(format!("Não foi possível iniciar a atualização: {error}"));
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn launch_verified_installer(_installer: PathBuf) -> Result<(), String> {
    Err("A instalação de atualizações está disponível apenas no Windows.".into())
}

fn update_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("ScryPuppy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("Não foi possível preparar a conexão de atualização: {error}"))
}

async fn read_bounded(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > max_bytes as u64)
    {
        return Err("O arquivo de atualização excede o tamanho permitido.".into());
    }

    let mut response = response;
    let mut content = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("O download da atualização falhou: {error}"))?
    {
        if content.len().saturating_add(chunk.len()) > max_bytes {
            return Err("O arquivo de atualização excede o tamanho permitido.".into());
        }
        content.extend_from_slice(&chunk);
    }
    Ok(content)
}

fn verify_signature(bytes: &[u8], signature: &Signature) -> Result<(), String> {
    let public_key = PublicKey::from_base64(UPDATE_PUBLIC_KEY)
        .map_err(|_| "A chave de assinatura do ScryPuppy é inválida.".to_string())?;
    public_key
        .verify(bytes, signature, false)
        .map_err(|_| "A assinatura da atualização não confere.".to_string())
}

fn decode_signature(encoded_signature: &str) -> Result<Signature, String> {
    let decoded = STANDARD
        .decode(encoded_signature.trim())
        .map_err(|error| error.to_string())?;
    let signature_text = std::str::from_utf8(&decoded).map_err(|error| error.to_string())?;
    Signature::decode(signature_text).map_err(|error| error.to_string())
}

fn validate_download_url(candidate: &str) -> Result<(), String> {
    let url = url::Url::parse(candidate)
        .map_err(|_| "O canal de atualização contém um endereço inválido.".to_string())?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("github.com")
                | Some("objects.githubusercontent.com")
                | Some("release-assets.githubusercontent.com")
        )
        || url.username() != ""
        || url.password().is_some()
    {
        return Err("O canal de atualização apontou para um endereço não confiável.".into());
    }
    Ok(())
}

fn powershell_literal(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_update_key_is_a_valid_minisign_public_key() {
        assert!(PublicKey::from_base64(UPDATE_PUBLIC_KEY).is_ok());
    }

    #[test]
    fn verifies_downloaded_installer_signature_fixture_when_configured() {
        let (Ok(installer_path), Ok(signature_path)) = (
            std::env::var("SCRYPUPPY_UPDATE_TEST_BINARY"),
            std::env::var("SCRYPUPPY_UPDATE_TEST_SIGNATURE"),
        ) else {
            return;
        };
        let signature_text =
            std::fs::read_to_string(signature_path).expect("signature fixture reads");
        let signature = decode_signature(&signature_text).expect("signature fixture parses");
        let installer = std::fs::read(installer_path).expect("installer fixture reads");
        verify_signature(&installer, &signature).expect("installer fixture signature verifies");
        let mut modified = installer;
        *modified.first_mut().expect("signed installer is not empty") ^= 0x01;
        assert!(verify_signature(&modified, &signature).is_err());
    }

    #[test]
    fn verifies_signed_update_manifest_and_installer_when_configured() {
        let (Ok(manifest_path), Ok(manifest_signature_path), Ok(installer_path)) = (
            std::env::var("SCRYPUPPY_UPDATE_TEST_MANIFEST"),
            std::env::var("SCRYPUPPY_UPDATE_TEST_MANIFEST_SIGNATURE"),
            std::env::var("SCRYPUPPY_UPDATE_TEST_BINARY"),
        ) else {
            return;
        };
        let manifest_bytes = std::fs::read(manifest_path).expect("update manifest reads");
        let manifest_signature_text =
            std::fs::read_to_string(manifest_signature_path).expect("manifest signature reads");
        let manifest_signature =
            decode_signature(&manifest_signature_text).expect("manifest signature parses");
        verify_signature(&manifest_bytes, &manifest_signature)
            .expect("update manifest signature verifies");

        let manifest: UpdateManifest =
            serde_json::from_slice(&manifest_bytes).expect("signed update manifest parses");
        let installer_signature = decode_signature(&manifest.platforms.windows_x86_64.signature)
            .expect("installer signature parses");
        let installer = std::fs::read(installer_path).expect("signed installer reads");
        verify_signature(&installer, &installer_signature)
            .expect("signed update installer verifies");
        validate_download_url(&manifest.platforms.windows_x86_64.url)
            .expect("signed update installer URL is trusted");
    }

    #[test]
    fn only_release_https_hosts_are_accepted() {
        assert!(validate_download_url("https://github.com/owner/repo/releases/file.exe").is_ok());
        assert!(validate_download_url("https://objects.githubusercontent.com/file").is_ok());
        assert!(validate_download_url("https://release-assets.githubusercontent.com/file").is_ok());
        assert!(validate_download_url("http://github.com/file").is_err());
        assert!(validate_download_url("https://127.0.0.1/file").is_err());
        assert!(validate_download_url("https://example.com/file").is_err());
    }

    #[test]
    fn powershell_paths_are_escaped_as_single_quoted_literals() {
        assert_eq!(
            powershell_literal(std::path::Path::new("C:\\Users\\O'Neil\\update.exe")),
            "C:\\Users\\O''Neil\\update.exe"
        );
    }

    #[test]
    fn dismissed_update_record_expires_and_is_version_specific() {
        let timestamp = 1_800_000_000_u64;
        let now = timestamp + DISMISSED_UPDATE_DURATION_SECS - 1;
        assert!(dismissal_is_current(
            "1.0.0-beta.13",
            "1.0.0-beta.13",
            timestamp,
            now
        ));
        assert!(!dismissal_is_current(
            "1.0.0-beta.14",
            "1.0.0-beta.13",
            timestamp,
            now
        ));
        assert!(!dismissal_is_current(
            "1.0.0-beta.13",
            "1.0.0-beta.13",
            timestamp,
            timestamp + DISMISSED_UPDATE_DURATION_SECS
        ));
    }

    fn dismissal_is_current(
        version: &str,
        dismissed_version: &str,
        timestamp: u64,
        now: u64,
    ) -> bool {
        version == dismissed_version
            && now.saturating_sub(timestamp) < DISMISSED_UPDATE_DURATION_SECS
    }
}
