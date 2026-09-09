use crate::models::CoreManifest;
use crate::process::hide_window;
use crate::storage::{Paths, load_sing_box_manifest, save_sing_box_manifest, set_file_mode};
use anyhow::{Context, Result, bail};
#[cfg(target_os = "linux")]
use flate2::read::GzDecoder;
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(target_os = "linux")]
use tar::Archive;
use uuid::Uuid;

const RELEASE_API: &str = "https://api.github.com/repos/SagerNet/sing-box/releases/latest";
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 96 * 1024 * 1024;
const BUNDLED_SING_BOX_VERSION: &str = "v1.13.18";

#[derive(Debug, Clone)]
pub struct SingBoxRelease {
    pub version: String,
    pub archive_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct SingBoxInstallation {
    pub version: String,
    pub binary: PathBuf,
    pub directory: PathBuf,
}

#[derive(Debug, Clone)]
pub enum SingBoxProgress {
    Downloading { received: u64, total: u64 },
    Verifying,
    Installing,
    Finished { version: String, path: PathBuf },
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

pub fn installed_sing_box(paths: &Paths) -> Result<Option<SingBoxInstallation>> {
    if let Some(directory) = bundled_core_directory() {
        let binary = directory.join(binary_name());
        if binary.is_file() {
            return Ok(Some(SingBoxInstallation {
                version: BUNDLED_SING_BOX_VERSION.into(),
                binary,
                directory,
            }));
        }
    }
    let Some(manifest) = load_sing_box_manifest(paths)? else {
        return Ok(None);
    };
    let directory = paths
        .sing_box_dir()
        .join("releases")
        .join(&manifest.directory);
    let binary = directory.join(binary_name());
    if !binary.is_file() {
        return Ok(None);
    }
    Ok(Some(SingBoxInstallation {
        version: manifest.version,
        binary,
        directory,
    }))
}

fn bundled_core_directory() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Some(PathBuf::from("/usr/lib/nory/cores/sing-box"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(|directory| directory.join("cores").join("sing-box"))
    }
}

pub fn check_latest(client: &Client) -> Result<SingBoxRelease> {
    let release: GithubRelease = client
        .get(RELEASE_API)
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .context("не удалось проверить обновление sing-box")?
        .error_for_status()
        .context("GitHub вернул ошибку при проверке sing-box")?
        .json()
        .context("некорректный ответ GitHub Releases для sing-box")?;

    if release.draft || release.prerelease {
        bail!("последний релиз sing-box не является стабильным");
    }
    let expected = archive_name(&release.tag_name)?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == expected)
        .with_context(|| format!("в релизе sing-box нет файла {expected}"))?;
    if asset.size == 0 || asset.size > MAX_ARCHIVE_BYTES {
        bail!("некорректный размер архива sing-box");
    }
    validate_download_url(&asset.browser_download_url)?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .context("релиз sing-box не содержит SHA-256")?;
    if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("некорректный SHA-256 релиза sing-box");
    }

    Ok(SingBoxRelease {
        version: release.tag_name,
        archive_name: asset.name,
        download_url: asset.browser_download_url,
        sha256: digest.to_ascii_lowercase(),
        size: asset.size,
    })
}

pub fn update_available(paths: &Paths, release: &SingBoxRelease) -> Result<bool> {
    Ok(installed_sing_box(paths)?.is_none_or(|installed| installed.version != release.version))
}

pub fn install_release<F>(
    paths: &Paths,
    client: &Client,
    release: &SingBoxRelease,
    mut progress: F,
) -> Result<SingBoxInstallation>
where
    F: FnMut(SingBoxProgress),
{
    fs::create_dir_all(paths.sing_box_dir().join("releases"))?;
    let stage = paths
        .sing_box_dir()
        .join(format!(".stage-{}", Uuid::new_v4()));
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
    release: &SingBoxRelease,
    stage: &Path,
    progress: &mut F,
) -> Result<SingBoxInstallation>
where
    F: FnMut(SingBoxProgress),
{
    let archive_path = stage.join(&release.archive_name);
    let response = client
        .get(&release.download_url)
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")))
        .send()
        .context("не удалось скачать sing-box")?
        .error_for_status()
        .context("сервер вернул ошибку при загрузке sing-box")?;
    download(response, &archive_path, release.size, progress)?;

    progress(SingBoxProgress::Verifying);
    if sha256_file(&archive_path)? != release.sha256 {
        bail!("контрольная сумма sing-box не совпадает; установка отменена");
    }

    progress(SingBoxProgress::Installing);
    extract_binary(&archive_path, stage)?;
    fs::remove_file(&archive_path)?;
    let binary = stage.join(binary_name());
    set_file_mode(&binary, true)?;
    validate_binary(&binary, stage)?;

    let directory_name = format!(
        "{}-{}",
        safe_version(&release.version),
        &release.sha256[..12]
    );
    let target = paths.sing_box_dir().join("releases").join(&directory_name);
    if target.exists() {
        fs::remove_dir_all(stage)?;
    } else {
        fs::rename(stage, &target)?;
    }

    save_sing_box_manifest(
        paths,
        &CoreManifest {
            version: release.version.clone(),
            directory: directory_name,
            installed_at: unix_time(),
            sha256: release.sha256.clone(),
        },
    )?;
    let installation = SingBoxInstallation {
        version: release.version.clone(),
        binary: target.join(binary_name()),
        directory: target,
    };
    progress(SingBoxProgress::Finished {
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
    F: FnMut(SingBoxProgress),
{
    if response
        .content_length()
        .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
    {
        bail!("архив sing-box слишком большой");
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
            bail!("архив sing-box превысил допустимый размер");
        }
        output.write_all(&buffer[..count])?;
        progress(SingBoxProgress::Downloading {
            received,
            total: expected_size,
        });
    }
    output.sync_all()?;
    if received != expected_size {
        bail!("архив sing-box загружен не полностью");
    }
    Ok(())
}

fn extract_binary(archive_path: &Path, destination: &Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let file = File::open(archive_path)?;
        let mut archive = zip::ZipArchive::new(file).context("повреждённый архив sing-box")?;
        let mut extracted = false;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.is_dir()
                || entry
                    .enclosed_name()
                    .as_deref()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    != Some(binary_name())
            {
                continue;
            }
            if extracted || entry.size() == 0 || entry.size() > MAX_BINARY_BYTES {
                bail!("некорректный архив sing-box");
            }
            let mut target = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination.join(binary_name()))?;
            std::io::copy(&mut entry, &mut target)?;
            target.sync_all()?;
            extracted = true;
        }
        if !extracted {
            bail!("в архиве sing-box отсутствует исполняемый файл");
        }
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        let decoder = GzDecoder::new(File::open(archive_path)?);
        let mut archive = Archive::new(decoder);
        let mut extracted = false;
        for entry in archive.entries().context("повреждённый архив sing-box")? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file()
                || entry.path()?.file_name().and_then(|name| name.to_str()) != Some(binary_name())
            {
                continue;
            }
            if extracted {
                bail!("архив sing-box содержит несколько исполняемых файлов");
            }
            if entry.size() == 0 || entry.size() > MAX_BINARY_BYTES {
                bail!("некорректный размер исполняемого файла sing-box");
            }
            let path = destination.join(binary_name());
            let mut target = OpenOptions::new().create_new(true).write(true).open(path)?;
            std::io::copy(&mut entry, &mut target)?;
            target.sync_all()?;
            extracted = true;
        }
        if !extracted {
            bail!("в архиве sing-box отсутствует исполняемый файл");
        }
        Ok(())
    }
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
        .context("не удалось запустить загруженное ядро sing-box")?;
    if !output.status.success() {
        bail!("загруженное ядро sing-box не прошло проверку запуска");
    }
    let description = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !description.to_ascii_lowercase().contains("sing-box") {
        bail!("загруженный файл не похож на sing-box");
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

fn archive_name(version: &str) -> Result<String> {
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || !version
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
    {
        bail!("некорректная версия sing-box в GitHub Releases");
    }
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        architecture => bail!("архитектура {architecture} пока не поддерживается sing-box"),
    };
    #[cfg(target_os = "linux")]
    let archive = format!("sing-box-{version}-linux-{architecture}.tar.gz");
    #[cfg(target_os = "windows")]
    let archive = format!("sing-box-{version}-windows-{architecture}.zip");
    Ok(archive)
}

fn binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "sing-box.exe"
    } else {
        "sing-box"
    }
}

fn validate_download_url(raw: &str) -> Result<()> {
    let url = url::Url::parse(raw)?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        bail!("GitHub вернул недоверенный адрес загрузки sing-box");
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
        "sing-box".into()
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
