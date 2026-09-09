use crate::models::CoreManifest;
use crate::process::hide_window;
use crate::storage::{Paths, load_core_manifest, save_core_manifest, set_file_mode};
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use zip::ZipArchive;

const RELEASE_API: &str = "https://api.github.com/repos/XTLS/Xray-core/releases/latest";
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const BUNDLED_XRAY_VERSION: &str = "v26.3.27";

#[derive(Debug, Clone)]
pub struct CoreRelease {
    pub version: String,
    pub archive_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum UpdateProgress {
    Checking,
    Downloading { received: u64, total: Option<u64> },
    Verifying,
    Installing,
    Finished { version: String, path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct CoreInstallation {
    pub version: String,
    pub binary: PathBuf,
    pub directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

pub fn installed_core(paths: &Paths) -> Result<Option<CoreInstallation>> {
    if let Some(directory) = bundled_core_directory() {
        let binary = directory.join(core_binary_name());
        if binary.is_file()
            && directory.join("geoip.dat").is_file()
            && directory.join("geosite.dat").is_file()
        {
            return Ok(Some(CoreInstallation {
                version: BUNDLED_XRAY_VERSION.into(),
                binary,
                directory,
            }));
        }
    }
    let Some(manifest) = load_core_manifest(paths)? else {
        return Ok(None);
    };
    let directory = paths.core_dir().join("releases").join(&manifest.directory);
    let binary = directory.join(core_binary_name());
    if !binary.is_file() {
        return Ok(None);
    }
    Ok(Some(CoreInstallation {
        version: manifest.version,
        binary,
        directory,
    }))
}

fn bundled_core_directory() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Some(PathBuf::from("/usr/lib/nory/cores/xray"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(|directory| directory.join("cores").join("xray"))
    }
}

pub fn check_latest(client: &Client) -> Result<CoreRelease> {
    let release: GithubRelease = client
        .get(RELEASE_API)
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .context("не удалось проверить обновление Xray")?
        .error_for_status()
        .context("GitHub вернул ошибку при проверке Xray")?
        .json()
        .context("некорректный ответ GitHub Releases")?;

    if release.draft || release.prerelease {
        bail!("последний релиз Xray не является стабильным");
    }
    let expected = archive_name()?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == expected)
        .with_context(|| format!("в релизе Xray нет файла {expected}"))?;
    if asset.size == 0 || asset.size > MAX_ARCHIVE_BYTES {
        bail!("некорректный размер архива Xray");
    }
    validate_download_url(&asset.browser_download_url)?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .context("релиз Xray не содержит SHA-256")?;
    if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("некорректный SHA-256 релиза Xray");
    }

    Ok(CoreRelease {
        version: release.tag_name,
        archive_name: asset.name,
        download_url: asset.browser_download_url,
        sha256: digest.to_ascii_lowercase(),
        size: asset.size,
    })
}

pub fn update_available(paths: &Paths, release: &CoreRelease) -> Result<bool> {
    Ok(installed_core(paths)?.is_none_or(|installed| installed.version != release.version))
}

pub fn install_release<F>(
    paths: &Paths,
    client: &Client,
    release: &CoreRelease,
    mut progress: F,
) -> Result<CoreInstallation>
where
    F: FnMut(UpdateProgress),
{
    progress(UpdateProgress::Downloading {
        received: 0,
        total: Some(release.size),
    });
    fs::create_dir_all(paths.core_dir().join("releases"))?;
    let stage = paths.core_dir().join(format!(".stage-{}", Uuid::new_v4()));
    fs::create_dir_all(&stage)?;
    let result = install_into_stage(paths, client, release, &stage, &mut progress);
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

fn install_into_stage<F>(
    paths: &Paths,
    client: &Client,
    release: &CoreRelease,
    stage: &Path,
    progress: &mut F,
) -> Result<CoreInstallation>
where
    F: FnMut(UpdateProgress),
{
    let archive_path = stage.join("xray.zip");
    let response = client
        .get(&release.download_url)
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")))
        .send()
        .context("не удалось скачать Xray")?
        .error_for_status()
        .context("сервер вернул ошибку при загрузке Xray")?;
    download(response, &archive_path, release.size, progress)?;

    progress(UpdateProgress::Verifying);
    let actual = sha256_file(&archive_path)?;
    if actual != release.sha256 {
        bail!("контрольная сумма Xray не совпадает; установка отменена");
    }

    progress(UpdateProgress::Installing);
    extract_core(&archive_path, stage)?;
    fs::remove_file(&archive_path)?;
    let binary = stage.join("xray");
    set_file_mode(&binary, true)?;
    validate_binary(&binary, stage)?;

    let directory_name = format!(
        "{}-{}",
        safe_version(&release.version),
        &release.sha256[..12]
    );
    let target = paths.core_dir().join("releases").join(&directory_name);
    if target.exists() {
        fs::remove_dir_all(stage)?;
    } else {
        fs::rename(stage, &target)?;
    }

    let manifest = CoreManifest {
        version: release.version.clone(),
        directory: directory_name,
        installed_at: unix_time(),
        sha256: release.sha256.clone(),
    };
    save_core_manifest(paths, &manifest)?;
    let installation = CoreInstallation {
        version: release.version.clone(),
        binary: target.join(core_binary_name()),
        directory: target,
    };
    progress(UpdateProgress::Finished {
        version: installation.version.clone(),
        path: installation.binary.clone(),
    });
    Ok(installation)
}

fn download<F>(
    mut response: Response,
    path: &Path,
    expected_size: u64,
    progress: &mut F,
) -> Result<()>
where
    F: FnMut(UpdateProgress),
{
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
    {
        bail!("архив Xray слишком большой");
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut received = 0_u64;
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        received = received.saturating_add(count as u64);
        if received > MAX_ARCHIVE_BYTES {
            bail!("архив Xray превысил допустимый размер");
        }
        output.write_all(&buffer[..count])?;
        progress(UpdateProgress::Downloading {
            received,
            total: Some(expected_size),
        });
    }
    output.sync_all()?;
    if received != expected_size {
        bail!("архив Xray загружен не полностью");
    }
    Ok(())
}

fn extract_core(archive_path: &Path, destination: &Path) -> Result<()> {
    let file = File::open(archive_path)?;
    let mut archive = ZipArchive::new(file).context("повреждённый ZIP-архив Xray")?;
    let names = [core_binary_name(), "geoip.dat", "geosite.dat"];
    for name in &names {
        let mut source = archive
            .by_name(name)
            .with_context(|| format!("в архиве Xray отсутствует {name}"))?;
        if source.size() > 96 * 1024 * 1024 {
            bail!("файл {name} в архиве слишком большой");
        }
        let path = destination.join(name);
        let mut target = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        std::io::copy(&mut source, &mut target)?;
        target.sync_all()?;
        set_file_mode(&path, *name == core_binary_name())?;
    }
    Ok(())
}

fn validate_binary(binary: &Path, directory: &Path) -> Result<()> {
    let mut command = Command::new(binary);
    command
        .arg("version")
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = hide_window(&mut command)
        .output()
        .context("не удалось запустить загруженное ядро Xray")?;
    if !output.status.success() {
        bail!("загруженное ядро Xray не прошло проверку запуска");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.to_ascii_lowercase().contains("xray") {
        bail!("загруженный файл не похож на Xray");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn archive_name() -> Result<String> {
    let suffix = match std::env::consts::ARCH {
        "x86_64" => "64",
        "aarch64" => "arm64-v8a",
        "arm" => "arm32-v7a",
        architecture => bail!("архитектура {architecture} пока не поддерживается"),
    };
    #[cfg(target_os = "linux")]
    let platform = "linux";
    #[cfg(target_os = "windows")]
    let platform = "windows";
    Ok(format!("Xray-{platform}-{suffix}.zip"))
}

fn core_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "xray.exe"
    } else {
        "xray"
    }
}

fn validate_download_url(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        bail!("GitHub вернул недоверенный адрес загрузки");
    }
    Ok(())
}

fn safe_version(version: &str) -> String {
    let sanitized: String = version
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        .take(64)
        .collect();
    if sanitized.is_empty() {
        "xray".into()
    } else {
        sanitized
    }
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64
}

pub fn default_client() -> Result<Client> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::limited(5))
        .https_only(true)
        .build()
        .map_err(Into::into)
}
