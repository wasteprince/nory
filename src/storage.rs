use crate::models::{AppData, CoreManifest};
use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Serialize, de::DeserializeOwned};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("io", "nory", "NORY")
            .context("не удалось определить домашние каталоги XDG")?;
        let legacy_vendor = ["no", "bium"].concat();
        let legacy_name = ["No", "bium"].concat();
        let legacy_dirs = ProjectDirs::from("io", &legacy_vendor, &legacy_name);
        #[cfg(target_os = "linux")]
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("nory-{}", unsafe { libc_uid() }))
            });
        #[cfg(target_os = "windows")]
        let runtime_dir = dirs.cache_dir().join("runtime");
        let paths = Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
            runtime_dir: runtime_dir.join("nory"),
        };
        if let Some(legacy) = legacy_dirs {
            migrate_legacy_directory(legacy.config_dir(), &paths.config_dir)?;
            migrate_legacy_directory(legacy.data_dir(), &paths.data_dir)?;
            migrate_legacy_directory(legacy.cache_dir(), &paths.cache_dir)?;
        }
        paths.ensure()?;
        Ok(paths)
    }

    #[cfg(target_os = "linux")]
    pub fn system() -> Self {
        Self {
            config_dir: PathBuf::from("/var/lib/nory/config"),
            data_dir: PathBuf::from("/var/lib/nory"),
            cache_dir: PathBuf::from("/var/cache/nory"),
            runtime_dir: PathBuf::from("/run/nory"),
        }
    }

    #[cfg(target_os = "windows")]
    pub fn system() -> Self {
        let root = std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("NORY");
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            runtime_dir: root.join("runtime"),
        }
    }

    fn ensure(&self) -> Result<()> {
        for directory in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.runtime_dir,
        ] {
            fs::create_dir_all(directory)
                .with_context(|| format!("не удалось создать {}", directory.display()))?;
            set_dir_mode(directory)?;
        }
        Ok(())
    }

    pub fn state_file(&self) -> PathBuf {
        self.config_dir.join("state.json")
    }
    pub fn core_dir(&self) -> PathBuf {
        self.data_dir.join("core")
    }
    pub fn core_manifest(&self) -> PathBuf {
        self.core_dir().join("active.json")
    }
    pub fn sing_box_dir(&self) -> PathBuf {
        self.data_dir.join("sing-box")
    }
    pub fn sing_box_manifest(&self) -> PathBuf {
        self.sing_box_dir().join("active.json")
    }
    pub fn geodata_dir(&self) -> PathBuf {
        self.data_dir.join("geodata")
    }
    pub fn generated_config(&self) -> PathBuf {
        self.runtime_dir.join("xray.json")
    }
}

fn migrate_legacy_directory(source: &Path, target: &Path) -> Result<()> {
    if target.exists() || !source.is_dir() {
        return Ok(());
    }
    copy_directory(source, target)
        .with_context(|| format!("не удалось перенести данные в {}", target.display()))
}

fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

pub fn load_state(paths: &Paths) -> Result<AppData> {
    if !paths.state_file().exists() {
        return Ok(AppData::default());
    }
    let mut data: AppData =
        read_json(&paths.state_file()).context("не удалось прочитать настройки NORY")?;
    data.version = crate::models::STATE_VERSION;
    let previous_subscription = data.selected_subscription;
    if data.selected_subscription.is_none_or(|id| {
        !data
            .subscriptions
            .iter()
            .any(|subscription| subscription.id == id)
    }) {
        data.selected_subscription = data
            .selected_profile
            .and_then(|id| data.profiles.iter().find(|profile| profile.id == id))
            .and_then(|profile| profile.subscription_id)
            .filter(|id| {
                data.subscriptions
                    .iter()
                    .any(|subscription| subscription.id == *id)
            })
            .or_else(|| {
                data.subscriptions
                    .first()
                    .map(|subscription| subscription.id)
            });
    }
    if let Some(subscription_id) = data.selected_subscription
        && data.selected_profile.is_none_or(|id| {
            !data
                .profiles
                .iter()
                .any(|profile| profile.id == id && profile.subscription_id == Some(subscription_id))
        })
    {
        data.selected_profile = data
            .profiles
            .iter()
            .find(|profile| profile.subscription_id == Some(subscription_id))
            .map(|profile| profile.id);
    }
    if data.settings.tun_interface_name == ["no", "bium0"].concat() {
        data.settings.tun_interface_name = "nory0".into();
        let _ = atomic_json(&paths.state_file(), &data);
    } else if previous_subscription != data.selected_subscription {
        let _ = atomic_json(&paths.state_file(), &data);
    }
    Ok(data)
}

pub fn save_state(paths: &Paths, data: &AppData) -> Result<()> {
    atomic_json(&paths.state_file(), data).context("не удалось сохранить настройки NORY")
}

pub fn load_or_create_hwid(paths: &Paths) -> Result<String> {
    let path = paths.config_dir.join("hwid.json");
    if path.exists() {
        let hwid: String = read_json(&path).context("не удалось прочитать HWID")?;
        validate_hwid(&hwid)?;
        return Ok(hwid);
    }
    let hwid = Uuid::new_v4().simple().to_string();
    atomic_json(&path, &hwid).context("не удалось сохранить HWID")?;
    Ok(hwid)
}

fn validate_hwid(hwid: &str) -> Result<()> {
    if !(16..=128).contains(&hwid.len())
        || !hwid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("локальный HWID повреждён");
    }
    Ok(())
}

pub fn load_core_manifest(paths: &Paths) -> Result<Option<CoreManifest>> {
    if !paths.core_manifest().exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&paths.core_manifest())?))
}

pub fn save_core_manifest(paths: &Paths, manifest: &CoreManifest) -> Result<()> {
    fs::create_dir_all(paths.core_dir())?;
    atomic_json(&paths.core_manifest(), manifest)
}

pub fn load_sing_box_manifest(paths: &Paths) -> Result<Option<CoreManifest>> {
    if !paths.sing_box_manifest().exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&paths.sing_box_manifest())?))
}

pub fn save_sing_box_manifest(paths: &Paths, manifest: &CoreManifest) -> Result<()> {
    fs::create_dir_all(paths.sing_box_dir())?;
    atomic_json(&paths.sing_box_manifest(), manifest)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("файл данных слишком большой");
    }
    serde_json::from_slice(&bytes).map_err(Into::into)
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .context("у файла нет родительского каталога")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file); // Windows cannot reliably rename an open file.
        #[cfg(target_os = "windows")]
        for attempt in 0..10 {
            match fs::rename(&temporary, path) {
                Ok(()) => return Ok(()),
                Err(error) if matches!(error.raw_os_error(), Some(5 | 32 | 33)) && attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(25))
                }
                Err(error) => return Err(error.into()),
            }
        }
        #[cfg(not(target_os = "windows"))]
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    set_file_mode(path, false)?;
    Ok(())
}

#[allow(unused_variables)]
pub fn set_file_mode(path: &Path, executable: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )?;
    }
    Ok(())
}

#[allow(unused_variables)]
fn set_dir_mode(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
unsafe fn libc_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(not(unix))]
unsafe fn libc_uid() -> u32 {
    0
}
