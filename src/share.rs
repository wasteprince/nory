use crate::models::{ConnectionSpec, Profile, StreamSettings, Transport, TransportSecurity};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use percent_encoding::percent_decode_str;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;
use uuid::Uuid;

const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;

pub fn parse_many(input: &str) -> Result<Vec<Profile>> {
    if input.len() > MAX_IMPORT_BYTES {
        bail!("данные импорта превышают 8 МБ");
    }
    let expanded = expand_subscription_body(input);
    let mut profiles = Vec::new();
    let mut failures = Vec::new();

    for line in expanded
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        match parse_link(line) {
            Ok(profile) => profiles.push(profile),
            Err(error) if looks_like_link(line) => failures.push(error.to_string()),
            Err(_) => {}
        }
    }

    if profiles.is_empty() {
        if let Some(error) = failures.first() {
            bail!("не удалось распознать сервер: {error}");
        }
        bail!("поддерживаемые ссылки не найдены");
    }
    Ok(profiles)
}

pub fn parse_link(link: &str) -> Result<Profile> {
    let mut profile = parse_link_inner(link)?;
    if let Some(encoded) = link.trim().strip_prefix("vmess://") {
        if let Ok(bytes) = decode_base64(encoded.trim())
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            profile.description = crate::subscription::host_description(&value);
        }
    } else if let Ok(url) = Url::parse(link.trim()) {
        profile.description = url.query_pairs().find_map(|(key, value)| {
            (matches!(
                key.as_ref(),
                "description"
                    | "serverDescription"
                    | "server_description"
                    | "server-description"
                    | "subtitle"
                    | "comment"
            ) && !value.trim().is_empty())
            .then(|| value.trim().chars().take(2_000).collect())
        });
    }
    Ok(profile)
}

fn parse_link_inner(link: &str) -> Result<Profile> {
    let link = link.trim();
    if link.starts_with("vless://") {
        return parse_vless(link);
    }
    if link.starts_with("vmess://") {
        return parse_vmess(link);
    }
    if link.starts_with("trojan://") {
        return parse_trojan(link);
    }
    if link.starts_with("ss://") {
        return parse_shadowsocks(link);
    }
    if link.starts_with("socks://") || link.starts_with("socks5://") {
        return parse_socks(link);
    }
    if link.starts_with("hysteria2://") || link.starts_with("hy2://") {
        return parse_hysteria2(link);
    }
    bail!("неподдерживаемый формат ссылки")
}

fn parse_vless(link: &str) -> Result<Profile> {
    let url = Url::parse(link).context("некорректная VLESS-ссылка")?;
    let id = decode(url.username());
    if id.is_empty() {
        bail!("в VLESS-ссылке отсутствует UUID");
    }
    let (address, port) = endpoint(&url, 443)?;
    let query = query_map(&url);
    let stream = stream_from_query(&query);
    Ok(profile(
        display_name(&url, &address),
        address,
        port,
        ConnectionSpec::Vless {
            id,
            encryption: query
                .get("encryption")
                .cloned()
                .unwrap_or_else(|| "none".into()),
            flow: query.get("flow").cloned().and_then(nonempty),
        },
        stream,
    ))
}

fn parse_trojan(link: &str) -> Result<Profile> {
    let url = Url::parse(link).context("некорректная Trojan-ссылка")?;
    let password = decode(url.username());
    if password.is_empty() {
        bail!("в Trojan-ссылке отсутствует пароль");
    }
    let (address, port) = endpoint(&url, 443)?;
    let query = query_map(&url);
    let mut stream = stream_from_query(&query);
    if !query.contains_key("security") {
        stream.security = TransportSecurity::Tls;
    }
    Ok(profile(
        display_name(&url, &address),
        address,
        port,
        ConnectionSpec::Trojan { password },
        stream,
    ))
}

fn parse_vmess(link: &str) -> Result<Profile> {
    let encoded = link
        .strip_prefix("vmess://")
        .context("некорректная VMess-ссылка")?;
    let bytes = decode_base64(encoded.trim()).context("VMess не является корректным Base64")?;
    let value: Value =
        serde_json::from_slice(&bytes).context("в VMess находится некорректный JSON")?;
    let text = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let number = |name: &str| -> u32 {
        value
            .get(name)
            .and_then(|item| {
                item.as_u64()
                    .map(|n| n as u32)
                    .or_else(|| item.as_str()?.parse().ok())
            })
            .unwrap_or(0)
    };
    let address = text("add");
    if address.is_empty() {
        bail!("в VMess отсутствует адрес сервера");
    }
    let port = value
        .get("port")
        .and_then(|item| {
            item.as_u64()
                .map(|n| n as u16)
                .or_else(|| item.as_str()?.parse().ok())
        })
        .context("в VMess отсутствует порт")?;
    let security_value = text("tls");
    let security_value = if security_value.is_empty() {
        text("security")
    } else {
        security_value
    };
    let stream = StreamSettings {
        network: Transport::from_share(&text("net")),
        security: TransportSecurity::from_share(&security_value),
        server_name: nonempty(text("sni")).or_else(|| nonempty(text("host"))),
        fingerprint: nonempty(text("fp")),
        alpn: split_csv(&text("alpn")),
        public_key: nonempty(text("pbk")),
        short_id: nonempty(text("sid")),
        spider_x: nonempty(text("spx")),
        path: nonempty(text("path")),
        host: nonempty(text("host")),
        service_name: nonempty(text("path")),
        mode: nonempty(text("mode")),
        header_type: nonempty(text("type")),
        packet_encoding: nonempty(text("packetEncoding")),
    };
    Ok(profile(
        nonempty(text("ps")).unwrap_or_else(|| address.clone()),
        address,
        port,
        ConnectionSpec::Vmess {
            id: text("id"),
            alter_id: number("aid"),
            security: nonempty(text("scy")).unwrap_or_else(|| "auto".into()),
        },
        stream,
    ))
}

fn parse_shadowsocks(link: &str) -> Result<Profile> {
    let raw = link
        .strip_prefix("ss://")
        .context("некорректная Shadowsocks-ссылка")?;
    let (without_fragment, fragment) = raw
        .split_once('#')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(a, _)| a);
    let decoded_whole = decode_base64(without_query)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let authority = decoded_whole.as_deref().unwrap_or(without_query);
    let (credentials, host_port) = authority
        .rsplit_once('@')
        .context("в Shadowsocks отсутствует адрес или пароль")?;
    let credentials = decode_base64(credentials)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| decode(credentials));
    let (method, password) = credentials
        .split_once(':')
        .context("в Shadowsocks отсутствует метод шифрования")?;
    let endpoint_url =
        Url::parse(&format!("http://{host_port}")).context("некорректный адрес Shadowsocks")?;
    let (address, port) = endpoint(&endpoint_url, 8388)?;
    let name = fragment
        .map(decode)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| address.clone());
    Ok(profile(
        name,
        address,
        port,
        ConnectionSpec::Shadowsocks {
            method: method.to_string(),
            password: password.to_string(),
        },
        StreamSettings::default(),
    ))
}

fn parse_socks(link: &str) -> Result<Profile> {
    let normalized = link.replacen("socks5://", "socks://", 1);
    let url = Url::parse(&normalized).context("некорректная SOCKS-ссылка")?;
    let (address, port) = endpoint(&url, 1080)?;
    let username = nonempty(decode(url.username()));
    let password = url.password().map(decode).and_then(nonempty);
    Ok(profile(
        display_name(&url, &address),
        address,
        port,
        ConnectionSpec::Socks { username, password },
        StreamSettings::default(),
    ))
}

fn parse_hysteria2(link: &str) -> Result<Profile> {
    let normalized = link.replacen("hy2://", "hysteria2://", 1);
    let url = Url::parse(&normalized).context("некорректная Hysteria2-ссылка")?;
    let auth = decode(url.username());
    if auth.is_empty() {
        bail!("в Hysteria2-ссылке отсутствует пароль");
    }
    let (address, port) = endpoint(&url, 443)?;
    let query = query_map(&url);
    let mut stream = stream_from_query(&query);
    stream.network = Transport::Hysteria;
    stream.security = TransportSecurity::Tls;
    if stream.server_name.is_none() {
        stream.server_name = Some(address.clone());
    }
    if stream.alpn.is_empty() {
        stream.alpn.push("h3".into());
    }
    Ok(profile(
        display_name(&url, &address),
        address,
        port,
        ConnectionSpec::Hysteria2 { auth },
        stream,
    ))
}

fn stream_from_query(query: &HashMap<String, String>) -> StreamSettings {
    let network = query
        .get("type")
        .or_else(|| query.get("net"))
        .map(String::as_str)
        .unwrap_or("raw");
    StreamSettings {
        network: Transport::from_share(network),
        security: TransportSecurity::from_share(
            query.get("security").map(String::as_str).unwrap_or("none"),
        ),
        server_name: query
            .get("sni")
            .or_else(|| query.get("serverName"))
            .cloned()
            .and_then(nonempty),
        fingerprint: query.get("fp").cloned().and_then(nonempty),
        alpn: query
            .get("alpn")
            .map(|value| split_csv(value))
            .unwrap_or_default(),
        public_key: query.get("pbk").cloned().and_then(nonempty),
        short_id: query.get("sid").cloned().and_then(nonempty),
        spider_x: query.get("spx").cloned().and_then(nonempty),
        path: query.get("path").cloned().and_then(nonempty),
        host: query.get("host").cloned().and_then(nonempty),
        service_name: query
            .get("serviceName")
            .or_else(|| query.get("service_name"))
            .cloned()
            .and_then(nonempty),
        mode: query.get("mode").cloned().and_then(nonempty),
        header_type: query
            .get("headerType")
            .or_else(|| query.get("header_type"))
            .cloned()
            .and_then(nonempty),
        packet_encoding: query
            .get("packetEncoding")
            .or_else(|| query.get("packet_encoding"))
            .cloned()
            .and_then(nonempty),
    }
}

fn endpoint(url: &Url, default_port: u16) -> Result<(String, u16)> {
    let host = url
        .host_str()
        .context("отсутствует адрес сервера")?
        .trim()
        .to_string();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        bail!("некорректный адрес сервера");
    }
    Ok((host, url.port().unwrap_or(default_port)))
}

fn profile(
    name: String,
    address: String,
    port: u16,
    connection: ConnectionSpec,
    stream: StreamSettings,
) -> Profile {
    Profile {
        id: Uuid::new_v4(),
        name: if name.trim().is_empty() {
            address.clone()
        } else {
            name.trim().chars().take(160).collect()
        },
        description: None,
        source_format: crate::models::ProfileFormat::Link,
        raw_config: None,
        address,
        port,
        connection,
        stream,
        subscription_id: None,
        favorite: false,
        latency_ms: None,
        updated_at: unix_time(),
    }
}

fn display_name(url: &Url, fallback: &str) -> String {
    url.fragment()
        .map(decode)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn query_map(url: &Url) -> HashMap<String, String> {
    url.query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

pub(crate) fn expand_subscription_body(input: &str) -> String {
    let trimmed = input.trim().trim_start_matches('\u{feff}');
    if looks_like_link(trimmed) || trimmed.lines().any(looks_like_link) {
        return trimmed.to_string();
    }
    decode_base64(trimmed)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| trimmed.to_string())
}

fn looks_like_link(value: &str) -> bool {
    [
        "vless://",
        "vmess://",
        "trojan://",
        "ss://",
        "socks://",
        "socks5://",
        "hysteria2://",
        "hy2://",
    ]
    .iter()
    .any(|prefix| value.trim().starts_with(prefix))
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    let compact: String = value.chars().filter(|ch| !ch.is_whitespace()).collect();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&compact) {
            return Ok(bytes);
        }
    }
    Err(anyhow!("некорректный Base64"))
}

fn decode(value: &str) -> String {
    percent_decode_str(value).decode_utf8_lossy().into_owned()
}

fn split_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn nonempty<T: AsRef<str>>(value: T) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
