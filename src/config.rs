use crate::models::{
    ConnectionMode, ConnectionSpec, GeoDataKind, Profile, Settings, Transport, TransportSecurity,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Export one saved server, without the subscription or runtime TUN settings.
/// Native JSON may contain several outbounds and a balancer: keep them together.
pub fn profile_json(profile: &Profile) -> Result<Value> {
    if let Some(raw) = &profile.raw_config {
        return Ok(raw.clone());
    }
    let mut proxy = outbound(profile, &Settings::default())?;
    // This routing mark is injected by NORY at runtime, not supplied by the link.
    if let Some(stream) = proxy
        .get_mut("streamSettings")
        .and_then(Value::as_object_mut)
    {
        stream.remove("sockopt");
    }
    Ok(json!({ "remarks": profile.name, "outbounds": [proxy] }))
}

pub fn build_xray_config(profile: &Profile, settings: &Settings) -> Result<Value> {
    validate_ports(settings)?;
    if let Some(raw) = &profile.raw_config {
        let xray = crate::mihomo::xray_config_from_json(raw)?;
        return build_happ_xray_config(profile, settings, &xray);
    }
    let inbounds = vec![json!({
        "tag": "proxy-in",
        "listen": "127.0.0.1",
        "port": settings.socks_port,
        "protocol": "socks",
        "settings": { "auth": "noauth", "udp": true },
        "sniffing": sniffing(settings)
    })];

    let mut rules = vec![json!({ "type": "field", "inboundTag": ["api"], "outboundTag": "api" })];
    let routing = &settings.routing;
    // Process bypass is enforced by the privileged TUN backend. Xray only
    // receives traffic that must enter the tunnel.
    let direct_domains = routing
        .bypass_domains
        .iter()
        .map(|domain| domain_matcher(domain))
        .collect::<Result<Vec<_>>>()?;
    if !direct_domains.is_empty() {
        rules.push(json!({
            "type": "field",
            "domain": direct_domains,
            "outboundTag": "direct",
            "ruleTag": "domain-bypass-direct"
        }));
    }
    let mut geosite = Vec::new();
    let mut geoip = Vec::new();
    if routing.bypass_ru {
        geosite.push("geosite:category-ru".to_string());
        geoip.push("geoip:ru".to_string());
    }
    for rule in &routing.bypass_geodata {
        validate_geodata_file_name(&rule.file_name)?;
        for tag in &rule.tags {
            validate_geodata_tag(tag)?;
            let matcher = format!("ext:{}:{}", rule.file_name, tag);
            match rule.kind {
                GeoDataKind::GeoSite => geosite.push(matcher),
                GeoDataKind::GeoIp => geoip.push(matcher),
            }
        }
    }
    if !geosite.is_empty() {
        rules.push(json!({
            "type": "field",
            "domain": geosite,
            "outboundTag": "direct",
            "ruleTag": "geosite-bypass-direct"
        }));
    }
    if !geoip.is_empty() {
        rules.push(json!({
            "type": "field",
            "ip": geoip,
            "outboundTag": "direct",
            "ruleTag": "geoip-bypass-direct"
        }));
    }
    let mut config = json!({
        "log": { "loglevel": settings.log_level.as_xray() },
        "api": { "tag": "api", "listen": format!("127.0.0.1:{}", settings.api_port), "services": ["StatsService"] },
        "stats": {},
        "policy": { "system": { "statsInboundUplink": true, "statsInboundDownlink": true } },
        "inbounds": inbounds,
        "outbounds": [
            outbound(profile, settings)?,
            direct_outbound(),
            { "tag": "block", "protocol": "blackhole", "settings": {} }
        ],
        // Resolve at the GeoIP rule, before a provider's catch-all can match.
        "routing": { "domainStrategy": if routing.bypass_ru { "IPOnDemand" } else { settings.domain_strategy.as_xray() }, "rules": rules },
        "remarks": { "noryInbound": "proxy-in" }
    });
    let dns_servers = settings
        .dns_servers
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .collect::<Vec<_>>();
    if !dns_servers.is_empty() {
        config
            .as_object_mut()
            .expect("config object")
            .insert("dns".into(), json!({ "servers": dns_servers }));
    }
    Ok(config)
}

fn domain_matcher(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 || value.contains(['\0', '\r', '\n']) {
        bail!("некорректный домен в правилах обхода");
    }
    if ["domain:", "full:", "keyword:", "regexp:"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
    {
        if value
            .split_once(':')
            .is_none_or(|(_, body)| body.is_empty())
        {
            bail!("пустое правило домена: {value}");
        }
        return Ok(value.to_string());
    }
    if value.contains(['/', '\\', ':']) || value.chars().any(char::is_whitespace) {
        bail!("некорректный домен: {value}");
    }
    let value = value
        .strip_prefix("*.")
        .or_else(|| value.strip_prefix('.'))
        .unwrap_or(value);
    if value.is_empty() {
        bail!("пустое правило домена");
    }
    Ok(format!("domain:{value}"))
}

fn validate_geodata_file_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value.contains(['/', '\\', ':', '\0'])
        || !value.to_ascii_lowercase().ends_with(".dat")
    {
        bail!("некорректное имя файла GeoData: {value}");
    }
    Ok(())
}

fn validate_geodata_tag(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.@!".contains(&byte))
    {
        bail!("некорректный тег GeoData: {value}");
    }
    Ok(())
}

fn build_happ_xray_config(profile: &Profile, settings: &Settings, raw: &Value) -> Result<Value> {
    let raw_object = raw
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Happ JSON должен быть объектом"))?;
    let raw_outbounds = raw_object
        .get("outbounds")
        .and_then(Value::as_array)
        .filter(|outbounds| !outbounds.is_empty())
        .ok_or_else(|| anyhow::anyhow!("в Happ JSON отсутствуют outbounds"))?;

    let mut representative = profile.clone();
    representative.raw_config = None;
    // Generic JSON-only protocols are kept byte-for-byte below. Use a harmless
    // placeholder solely to create NORY's inbounds/routing skeleton first.
    if matches!(representative.connection, ConnectionSpec::XrayJson { .. }) {
        representative.address = "127.0.0.1".into();
        representative.port = 9;
        representative.connection = ConnectionSpec::Socks {
            username: None,
            password: None,
        };
    }
    let mut config = build_xray_config(&representative, settings)?;
    let object = config.as_object_mut().expect("generated config object");
    let generated_outbounds = object
        .get("outbounds")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut outbounds = raw_outbounds.clone();
    for required_tag in ["direct", "block"] {
        if outbound_with_tag(&outbounds, required_tag).is_none()
            && let Some(fallback) = outbound_with_tag(&generated_outbounds, required_tag)
        {
            outbounds.push(fallback.clone());
        }
    }
    #[cfg(target_os = "linux")]
    if settings.mode == ConnectionMode::Tun {
        for outbound in &mut outbounds {
            add_linux_tun_mark(outbound);
        }
    }
    object.insert("outbounds".into(), Value::Array(outbounds));

    for key in [
        "dns",
        "fakedns",
        "observatory",
        "burstObservatory",
        "reverse",
    ] {
        if let Some(value) = raw_object.get(key) {
            object.insert(key.into(), value.clone());
        }
    }

    if let Some(raw_policy) = raw_object.get("policy").and_then(Value::as_object) {
        let policy = object
            .entry("policy")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("generated policy object");
        for (key, value) in raw_policy {
            if key != "system" {
                policy.insert(key.clone(), value.clone());
            }
        }
    }

    if let Some(raw_routing) = raw_object.get("routing").and_then(Value::as_object) {
        let mut routing = raw_routing.clone();
        let mut rules = object
            .get("routing")
            .and_then(|routing| routing.get("rules"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let provider_inbounds = provider_inbound_tags(raw_object);
        if let Some(provider_rules) = raw_routing.get("rules").and_then(Value::as_array) {
            rules.extend(
                provider_rules
                    .iter()
                    .map(|rule| remap_inbound_tags(rule, &provider_inbounds)),
            );
        }
        routing.insert("rules".into(), Value::Array(rules));
        if settings.routing.bypass_ru {
            routing.insert("domainStrategy".into(), json!("IPOnDemand"));
        }
        object.insert("routing".into(), Value::Object(routing));
    }
    object.insert(
        "remarks".into(),
        json!({ "noryInbound": "proxy-in", "profile": profile.name }),
    );
    Ok(config)
}

fn outbound_with_tag<'a>(outbounds: &'a [Value], tag: &str) -> Option<&'a Value> {
    outbounds
        .iter()
        .find(|outbound| outbound.get("tag").and_then(Value::as_str) == Some(tag))
}

fn provider_inbound_tags(raw: &serde_json::Map<String, Value>) -> HashSet<String> {
    raw.get("inbounds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|inbound| inbound.get("tag").and_then(Value::as_str))
        .filter(|tag| *tag != "api")
        .map(ToOwned::to_owned)
        .collect()
}

fn remap_inbound_tags(rule: &Value, provider_inbounds: &HashSet<String>) -> Value {
    if provider_inbounds.is_empty() {
        return rule.clone();
    }
    let mut rule = rule.clone();
    let Some(object) = rule.as_object_mut() else {
        return rule;
    };
    let Some(tags) = object.get_mut("inboundTag") else {
        return rule;
    };
    match tags {
        Value::String(tag) if provider_inbounds.contains(tag) => {
            *tag = "proxy-in".into();
        }
        Value::Array(values) => {
            let mut remapped = Vec::with_capacity(values.len());
            let mut seen = HashSet::new();
            for value in values.iter() {
                let Some(tag) = value.as_str() else {
                    remapped.push(value.clone());
                    continue;
                };
                let tag = if provider_inbounds.contains(tag) {
                    "proxy-in"
                } else {
                    tag
                };
                if seen.insert(tag.to_string()) {
                    remapped.push(Value::String(tag.to_string()));
                }
            }
            *values = remapped;
        }
        _ => {}
    }
    rule
}

#[cfg(target_os = "linux")]
fn add_linux_tun_mark(outbound: &mut Value) {
    if outbound.get("protocol").and_then(Value::as_str) == Some("blackhole") {
        return;
    }
    let Some(object) = outbound.as_object_mut() else {
        return;
    };
    let stream = object
        .entry("streamSettings")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    let Some(stream) = stream else {
        return;
    };
    let sockopt = stream
        .entry("sockopt")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(sockopt) = sockopt {
        sockopt.insert("mark".into(), json!(20220));
    }
}

fn outbound(profile: &Profile, settings: &Settings) -> Result<Value> {
    if profile.address.trim().is_empty() || profile.port == 0 {
        bail!("некорректный адрес сервера");
    }
    let connection_settings = match &profile.connection {
        ConnectionSpec::Vless {
            id,
            encryption,
            flow,
        } => json!({
            "vnext": [{ "address": profile.address, "port": profile.port, "users": [{
                "id": id, "encryption": encryption, "flow": flow.as_deref().unwrap_or("")
            }] }]
        }),
        ConnectionSpec::Vmess {
            id,
            alter_id,
            security,
        } => json!({
            "vnext": [{ "address": profile.address, "port": profile.port, "users": [{
                "id": id, "alterId": alter_id, "security": security
            }] }]
        }),
        ConnectionSpec::Trojan { password } => json!({
            "servers": [{ "address": profile.address, "port": profile.port, "password": password }]
        }),
        ConnectionSpec::Shadowsocks { method, password } => json!({
            "servers": [{ "address": profile.address, "port": profile.port, "method": method, "password": password }]
        }),
        ConnectionSpec::Socks { username, password } => {
            let users = username
                .as_ref()
                .map(|user| json!([{ "user": user, "pass": password.as_deref().unwrap_or("") }]))
                .unwrap_or_else(|| json!([]));
            json!({ "servers": [{ "address": profile.address, "port": profile.port, "users": users }] })
        }
        ConnectionSpec::Hysteria2 { .. } => json!({
            "version": 2,
            "address": profile.address,
            "port": profile.port
        }),
        ConnectionSpec::XrayJson { name } => {
            bail!("протокол {name} доступен только из исходной JSON-конфигурации")
        }
    };
    Ok(json!({
        "tag": "proxy",
        "protocol": match &profile.connection {
            ConnectionSpec::Vless { .. } => "vless",
            ConnectionSpec::Vmess { .. } => "vmess",
            ConnectionSpec::Trojan { .. } => "trojan",
            ConnectionSpec::Shadowsocks { .. } => "shadowsocks",
            ConnectionSpec::Socks { .. } => "socks",
            ConnectionSpec::Hysteria2 { .. } => "hysteria",
            ConnectionSpec::XrayJson { name } => name.as_str(),
        },
        "settings": connection_settings,
        "streamSettings": stream_settings(profile, settings.tls_allow_insecure),
        "mux": {
            "enabled": settings.mux_enabled,
            "concurrency": settings.mux_concurrency
        }
    }))
}

fn stream_settings(profile: &Profile, tls_allow_insecure: bool) -> Value {
    let stream = &profile.stream;
    let mut value = json!({
        "network": transport_name(stream.network),
        "security": stream.security.as_xray()
    });
    let object = value.as_object_mut().expect("object");
    #[cfg(target_os = "linux")]
    object.insert("sockopt".into(), json!({ "mark": 20220 }));
    match stream.security {
        TransportSecurity::Tls => {
            object.insert(
                "tlsSettings".into(),
                json!({
                    "serverName": stream.server_name.as_deref().unwrap_or(&profile.address),
                    "fingerprint": stream.fingerprint.as_deref().unwrap_or("chrome"),
                    "alpn": stream.alpn,
                    "allowInsecure": tls_allow_insecure
                }),
            );
        }
        TransportSecurity::Reality => {
            object.insert(
                "realitySettings".into(),
                json!({
                    "show": false,
                    "fingerprint": stream.fingerprint.as_deref().unwrap_or("chrome"),
                    "serverName": stream.server_name.as_deref().unwrap_or(&profile.address),
                    "password": stream.public_key,
                    "shortId": stream.short_id.as_deref().unwrap_or(""),
                    "spiderX": stream.spider_x.as_deref().unwrap_or("")
                }),
            );
        }
        TransportSecurity::None => {}
    }
    match stream.network {
        Transport::Websocket => {
            object.insert("wsSettings".into(), json!({ "path": stream.path.as_deref().unwrap_or("/"), "headers": { "Host": stream.host.as_deref().unwrap_or("") } }));
        }
        Transport::Grpc => {
            object.insert("grpcSettings".into(), json!({ "serviceName": stream.service_name.as_deref().unwrap_or(""), "multiMode": stream.mode.as_deref() == Some("multi") }));
        }
        Transport::Xhttp => {
            object.insert("xhttpSettings".into(), json!({ "path": stream.path.as_deref().unwrap_or("/"), "host": stream.host.as_deref().unwrap_or(""), "mode": stream.mode }));
        }
        Transport::Httpupgrade => {
            object.insert("httpupgradeSettings".into(), json!({ "path": stream.path.as_deref().unwrap_or("/"), "host": stream.host.as_deref().unwrap_or("") }));
        }
        Transport::Mkcp => {
            object.insert(
                "kcpSettings".into(),
                json!({ "header": { "type": stream.header_type.as_deref().unwrap_or("none") } }),
            );
        }
        Transport::Hysteria => {
            let auth = match &profile.connection {
                ConnectionSpec::Hysteria2 { auth } => auth.as_str(),
                _ => "",
            };
            object.insert(
                "hysteriaSettings".into(),
                json!({ "version": 2, "auth": auth }),
            );
        }
        Transport::Raw => {
            if let Some(header) = &stream.header_type
                && header != "none"
            {
                object.insert(
                    "rawSettings".into(),
                    json!({ "header": { "type": header } }),
                );
            }
        }
    }
    value
}

fn direct_outbound() -> Value {
    #[allow(unused_mut)]
    let mut outbound = json!({
        "tag": "direct",
        "protocol": "freedom",
        "settings": {}
    });
    #[cfg(target_os = "linux")]
    outbound.as_object_mut().expect("direct outbound").insert(
        "streamSettings".into(),
        json!({ "sockopt": { "mark": 20220 } }),
    );
    outbound
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Raw => "raw",
        Transport::Websocket => "ws",
        Transport::Grpc => "grpc",
        Transport::Xhttp => "xhttp",
        Transport::Httpupgrade => "httpupgrade",
        Transport::Mkcp => "kcp",
        Transport::Hysteria => "hysteria",
    }
}

fn sniffing(settings: &Settings) -> Value {
    json!({ "enabled": settings.sniffing, "destOverride": ["http", "tls", "quic"], "routeOnly": settings.sniffing_route_only })
}

fn validate_ports(settings: &Settings) -> Result<()> {
    for (name, port) in [
        ("SOCKS", settings.socks_port),
        ("HTTP", settings.http_port),
        ("API", settings.api_port),
    ] {
        if port < 1024 {
            bail!("порт {name} должен быть не ниже 1024");
        }
    }
    if settings.socks_port == settings.http_port
        || settings.socks_port == settings.api_port
        || settings.http_port == settings.api_port
    {
        bail!("порты SOCKS, HTTP и API должны различаться");
    }
    if !(1280..=9000).contains(&settings.mtu) {
        bail!("MTU должен быть от 1280 до 9000");
    }
    let tun_name = settings.tun_interface_name.trim();
    if tun_name.is_empty()
        || tun_name.len() > 15
        || !tun_name
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
    {
        bail!("имя TUN должно содержать до 15 латинских букв, цифр, '-' или '_'");
    }
    if !(1..=1024).contains(&settings.mux_concurrency) {
        bail!("число потоков Mux должно быть от 1 до 1024");
    }
    Ok(())
}
