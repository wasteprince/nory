use crate::models::{
    AppData, ConnectionSpec, Profile, ProfileFormat, StreamSettings, Subscription, Transport,
    TransportSecurity,
};
use crate::share::parse_many;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use reqwest::blocking::Client;
use reqwest::blocking::RequestBuilder;
use reqwest::header::{ETAG, HeaderMap, IF_NONE_MATCH, USER_AGENT};
use serde_json::{Value, json};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAX_SUBSCRIPTION_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum FetchResult {
    NotModified,
    Updated(Box<SubscriptionUpdate>),
}

#[derive(Debug)]
pub struct SubscriptionUpdate {
    pub profiles: Vec<Profile>,
    pub source_json: Option<Value>,
    pub etag: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub website_url: Option<String>,
    pub support_url: Option<String>,
    pub update_interval_hours: Option<u16>,
    pub upload: Option<u64>,
    pub download: Option<u64>,
    pub total: Option<u64>,
    pub expire: Option<i64>,
}

pub fn new_subscription(url: &str, send_hwid: bool) -> Result<Subscription> {
    let url = normalize_url(url)?;
    let id = Uuid::new_v4();
    let name = subscription_name_from_url(&url).unwrap_or_else(|| random_subscription_name(id));
    Ok(Subscription {
        id,
        name: name.chars().take(100).collect(),
        url: url.to_string(),
        source_json: None,
        description: None,
        website_url: None,
        support_url: None,
        updated_at: None,
        expires_at: None,
        upload_bytes: None,
        download_bytes: None,
        total_bytes: None,
        send_hwid,
        etag: None,
        provider_update_interval_hours: None,
    })
}

fn subscription_name_from_url(url: &url::Url) -> Option<String> {
    url.query_pairs()
        .find(|(key, value)| {
            matches!(key.as_ref(), "name" | "title" | "remark") && !value.trim().is_empty()
        })
        .map(|(_, value)| value.trim().chars().take(100).collect())
        .or_else(|| {
            url.fragment()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(100).collect())
        })
}

fn random_subscription_name(id: Uuid) -> String {
    const ADJECTIVES: [&str; 12] = [
        "Тихая",
        "Лунная",
        "Розовая",
        "Северная",
        "Мягкая",
        "Ясная",
        "Тёплая",
        "Ночная",
        "Лёгкая",
        "Нежная",
        "Дальняя",
        "Свободная",
    ];
    const NOUNS: [&str; 12] = [
        "Комета",
        "Волна",
        "Орбита",
        "Искра",
        "Линия",
        "Звезда",
        "Сфера",
        "Лиса",
        "Река",
        "Точка",
        "Панда",
        "Стрела",
    ];
    let bytes = id.as_bytes();
    format!(
        "{} {}",
        ADJECTIVES[usize::from(bytes[0]) % ADJECTIVES.len()],
        NOUNS[usize::from(bytes[1]) % NOUNS.len()]
    )
}

pub fn fetch(
    client: &Client,
    subscription: &Subscription,
    hwid: Option<&str>,
) -> Result<FetchResult> {
    validate_url(&subscription.url)?;
    let mut request = client
        .get(&subscription.url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/plain;q=0.9",
        )
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")));
    if subscription.send_hwid
        && let Some(hwid) = hwid
    {
        request = with_device_headers(request, hwid);
    }
    if let Some(etag) = &subscription.etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let mut response = request.send().context("не удалось загрузить подписку")?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchResult::NotModified);
    }
    response = response
        .error_for_status()
        .context("сервер подписки вернул ошибку")?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_SUBSCRIPTION_BYTES)
    {
        bail!("подписка превышает 8 МБ");
    }

    let mut etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let mut headers = response.headers().clone();
    let mut bytes = Vec::new();
    response
        .take(MAX_SUBSCRIPTION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SUBSCRIPTION_BYTES {
        bail!("подписка превышает 8 МБ");
    }
    let body = String::from_utf8(bytes).context("подписка не является текстом UTF-8")?;
    let mut body = crate::share::expand_subscription_body(&body);
    merge_body_headers(&mut headers, &body);
    // Many providers return bare links to an unknown User-Agent even though
    // their explicit /json endpoint includes host descriptions and balancers.
    // Negotiate that format under our own identity; never impersonate Happ.
    if !body
        .lines()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .is_some_and(|line| line.trim_start().starts_with(['[', '{']))
        && parse_many(&body).is_ok()
    {
        if let Ok(Some((json_body, json_headers))) = fetch_json_variant(client, subscription, hwid)
        {
            body = json_body;
            for (name, value) in json_headers.iter() {
                headers.insert(name.clone(), value.clone());
            }
            merge_body_headers(&mut headers, &body);
            // The base URL's ETag must not mask changes at the JSON endpoint.
            etag = None;
        }
    }
    let title = metadata_header(&headers, "profile-title");
    let description = metadata_header(&headers, "announce")
        .or_else(|| metadata_header(&headers, "profile-description"));
    let website_url = plain_header(&headers, "profile-web-page-url");
    let support_url = plain_header(&headers, "support-url");
    let update_interval_hours = plain_header(&headers, "profile-update-interval")
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0);
    let user_info = plain_header(&headers, "subscription-userinfo")
        .map(|value| parse_user_info(&value))
        .unwrap_or_default();
    // Parse the actual NORY response. Never make a hidden second request
    // impersonating another client (or discard its headers/ETag).
    let mut profiles = parse_profiles(&body)?;
    let clean_json = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let source_json = serde_json::from_str::<Value>(&clean_json).ok();
    for profile in &mut profiles {
        profile.subscription_id = Some(subscription.id);
        if let Some(raw) = profile.raw_config.as_mut() {
            for (header, key) in [("geosite-url", "geosite"), ("geoip-url", "geoip")] {
                if let Some(url) = plain_header(&headers, header)
                    .or_else(|| plain_header(&headers, &header.replace('-', "")))
                {
                    raw["noryGeodata"][key] = Value::String(url);
                }
            }
        }
    }

    Ok(FetchResult::Updated(Box::new(SubscriptionUpdate {
        profiles,
        source_json,
        etag,
        title,
        description,
        website_url,
        support_url,
        update_interval_hours,
        upload: user_info.upload,
        download: user_info.download,
        total: user_info.total,
        expire: user_info.expire,
    })))
}

pub fn parse_profiles(body: &str) -> Result<Vec<Profile>> {
    let body = crate::share::expand_subscription_body(body);
    let clean = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if !clean.trim_start().starts_with(['[', '{']) {
        return parse_many(&body);
    }
    let document: Value = serde_json::from_str(&clean).context("некорректный JSON подписки")?;
    let items = match document {
        Value::Array(items) => items,
        Value::Object(_) => vec![document],
        _ => bail!("JSON должен содержать конфигурацию или массив конфигураций"),
    };
    let mut profiles = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        let name = json_profile_name(&item, index);
        let description = host_description(&item);
        let mut profile = profile_from_json_item(name, item)?;
        profile.description = description;
        profiles.push(profile);
    }
    if profiles.is_empty() {
        bail!("JSON подписки не содержит серверов");
    }
    Ok(merge_native_json_variants(profiles))
}

pub(crate) fn host_description(item: &Value) -> Option<String> {
    fn from_object(object: &Value) -> Option<String> {
        [
            "serverDescription",
            "server_description",
            "server-description",
            "description",
            "subtitle",
            "comment",
        ]
        .iter()
        .find_map(|key| {
            let value = object.get(key)?;
            let text = value
                .as_str()
                .or_else(|| value.get("text").and_then(Value::as_str))?;
            let text = text.trim();
            (!text.is_empty()).then(|| text.chars().take(2_000).collect())
        })
    }
    [Some(item), item.get("xray"), item.get("mihomo")]
        .into_iter()
        .flatten()
        .find_map(|node| {
            node.get("meta")
                .and_then(from_object)
                .or_else(|| node.get("metadata").and_then(from_object))
                .or_else(|| from_object(node))
        })
}

fn fetch_json_variant(
    client: &Client,
    subscription: &Subscription,
    hwid: Option<&str>,
) -> Result<Option<(String, HeaderMap)>> {
    let mut url = normalize_url(&subscription.url)?;
    let last = url
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if last.is_empty()
        || matches!(
            last,
            "json" | "mihomo" | "clash" | "singbox" | "stash" | "v2ray-json"
        )
    {
        return Ok(None);
    }
    let path = format!("{}/json", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_fragment(None);
    let mut request = client
        .get(url)
        .timeout(std::time::Duration::from_secs(8))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(USER_AGENT, format!("NORY/{}", env!("CARGO_PKG_VERSION")));
    if subscription.send_hwid
        && let Some(hwid) = hwid
    {
        request = with_device_headers(request, hwid);
    }
    let response = request.send()?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|len| len > MAX_SUBSCRIPTION_BYTES)
    {
        bail!("подписка превышает 8 МБ");
    }
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    response
        .take(MAX_SUBSCRIPTION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SUBSCRIPTION_BYTES {
        bail!("подписка превышает 8 МБ");
    }
    let body = crate::share::expand_subscription_body(&String::from_utf8(bytes)?);
    let profiles = parse_profiles(&body)?;
    if !profiles
        .iter()
        .all(|p| p.source_format == ProfileFormat::Json)
    {
        return Ok(None);
    }
    Ok(Some((body, headers)))
}

fn merge_body_headers(headers: &mut HeaderMap, body: &str) {
    for line in body.lines().take(128) {
        let Some((name, value)) = line
            .trim()
            .strip_prefix('#')
            .and_then(|v| v.split_once(':'))
        else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "profile-title"
                | "announce"
                | "profile-description"
                | "profile-web-page-url"
                | "support-url"
                | "profile-update-interval"
                | "subscription-userinfo"
                | "geosite-url"
                | "geoip-url"
                | "geositeurl"
                | "geoipurl"
        ) || headers.contains_key(&name)
        {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value.trim()),
        ) {
            headers.insert(name, value);
        }
    }
}

fn merge_native_json_variants(profiles: Vec<Profile>) -> Vec<Profile> {
    let mut merged: Vec<Profile> = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let Some(raw) = profile.raw_config.as_ref() else {
            merged.push(profile);
            continue;
        };
        let incoming_xray = crate::mihomo::native_xray_config(raw).cloned();
        let incoming_mihomo = crate::mihomo::native_mihomo_config(raw).cloned();
        if incoming_xray.is_some() && incoming_mihomo.is_some() {
            merged.push(profile);
            continue;
        }
        let pair = merged.iter().position(|existing| {
            if !existing.name.eq_ignore_ascii_case(&profile.name)
                || !existing.address.eq_ignore_ascii_case(&profile.address)
                || existing.port != profile.port
                || existing.protocol() != profile.protocol()
                || serde_json::to_value(&existing.connection).ok()
                    != serde_json::to_value(&profile.connection).ok()
                || existing.stream.network.as_xray() != profile.stream.network.as_xray()
            {
                return false;
            }
            let Some(raw) = existing.raw_config.as_ref() else {
                return false;
            };
            let existing_xray = crate::mihomo::native_xray_config(raw).is_some();
            let existing_mihomo = crate::mihomo::native_mihomo_config(raw).is_some();
            if existing_xray && existing_mihomo {
                return false;
            }
            existing_xray && incoming_mihomo.is_some() || existing_mihomo && incoming_xray.is_some()
        });
        let Some(index) = pair else {
            merged.push(profile);
            continue;
        };

        let existing_raw = merged[index]
            .raw_config
            .as_ref()
            .expect("paired JSON profile");
        let xray = crate::mihomo::native_xray_config(existing_raw)
            .cloned()
            .or(incoming_xray);
        let mihomo = crate::mihomo::native_mihomo_config(existing_raw)
            .cloned()
            .or(incoming_mihomo);
        let mut combined = if xray.is_some() {
            if crate::mihomo::native_xray_config(
                profile.raw_config.as_ref().expect("incoming JSON profile"),
            )
            .is_some()
            {
                profile
            } else {
                merged[index].clone()
            }
        } else {
            merged[index].clone()
        };
        combined.raw_config = Some(json!({
            "xray": xray.expect("paired Xray JSON"),
            "mihomo": mihomo.expect("paired Mihomo JSON")
        }));
        if combined.description.is_none() {
            combined.description = merged[index]
                .description
                .clone()
                .or_else(|| combined.raw_config.as_ref().and_then(host_description));
        }
        merged[index] = combined;
    }
    merged
}

fn json_profile_name(item: &Value, index: usize) -> String {
    item.get("remarks")
        .and_then(Value::as_str)
        .or_else(|| item.get("name").and_then(Value::as_str))
        .or_else(|| item.pointer("/xray/remarks").and_then(Value::as_str))
        .or_else(|| item.pointer("/mihomo/name").and_then(Value::as_str))
        .or_else(|| {
            item.pointer("/mihomo/proxies/0/name")
                .and_then(Value::as_str)
        })
        .or_else(|| item.pointer("/proxies/0/name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().take(160).collect())
        .unwrap_or_else(|| format!("JSON сервер {}", index + 1))
}

fn profile_from_json_item(name: String, raw: Value) -> Result<Profile> {
    let raw =
        if raw.get("type").is_some() && raw.get("server").is_some() && raw.get("port").is_some() {
            let mut proxy = raw;
            if proxy
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                proxy["name"] = json!(name);
            }
            // Keep the native proxy name, which may differ from its display
            // remarks. The runtime builder adds a comma-safe default MATCH.
            json!({
                "name": name.clone(),
                "proxies": [proxy]
            })
        } else {
            raw
        };
    if let Some(xray) = crate::mihomo::native_xray_config(&raw) {
        let mut profile = profile_from_happ_json(name, xray.clone())?;
        // Preserve a hybrid item as-is. Xray and Mihomo can then each select
        // their native section without converting it.
        profile.raw_config = Some(raw);
        return Ok(profile);
    }
    if crate::mihomo::native_mihomo_config(&raw).is_some() {
        let converted = crate::mihomo::xray_config_from_json(&raw)?;
        let mut profile = profile_from_happ_json(name, converted)?;
        // The conversion above is only used to derive list metadata. The
        // original Mihomo JSON remains the source of truth in storage.
        profile.raw_config = Some(raw);
        return Ok(profile);
    }
    bail!("JSON не содержит конфигурацию Xray или Mihomo")
}

fn profile_from_happ_json(name: String, raw: Value) -> Result<Profile> {
    let outbound = raw
        .get("outbounds")
        .and_then(Value::as_array)
        .and_then(|outbounds| {
            outbounds.iter().find(|outbound| {
                outbound
                    .get("protocol")
                    .and_then(Value::as_str)
                    .is_some_and(supported_happ_protocol)
            })
        })
        .context("в Happ JSON нет поддерживаемого proxy-outbound")?;
    let protocol = outbound
        .get("protocol")
        .and_then(Value::as_str)
        .context("в outbound отсутствует протокол")?
        .to_ascii_lowercase();
    let (address, port, connection) = match protocol.as_str() {
        "vless" | "vmess" => {
            let server = outbound
                .pointer("/settings/vnext/0")
                .context("в outbound отсутствует vnext")?;
            let user = server
                .pointer("/users/0")
                .context("в outbound отсутствует пользователь")?;
            let address = json_text(server, "address")?;
            let port = json_port(server, "port")?;
            let connection = if protocol == "vless" {
                ConnectionSpec::Vless {
                    id: json_text(user, "id")?,
                    encryption: optional_json_text(user, "encryption")
                        .unwrap_or_else(|| "none".into()),
                    flow: optional_json_text(user, "flow"),
                }
            } else {
                ConnectionSpec::Vmess {
                    id: json_text(user, "id")?,
                    alter_id: user
                        .get("alterId")
                        .and_then(json_u64)
                        .unwrap_or_default()
                        .min(u32::MAX as u64) as u32,
                    security: optional_json_text(user, "security").unwrap_or_else(|| "auto".into()),
                }
            };
            (address, port, connection)
        }
        "trojan" | "shadowsocks" | "socks" => {
            let server = outbound
                .pointer("/settings/servers/0")
                .context("в outbound отсутствует сервер")?;
            let address = json_text(server, "address")?;
            let port = json_port(server, "port")?;
            let connection = match protocol.as_str() {
                "trojan" => ConnectionSpec::Trojan {
                    password: json_text(server, "password")?,
                },
                "shadowsocks" => ConnectionSpec::Shadowsocks {
                    method: json_text(server, "method")?,
                    password: json_text(server, "password")?,
                },
                _ => {
                    let user = server.pointer("/users/0");
                    ConnectionSpec::Socks {
                        username: user.and_then(|value| optional_json_text(value, "user")),
                        password: user.and_then(|value| optional_json_text(value, "pass")),
                    }
                }
            };
            (address, port, connection)
        }
        "hysteria" | "hysteria2" | "hy2" => {
            let settings = outbound
                .get("settings")
                .context("в Hysteria2 outbound отсутствуют settings")?;
            let hysteria = outbound
                .pointer("/streamSettings/hysteriaSettings")
                .unwrap_or(&Value::Null);
            let version = settings
                .get("version")
                .and_then(json_u64)
                .or_else(|| hysteria.get("version").and_then(json_u64))
                .unwrap_or_else(|| u64::from(protocol != "hysteria") + 1);
            if version != 2 {
                bail!("поддерживается только Hysteria2, получена версия {version}");
            }
            let address = json_text(settings, "address")?;
            let port = json_port(settings, "port")?;
            let auth = optional_json_text(hysteria, "auth")
                .or_else(|| optional_json_text(settings, "auth"))
                .or_else(|| optional_json_text(settings, "password"))
                .unwrap_or_default();
            (address, port, ConnectionSpec::Hysteria2 { auth })
        }
        "http" => {
            let settings = outbound
                .get("settings")
                .context("в HTTP outbound отсутствуют settings")?;
            (
                json_text(settings, "address")?,
                json_port(settings, "port")?,
                ConnectionSpec::XrayJson {
                    name: "HTTP".into(),
                },
            )
        }
        "wireguard" => {
            let endpoint = outbound
                .pointer("/settings/peers/0/endpoint")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("в WireGuard outbound отсутствует endpoint")?;
            let endpoint = url::Url::parse(&format!("wireguard://{endpoint}"))
                .context("некорректный WireGuard endpoint")?;
            let address = endpoint
                .host_str()
                .map(ToOwned::to_owned)
                .context("в WireGuard endpoint отсутствует адрес")?;
            let port = endpoint
                .port()
                .context("в WireGuard endpoint отсутствует порт")?;
            (
                address,
                port,
                ConnectionSpec::XrayJson {
                    name: "WireGuard".into(),
                },
            )
        }
        _ => unreachable!(),
    };
    let stream = stream_from_happ_outbound(outbound);
    Ok(Profile {
        id: Uuid::new_v4(),
        name,
        description: None,
        source_format: ProfileFormat::Json,
        raw_config: Some(raw),
        address,
        port,
        connection,
        stream,
        subscription_id: None,
        favorite: false,
        latency_ms: None,
        updated_at: unix_time(),
    })
}

fn supported_happ_protocol(protocol: &str) -> bool {
    matches!(
        protocol.to_ascii_lowercase().as_str(),
        "vless"
            | "vmess"
            | "trojan"
            | "shadowsocks"
            | "socks"
            | "hysteria"
            | "hysteria2"
            | "hy2"
            | "http"
            | "wireguard"
    )
}

fn stream_from_happ_outbound(outbound: &Value) -> StreamSettings {
    let stream = outbound.get("streamSettings").unwrap_or(&Value::Null);
    let security = optional_json_text(stream, "security").unwrap_or_default();
    let tls = stream.get("tlsSettings").unwrap_or(&Value::Null);
    let reality = stream.get("realitySettings").unwrap_or(&Value::Null);
    let network = optional_json_text(stream, "network").unwrap_or_default();
    let transport = Transport::from_share(&network);
    let transport_settings = match transport {
        Transport::Websocket => stream.get("wsSettings"),
        Transport::Grpc => stream.get("grpcSettings"),
        Transport::Xhttp => stream.get("xhttpSettings"),
        Transport::Httpupgrade => stream.get("httpupgradeSettings"),
        Transport::Mkcp => stream.get("kcpSettings"),
        _ => stream
            .get("rawSettings")
            .or_else(|| stream.get("tcpSettings")),
    }
    .unwrap_or(&Value::Null);
    let alpn = tls
        .get("alpn")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    StreamSettings {
        network: transport,
        security: TransportSecurity::from_share(&security),
        server_name: optional_json_text(reality, "serverName")
            .or_else(|| optional_json_text(tls, "serverName")),
        fingerprint: optional_json_text(reality, "fingerprint")
            .or_else(|| optional_json_text(tls, "fingerprint")),
        alpn,
        public_key: optional_json_text(reality, "publicKey")
            .or_else(|| optional_json_text(reality, "password")),
        short_id: optional_json_text(reality, "shortId"),
        spider_x: optional_json_text(reality, "spiderX"),
        path: optional_json_text(transport_settings, "path"),
        host: optional_json_text(transport_settings, "host").or_else(|| {
            transport_settings
                .pointer("/headers/Host")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        }),
        service_name: optional_json_text(transport_settings, "serviceName"),
        mode: optional_json_text(transport_settings, "mode"),
        header_type: transport_settings
            .pointer("/header/type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        packet_encoding: optional_json_text(outbound, "packetEncoding"),
    }
}

fn json_text(value: &Value, name: &str) -> Result<String> {
    optional_json_text(value, name).with_context(|| format!("в JSON отсутствует {name}"))
}

fn optional_json_text(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_port(value: &Value, name: &str) -> Result<u16> {
    value
        .get(name)
        .and_then(json_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .with_context(|| format!("в JSON отсутствует корректный {name}"))
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn with_device_headers(request: RequestBuilder, hwid: &str) -> RequestBuilder {
    let (os, version, model) = device_identity();
    request
        .header("X-HWID", hwid)
        .header("X-Device-OS", &os)
        .header("X-Ver-OS", &version)
        .header("X-Device-Model", &model)
        .header("Device-OS", &os)
        .header("Device-Model", &model)
        .header(
            "X-Device-Info",
            format!(
                "{os}; {version}; {model}; NORY/{}",
                env!("CARGO_PKG_VERSION")
            ),
        )
}

#[cfg(target_os = "linux")]
fn device_identity() -> (String, String, String) {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|value| value.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into());
    let version =
        std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_else(|_| "Linux".into());
    let hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_else(|_| "Linux PC".into());
    let product = std::fs::read_to_string("/sys/class/dmi/id/product_name").unwrap_or_default();
    let model = if product.trim().is_empty() {
        hostname
    } else {
        format!("{} · {}", hostname.trim(), product.trim())
    };
    (
        header_text(&os, "Linux"),
        header_text(&version, "Linux"),
        header_text(&model, "Linux PC"),
    )
}

#[cfg(target_os = "windows")]
fn device_identity() -> (String, String, String) {
    let version = crate::windows::os_version_description();
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into());
    let processor = std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "x64 device".into());
    (
        "Windows 11".into(),
        header_text(&version, "Windows 11"),
        header_text(&format!("{hostname} · {processor}"), "Windows PC"),
    )
}

fn header_text(value: &str, fallback: &str) -> String {
    let clean = value
        .trim()
        .chars()
        .filter(|character| character.is_ascii() && !character.is_ascii_control())
        .take(120)
        .collect::<String>();
    if clean.is_empty() {
        fallback.into()
    } else {
        clean
    }
}

pub fn apply_url_change(
    data: &mut AppData,
    old: &Subscription,
    replacement: Subscription,
    result: FetchResult,
) -> Result<()> {
    let index = data
        .subscriptions
        .iter()
        .position(|s| s.id == old.id)
        .context("Подписка удалена")?;
    if data.subscriptions[index].url != old.url {
        bail!("Ссылка уже изменена. Откройте редактирование ещё раз");
    }
    if data
        .subscriptions
        .iter()
        .any(|s| s.id != old.id && s.url == replacement.url)
    {
        bail!("Эта ссылка уже используется другой подпиской");
    }
    if replacement.id != old.id || matches!(result, FetchResult::NotModified) {
        bail!("Не получены данные новой подписки");
    }
    data.subscriptions[index] = replacement;
    apply_fetch(data, old.id, result)?;
    Ok(())
}

pub fn apply_fetch(data: &mut AppData, id: Uuid, result: FetchResult) -> Result<usize> {
    let subscription = data
        .subscriptions
        .iter_mut()
        .find(|item| item.id == id)
        .context("подписка не найдена")?;
    match result {
        FetchResult::NotModified => {
            subscription.updated_at = Some(unix_time());
            Ok(data
                .profiles
                .iter()
                .filter(|profile| profile.subscription_id == Some(id))
                .count())
        }
        FetchResult::Updated(update) => {
            let SubscriptionUpdate {
                mut profiles,
                source_json,
                etag,
                title,
                description,
                website_url,
                support_url,
                update_interval_hours,
                upload,
                download,
                total,
                expire,
            } = *update;
            // One-to-one matching; hash each JSON only once, not for every
            // pair in a subscription. Duplicate rows keep distinct identities.
            let mut old: std::collections::HashMap<
                (String, String),
                std::collections::VecDeque<Profile>,
            > = std::collections::HashMap::new();
            for profile in data
                .profiles
                .iter()
                .filter(|p| p.subscription_id == Some(id))
            {
                old.entry((profile.name.clone(), profile.endpoint_key()))
                    .or_default()
                    .push_back(profile.clone());
            }
            let mut used_ids: std::collections::HashSet<Uuid> = data
                .profiles
                .iter()
                .filter(|p| p.subscription_id != Some(id))
                .map(|p| p.id)
                .collect();
            for profile in &mut profiles {
                profile.subscription_id = Some(id);
                if let Some(previous) = old
                    .get_mut(&(profile.name.clone(), profile.endpoint_key()))
                    .and_then(std::collections::VecDeque::pop_front)
                {
                    profile.id = previous.id;
                    profile.favorite = previous.favorite;
                    profile.latency_ms = previous.latency_ms;
                }
                while !used_ids.insert(profile.id) {
                    profile.id = Uuid::new_v4();
                }
            }
            let insert_at = data
                .profiles
                .iter()
                .position(|profile| profile.subscription_id == Some(id))
                .unwrap_or(data.profiles.len());
            data.profiles
                .retain(|profile| profile.subscription_id != Some(id));
            let count = profiles.len();
            data.profiles.splice(insert_at..insert_at, profiles);
            if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
                subscription.name = title.trim().chars().take(100).collect();
            }
            subscription.description = description;
            subscription.source_json = source_json;
            subscription.website_url = website_url;
            subscription.support_url = support_url;
            subscription.provider_update_interval_hours = update_interval_hours;
            subscription.updated_at = Some(unix_time());
            subscription.etag = etag;
            subscription.upload_bytes = upload;
            subscription.download_bytes = download;
            subscription.total_bytes = total;
            subscription.expires_at = expire;
            if data
                .selected_profile
                .is_some_and(|selected| !data.profiles.iter().any(|profile| profile.id == selected))
            {
                data.selected_profile = data
                    .selected_subscription
                    .and_then(|subscription| {
                        data.profiles
                            .iter()
                            .find(|profile| profile.subscription_id == Some(subscription))
                    })
                    .or_else(|| data.profiles.first())
                    .map(|profile| profile.id);
            }
            Ok(count)
        }
    }
}

fn plain_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| std::str::from_utf8(value.as_bytes()).ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(2_000).collect())
}

fn metadata_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = plain_header(headers, name)?;
    let Some(encoded) = value.strip_prefix("base64:") else {
        return Some(value);
    };
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .into_iter()
        .find_map(|engine| engine.decode(encoded).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|decoded| decoded.trim().chars().take(2_000).collect())
        .filter(|decoded: &String| !decoded.is_empty())
}

#[derive(Debug, Default)]
struct UserInfo {
    upload: Option<u64>,
    download: Option<u64>,
    total: Option<u64>,
    expire: Option<i64>,
}

fn parse_user_info(raw: &str) -> UserInfo {
    let mut info = UserInfo::default();
    for part in raw.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => info.upload = value.trim().parse().ok(),
            "download" => info.download = value.trim().parse().ok(),
            "total" => info.total = value.trim().parse().ok(),
            "expire" => info.expire = value.trim().parse().ok(),
            _ => {}
        }
    }
    info
}

fn validate_url(raw: &str) -> Result<()> {
    normalize_url(raw).map(|_| ())
}

fn normalize_url(raw: &str) -> Result<url::Url> {
    let raw = raw.trim();
    let raw = raw
        .strip_prefix('[')
        .and_then(|value| value.split_once("]("))
        .and_then(|(_, target)| target.strip_suffix(')'))
        .unwrap_or(raw);
    let url = url::Url::parse(raw).context("некорректный URL подписки")?;
    if !matches!(url.scheme(), "https" | "http") {
        bail!("подписка должна использовать HTTP или HTTPS");
    }
    if url.host_str().is_none() {
        bail!("в URL подписки отсутствует адрес");
    }
    Ok(url)
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
