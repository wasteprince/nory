use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

const UPDATE_RELEASES: &str = "https://github.com/wasteprince/nory/releases";
const UPDATE_PUBLIC_KEY_BASE64: &str = "6EhlbuPGEkEoV/Vy3BlgshMjsJSWhZgLt98x+EWqONc=";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ARTIFACT_BYTES: u64 = 384 * 1024 * 1024;

#[cfg(target_os = "windows")]
const CHANNEL: &str = "windows";
#[cfg(all(target_os = "linux", feature = "debian-package"))]
const CHANNEL: &str = "debian";
#[cfg(all(target_os = "linux", not(feature = "debian-package")))]
const CHANNEL: &str = "arch";

#[derive(Debug, Clone, Deserialize)]
pub struct AppRelease {
    pub channel: String,
    pub version: String,
    pub artifact: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub notes: String,
    pub published_at: String,
}

#[derive(Debug, Deserialize)]
struct SignedManifest {
    payload: String,
    signature: String,
}

#[derive(Debug, Clone)]
pub enum AppUpdateProgress {
    Downloading { received: u64, total: u64 },
    Verifying,
}

pub fn check_for_update() -> Result<Option<AppRelease>> {
    let client = update_client()?;
    let response = fetch_manifest(&client)?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MANIFEST_BYTES)
    {
        bail!("манифест обновления слишком большой");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("манифест обновления слишком большой");
    }
    let release = parse_manifest(&bytes)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let available = Version::parse(&release.version)?;
    Ok((available > current).then_some(release))
}

fn fetch_manifest(client: &Client) -> Result<reqwest::blocking::Response> {
    let response = client
        .get(manifest_url())
        .timeout(Duration::from_secs(15))
        .send()
        .context("сервер обновлений NORY недоступен")?;
    // A Windows preview may coexist with an older Linux-only stable release.
    // Validate this installation's signed manifest until a stable Windows
    // channel exists; do not mistake network/TLS errors for "no update".
    #[cfg(target_os = "windows")]
    let response = if response.status() == reqwest::StatusCode::NOT_FOUND {
        client
            .get(format!(
                "{UPDATE_RELEASES}/download/v{}/windows-manifest.json",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(15))
            .send()
            .context("манифест Windows-сборки недоступен")?
    } else {
        response
    };
    response
        .error_for_status()
        .context("сервер обновлений вернул ошибку")
}

fn parse_manifest(bytes: &[u8]) -> Result<AppRelease> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("манифест обновления слишком большой");
    }
    let signed: SignedManifest =
        serde_json::from_slice(&bytes).context("сервер вернул некорректный манифест")?;
    let payload = STANDARD
        .decode(signed.payload)
        .context("повреждён payload манифеста")?;
    let signature_bytes = STANDARD
        .decode(signed.signature)
        .context("повреждена подпись манифеста")?;
    let signature =
        Signature::from_slice(&signature_bytes).context("некорректная длина подписи обновления")?;
    let public_key = STANDARD
        .decode(UPDATE_PUBLIC_KEY_BASE64)
        .context("встроенный ключ обновлений повреждён")?;
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("некорректный встроенный ключ обновлений"))?;
    VerifyingKey::from_bytes(&public_key)?
        .verify_strict(&payload, &signature)
        .context("подпись обновления не прошла проверку")?;

    let release: AppRelease =
        serde_json::from_slice(&payload).context("повреждены данные подписанного обновления")?;
    validate_release(&release)?;
    Ok(release)
}

pub fn download_update<F>(
    release: &AppRelease,
    cache_dir: &Path,
    mut progress: F,
) -> Result<PathBuf>
where
    F: FnMut(AppUpdateProgress),
{
    validate_release(release)?;
    let directory = cache_dir.join("updates").join(CHANNEL);
    fs::create_dir_all(&directory)?;
    let target = directory.join(&release.artifact);
    if target.is_file() && verify_artifact(&target, release).is_ok() {
        progress(AppUpdateProgress::Verifying);
        return Ok(target);
    }

    let stage = directory.join(format!(".download-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let client = update_client()?;
        let url = artifact_url(release)?;
        let mut response = client
            .get(url)
            .send()
            .context("не удалось скачать обновление NORY")?
            .error_for_status()
            .context("сервер не отдал файл обновления")?;
        if response
            .content_length()
            .is_some_and(|length| length != release.size)
        {
            bail!("размер файла на сервере не совпадает с манифестом");
        }
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stage)?;
        let mut hash = Sha256::new();
        let mut received = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            received = received.saturating_add(count as u64);
            if received > release.size || received > MAX_ARTIFACT_BYTES {
                bail!("файл обновления превысил ожидаемый размер");
            }
            output.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
            progress(AppUpdateProgress::Downloading {
                received,
                total: release.size,
            });
        }
        output.sync_all()?;
        if received != release.size {
            bail!("обновление загружено не полностью");
        }
        progress(AppUpdateProgress::Verifying);
        if hex::encode(hash.finalize()) != release.sha256 {
            bail!("SHA-256 обновления не совпадает с подписанным манифестом");
        }
        if target.exists() {
            fs::remove_file(&target)?;
        }
        fs::rename(&stage, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result?;
    Ok(target)
}

#[cfg(all(target_os = "linux", not(feature = "debian-package")))]
pub fn launch_update(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("загруженный установщик не найден");
    }
    let output = Command::new("/usr/bin/pkexec")
        .args(["/usr/bin/pacman", "-U", "--needed", "--noconfirm"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("не удалось запустить обновление через pacman")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if detail.is_empty() {
            bail!("pacman не установил обновление ({})", output.status);
        }
        bail!("pacman не установил обновление: {detail}");
    }
    Ok(())
}

#[cfg(all(target_os = "linux", feature = "debian-package"))]
pub fn launch_update(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("загруженный установщик не найден");
    }
    let output = Command::new("/usr/bin/pkexec")
        .args(["/usr/bin/dpkg", "--install"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("не удалось запустить обновление через dpkg")?;
    if !output.status.success() {
        bail!(
            "dpkg не установил обновление: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn launch_update(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("загруженный установщик не найден");
    }
    crate::windows::launch_installer(path)?;
    std::process::exit(0);
}

/// Replaces the running process with the freshly installed binary. Using
/// `exec` avoids racing two GTK application instances for the same D-Bus ID.
#[cfg(target_os = "linux")]
pub fn restart_after_update() -> Result<()> {
    let error = Command::new("/usr/bin/nory")
        .arg("--updated")
        .stdin(Stdio::null())
        .exec();
    Err(error).context("обновление установлено, но не удалось перезапустить NORY")
}

#[cfg(target_os = "windows")]
pub fn restart_after_update() -> Result<()> {
    let executable = std::env::current_exe().context("путь NORY недоступен")?;
    let pid = std::process::id().to_string();
    let mut command = Command::new(executable);
    command
        .args(["--restart-after-pid", &pid, "--updated"])
        .stdin(Stdio::null());
    crate::process::hide_window(&mut command)
        .spawn()
        .context("не удалось перезапустить NORY")?;
    Ok(())
}

fn update_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 || !trusted_update_url(attempt.url()) {
                attempt.error("небезопасное перенаправление обновления GitHub")
            } else {
                attempt.follow()
            }
        }))
        .user_agent(format!("NORY/{} ({CHANNEL})", env!("CARGO_PKG_VERSION")))
        .build()
        .context("не удалось создать клиент обновлений")
}

fn manifest_url() -> String {
    format!("{UPDATE_RELEASES}/latest/download/{CHANNEL}-manifest.json")
}

fn artifact_url(release: &AppRelease) -> Result<String> {
    validate_release(release)?;
    // Pin the package to the signed version, not /latest, which can change
    // between checking for an update and downloading it.
    Ok(format!(
        "{UPDATE_RELEASES}/download/v{}/{}",
        release.version, release.artifact
    ))
}

fn trusted_update_url(url: &url::Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return false;
    }
    match url.host_str() {
        Some("github.com") => url.path().starts_with("/wasteprince/nory/releases/"),
        Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => true,
        _ => false,
    }
}

fn validate_release(release: &AppRelease) -> Result<()> {
    if release.channel != CHANNEL {
        bail!("манифест предназначен для другого канала");
    }
    Version::parse(&release.version).context("некорректная версия в манифесте")?;
    if release.artifact.is_empty()
        || release.artifact.len() > 128
        || !release
            .artifact
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("некорректное имя файла обновления");
    }
    let expected_suffix = if cfg!(target_os = "windows") {
        ".exe"
    } else if cfg!(feature = "debian-package") {
        ".deb"
    } else {
        ".pkg.tar.zst"
    };
    if !release.artifact.ends_with(expected_suffix) {
        bail!("канал обновлений содержит пакет другого формата");
    }
    if release.sha256.len() != 64
        || !release.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || release.sha256 != release.sha256.to_ascii_lowercase()
    {
        bail!("некорректный SHA-256 обновления");
    }
    if release.size == 0 || release.size > MAX_ARTIFACT_BYTES {
        bail!("некорректный размер обновления");
    }
    if release.notes.len() > 8 * 1024 || release.published_at.len() > 64 {
        bail!("некорректное описание обновления");
    }
    Ok(())
}

fn verify_artifact(path: &Path, release: &AppRelease) -> Result<()> {
    if fs::metadata(path)?.len() != release.size {
        bail!("размер кэшированного обновления не совпадает");
    }
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if hex::encode(hash.finalize()) != release.sha256 {
        bail!("SHA-256 кэшированного обновления не совпадает");
    }
    Ok(())
}
