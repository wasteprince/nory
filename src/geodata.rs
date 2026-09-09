//! Resolve GeoData references without dropping or broadening routing rules.
use crate::{
    models::{Profile, Settings},
    storage::Paths,
    updater::CoreInstallation,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_BYTES: u64 = 128 * 1024 * 1024;
// The provider-style `torrent` tag comes from roscomvpn's small custom database,
// not the generic Xray/RunetFreedom catalogue. Pin both revision and digest.
const TORRENT_DATA: &str = "https://raw.githubusercontent.com/hydraponique/roscomvpn-geosite/2041bdc2748776fd083ea3bb8790eab14a7aed25/geosite.dat";
const TORRENT_SHA256: &str = "765b86e4b6aed5da1a206304b5500c7668687fa1df8e8322c8a4961e1b672190";

pub fn validate_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value).context("Некорректный URL GeoData")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("GeoData требует HTTPS-ссылку без логина и пароля");
    }
    Ok(())
}

fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *bytes.get(*cursor).context("GeoData: оборванный protobuf")?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            bail!("GeoData: переполнение protobuf");
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    bail!("GeoData: некорректный protobuf")
}

fn fields(bytes: &[u8], mut visit: impl FnMut(u64, &[u8]) -> Result<()>) -> Result<()> {
    let mut cursor = 0;
    while cursor < bytes.len() {
        let key = read_varint(bytes, &mut cursor)?;
        if key >> 3 == 0 {
            bail!("GeoData: неверный номер поля");
        }
        let length = match key & 7 {
            0 => {
                read_varint(bytes, &mut cursor)?;
                continue;
            }
            1 => 8,
            2 => usize::try_from(read_varint(bytes, &mut cursor)?)?,
            5 => 4,
            _ => bail!("GeoData: неподдерживаемый protobuf"),
        };
        let end = cursor
            .checked_add(length)
            .context("GeoData: переполнение длины")?;
        let value = bytes.get(cursor..end).context("GeoData: оборванное поле")?;
        if key & 7 == 2 {
            visit(key >> 3, value)?;
        }
        cursor = end;
    }
    Ok(())
}

pub(crate) fn categories(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let mut tags = BTreeSet::new();
    fields(bytes, |field, entry| {
        if field == 1 {
            fields(entry, |field, value| {
                if field == 1 {
                    let tag = std::str::from_utf8(value)?.to_ascii_lowercase();
                    if tag.is_empty() || tag.len() > 128 {
                        bail!("GeoData: неверный тег");
                    }
                    tags.insert(tag);
                }
                Ok(())
            })?;
        }
        Ok(())
    })?;
    if tags.is_empty() {
        bail!("GeoData не содержит категорий (ожидается бинарный .dat, не HTML/JSON)");
    }
    Ok(tags)
}

fn read_data(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("GeoData превышает 128 МБ");
    }
    Ok(bytes)
}

fn download(url: &str, directory: &Path, only: Option<&str>) -> Result<PathBuf> {
    validate_url(url)?;
    let identity = format!("{url}#{}", only.unwrap_or("all"));
    let path = directory.join(format!(
        "nory-{}.dat",
        hex::encode(Sha256::digest(identity.as_bytes()))
    ));
    if path.is_file()
        && fs::metadata(&path)?
            .modified()?
            .elapsed()
            .unwrap_or_default()
            < Duration::from_secs(86400)
        && categories(&read_data(&path)?).is_ok()
    {
        return Ok(path);
    }
    let route = crate::network::DirectRoute::discover()?;
    let parsed = url::Url::parse(url)?;
    let host = parsed
        .host_str()
        .context("В URL GeoData отсутствует сервер")?;
    let address = route.resolve(host)?;
    let client = route
        .http(Duration::from_secs(30))
        .https_only(true)
        .resolve(
            host,
            std::net::SocketAddr::new(
                address.into(),
                parsed.port_or_known_default().unwrap_or(443),
            ),
        )
        .user_agent(format!("NORY/{}", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("GeoData превышает 128 МБ");
    }
    if url == TORRENT_DATA && hex::encode(Sha256::digest(&bytes)) != TORRENT_SHA256 {
        bail!("Контрольная сумма дополнительной GeoData не совпала");
    }
    categories(&bytes)?;
    let bytes = if let Some(tag) = only {
        single_category(&bytes, tag)?
    } else {
        bytes
    };
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    Ok(path)
}

fn single_category(bytes: &[u8], tag: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    fields(bytes, |field, entry| {
        if field != 1 {
            return Ok(());
        }
        let mut matched = false;
        fields(entry, |field, value| {
            if field == 1 && std::str::from_utf8(value)?.eq_ignore_ascii_case(tag) {
                matched = true;
            }
            Ok(())
        })?;
        if matched {
            output.push(10);
            let mut length = entry.len();
            while length >= 128 {
                output.push((length as u8 & 127) | 128);
                length >>= 7;
            }
            output.push(length as u8);
            output.extend_from_slice(entry);
        }
        Ok(())
    })?;
    if output.is_empty() {
        bail!("Дополнительная GeoData не содержит категорию {tag}");
    }
    Ok(output)
}

fn references(value: &Value, refs: &mut BTreeSet<String>) {
    match value {
        Value::String(s)
            if s.starts_with("geosite:") || s.starts_with("geoip:") || s.starts_with("ext:") =>
        {
            refs.insert(s.clone());
        }
        Value::Array(items) => items.iter().for_each(|v| references(v, refs)),
        Value::Object(items) => {
            for (key, v) in items {
                references(&Value::String(key.clone()), refs);
                references(v, refs);
            }
        }
        _ => (),
    }
}
fn replace(value: &mut Value, mapping: &BTreeMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(to) = mapping.get(s) {
                *s = to.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| replace(v, mapping)),
        Value::Object(items) => {
            for v in items.values_mut() {
                replace(v, mapping);
            }
            for (from, to) in mapping {
                if let Some(v) = items.remove(from) {
                    items.insert(to.clone(), v);
                }
            }
        }
        _ => (),
    }
}

pub fn prepare(
    config: &mut Value,
    profile: &Profile,
    settings: &Settings,
    installation: &CoreInstallation,
    paths: &Paths,
) -> Result<PathBuf> {
    let directory = paths.geodata_dir();
    fs::create_dir_all(&directory)?;
    let mut refs = BTreeSet::new();
    references(config, &mut refs);
    let mut mapping = BTreeMap::new();
    let mut indices: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    for reference in refs {
        let (kind, tag) = reference.split_once(':').unwrap();
        let (path, category) = if kind == "ext" {
            let (file, tag) = tag
                .split_once(':')
                .context("GeoData: ожидается ext:имя.dat:категория")?;
            if file.is_empty() || file.contains(['/', '\\', ':']) || file == "." || file == ".." {
                bail!("GeoData: небезопасное имя файла");
            }
            (directory.join(file), tag)
        } else {
            let configured = if kind == "geosite" {
                &settings.geosite_url
            } else {
                &settings.geoip_url
            };
            let raw_url = profile.raw_config.as_ref().and_then(|raw| {
                raw.pointer(&format!("/noryGeodata/{kind}"))
                    .or_else(|| {
                        raw.get(if kind == "geosite" {
                            "Geositeurl"
                        } else {
                            "Geoipurl"
                        })
                    })
                    .and_then(Value::as_str)
            });
            let explicit = if configured.trim().is_empty() {
                raw_url
            } else {
                Some(configured.trim())
            };
            let path = if let Some(url) = explicit {
                download(url, &directory, None)?
            } else {
                installation.directory.join(format!("{kind}.dat"))
            };
            (path, tag)
        };
        if !indices.contains_key(&path) {
            indices.insert(
                path.clone(),
                categories(
                    &read_data(&path).with_context(|| format!("Не найден файл для {reference}"))?,
                )?,
            );
        }
        let tag = category
            .split('@')
            .next()
            .unwrap()
            .trim_start_matches('!')
            .to_ascii_lowercase();
        let path = if indices[&path].contains(&tag) {
            path
        } else if kind == "geosite"
            && tag == "torrent"
            && path == installation.directory.join("geosite.dat")
        {
            let supplemental = download(TORRENT_DATA, &directory, Some("torrent"))
                .context("Не удалось загрузить дополнительную категорию geosite:torrent")?;
            if !categories(&read_data(&supplemental)?)?.contains(&tag) {
                bail!("Дополнительный geosite.dat не содержит torrent");
            }
            supplemental
        } else {
            bail!(
                "Категория {reference} отсутствует в GeoData. Укажите файл провайдера в Настройки → Сеть → URL GeoSite/GeoIP. Правило не удалено"
            );
        };
        // Keep packaged databases unchanged and use explicit per-file references.
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.parent() != Some(directory.as_path()) {
            let destination = directory.join(&file);
            if !destination.exists()
                || fs::metadata(&destination)?.len() != fs::metadata(&path)?.len()
                || fs::metadata(&path)?.modified()? > fs::metadata(&destination)?.modified()?
            {
                let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
                temporary.write_all(&read_data(&path)?)?;
                temporary.persist(destination)?;
            }
        }
        mapping.insert(reference.clone(), format!("ext:{file}:{category}"));
    }
    replace(config, &mapping);
    Ok(directory)
}
