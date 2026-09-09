use crate::models::{
    ConnectionSpec, LogLevel, Profile, Settings, StreamSettings, Transport, TransportSecurity,
};
use crate::storage::Paths;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

pub const BUNDLED_MIHOMO_VERSION: &str = "v1.19.30";

#[derive(Debug, Clone)]
pub struct MihomoInstallation {
    pub version: String,
    pub binary: PathBuf,
    pub directory: PathBuf,
}

pub fn installed_mihomo(paths: &Paths) -> Result<Option<MihomoInstallation>> {
    #[cfg(target_os = "linux")]
    let directory = PathBuf::from("/usr/lib/nory/cores/mihomo");
    #[cfg(target_os = "windows")]
    let directory = std::env::current_exe()
        .context("путь NORY недоступен")?
        .parent()
        .context("каталог NORY недоступен")?
        .join("cores")
        .join("mihomo");
    let binary = directory.join(mihomo_binary_name());
    if binary.is_file() {
        return Ok(Some(MihomoInstallation {
            version: BUNDLED_MIHOMO_VERSION.into(),
            binary,
            directory,
        }));
    }

    let directory = paths.data_dir.join("mihomo");
    let binary = directory.join(mihomo_binary_name());
    Ok(binary.is_file().then(|| MihomoInstallation {
        version: BUNDLED_MIHOMO_VERSION.into(),
        binary,
        directory,
    }))
}

fn mihomo_binary_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "mihomo.exe"
    } else {
        "mihomo"
    }
}

#[derive(Debug)]
struct ConvertedProxy {
    source_tag: String,
    name: String,
    value: Value,
}

pub fn build_mihomo_config(
    profile: &Profile,
    settings: &Settings,
    bypass_processes: &[String],
) -> Result<Value> {
    if let Some(raw) = profile.raw_config.as_ref()
        && let Some(native) = native_mihomo_config(raw)
    {
        return prepare_native_mihomo_config(native, settings, bypass_processes);
    }

    let (proxies, groups, target) = if let Some(raw) = profile.raw_config.as_ref() {
        let raw =
            native_xray_config(raw).context("JSON не содержит конфигурацию Xray или Mihomo")?;
        convert_xray_config(raw)?
    } else {
        let name = safe_name(&profile.name, "NORY proxy");
        let value = convert_profile(profile, &name, settings.tls_allow_insecure)?;
        (vec![value], Vec::new(), name)
    };
    if proxies.is_empty() {
        bail!("конфигурация не содержит протоколов, поддерживаемых Mihomo");
    }

    let mut rules = bypass_rules(settings, bypass_processes);
    rules.push(format!("MATCH,{target}"));

    let nameservers = settings
        .dns_servers
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .map(str::trim)
        .filter(|server| !server.is_empty() && !server.contains(['\0', '\r', '\n']))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let nameservers = if nameservers.is_empty() {
        vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()]
    } else {
        nameservers
    };

    let mut config = json!({
        "mode": "rule",
        "log-level": mihomo_log_level(settings.log_level),
        "ipv6": settings.enable_ipv6,
        "allow-lan": false,
        "find-process-mode": "strict",
        "unified-delay": true,
        "tcp-concurrent": true,
        "geodata-mode": true,
        "geo-auto-update": false,
        "external-controller": format!("127.0.0.1:{}", settings.api_port),
        "profile": { "store-selected": false, "store-fake-ip": false },
        "dns": {
            "enable": true,
            "ipv6": settings.enable_ipv6,
            "enhanced-mode": "redir-host",
            "nameserver": nameservers
        },
        "tun": {
            "enable": true,
            "stack": "system",
            "device": settings.tun_interface_name,
            "mtu": settings.mtu,
            "auto-route": true,
            "auto-redirect": cfg!(target_os = "linux"),
            "auto-detect-interface": true,
            "strict-route": true,
            "dns-hijack": ["any:53", "tcp://any:53"]
        },
        "proxies": proxies,
        "proxy-groups": groups,
        "rules": rules
    });
    protect_match_targets(config.as_object_mut().expect("generated Mihomo config"))?;
    Ok(config)
}

/// Returns the native Xray half of a JSON profile. Hybrid subscription items
/// can carry both `xray` and `mihomo`; callers then use their native half and
/// avoid a lossy conversion entirely.
pub fn native_xray_config(raw: &Value) -> Option<&Value> {
    raw.get("xray")
        .filter(|value| value.get("outbounds").and_then(Value::as_array).is_some())
        .or_else(|| {
            raw.get("xrayConfig")
                .filter(|value| value.get("outbounds").and_then(Value::as_array).is_some())
        })
        .or_else(|| raw.get("outbounds").and_then(Value::as_array).map(|_| raw))
}

/// Returns the native Mihomo half of a JSON profile without rewriting its
/// proxies, groups or provider-specific options.
pub fn native_mihomo_config(raw: &Value) -> Option<&Value> {
    raw.get("mihomo")
        .filter(|value| value.get("proxies").and_then(Value::as_array).is_some())
        .or_else(|| {
            raw.get("mihomoConfig")
                .filter(|value| value.get("proxies").and_then(Value::as_array).is_some())
        })
        .or_else(|| raw.get("proxies").and_then(Value::as_array).map(|_| raw))
}

pub fn xray_config_from_json(raw: &Value) -> Result<Value> {
    if let Some(native) = native_xray_config(raw) {
        return Ok(native.clone());
    }
    let native =
        native_mihomo_config(raw).context("JSON не содержит конфигурацию Xray или Mihomo")?;
    convert_mihomo_config(native)
}

fn prepare_native_mihomo_config(
    native: &Value,
    settings: &Settings,
    bypass_processes: &[String],
) -> Result<Value> {
    let mut config = native
        .as_object()
        .cloned()
        .context("конфигурация Mihomo должна быть объектом")?;
    if config
        .get("proxies")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
        && config.get("proxy-providers").is_none()
    {
        bail!("в конфигурации Mihomo нет proxies или proxy-providers");
    }

    // Provider JSON may contain local listeners or a downloadable dashboard.
    // They are unrelated to NORY's TUN session and must not run as root.
    for key in [
        "port",
        "socks-port",
        "redir-port",
        "tproxy-port",
        "mixed-port",
        "bind-address",
        "authentication",
        "external-ui",
        "external-ui-name",
        "external-ui-url",
        "secret",
    ] {
        config.remove(key);
    }

    config.insert("mode".into(), json!("rule"));
    config.insert(
        "log-level".into(),
        json!(mihomo_log_level(settings.log_level)),
    );
    config.insert("ipv6".into(), json!(settings.enable_ipv6));
    config.insert("allow-lan".into(), json!(false));
    config.insert("find-process-mode".into(), json!("strict"));
    if settings.routing.bypass_ru {
        config.insert("geodata-mode".into(), json!(true));
        config.insert("geo-auto-update".into(), json!(false));
    }
    config.insert(
        "external-controller".into(),
        json!(format!("127.0.0.1:{}", settings.api_port)),
    );
    config.insert(
        "tun".into(),
        json!({
            "enable": true,
            "stack": "system",
            "device": settings.tun_interface_name,
            "mtu": settings.mtu,
            "auto-route": true,
            "auto-redirect": cfg!(target_os = "linux"),
            "auto-detect-interface": true,
            "strict-route": true,
            "dns-hijack": ["any:53", "tcp://any:53"]
        }),
    );
    let mut rules = bypass_rules(settings, bypass_processes);
    rules.extend(
        config
            .remove("rules")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|rule| rule.as_str().map(ToOwned::to_owned)),
    );
    if !rules.iter().any(|rule| {
        rule.split_once(',')
            .is_some_and(|(kind, _)| kind.trim().eq_ignore_ascii_case("MATCH"))
    }) {
        let target = config
            .get("proxy-groups")
            .and_then(Value::as_array)
            .and_then(|groups| groups.first())
            .and_then(|group| group.get("name"))
            .and_then(Value::as_str)
            .or_else(|| {
                config
                    .get("proxies")
                    .and_then(Value::as_array)
                    .and_then(|proxies| proxies.first())
                    .and_then(|proxy| proxy.get("name"))
                    .and_then(Value::as_str)
            })
            .context("в конфигурации Mihomo отсутствует целевой proxy")?;
        rules.push(format!("MATCH,{target}"));
    }
    config.insert(
        "rules".into(),
        Value::Array(rules.into_iter().map(Value::String).collect()),
    );
    protect_match_targets(&mut config)?;
    Ok(Value::Object(config))
}

/// Mihomo splits rule strings on commas without CSV quoting. MATCH has no
/// payload or parameters, so a comma in its target name breaks routing.
/// Use a one-member select group as an internal alias: JSON group members can
/// refer to arbitrary names, preserving native proxies, balancers and detours.
/// This also repairs MATCH rules saved by older NORY subscription imports.
fn protect_match_targets(config: &mut Map<String, Value>) -> Result<()> {
    let names = ["proxies", "proxy-groups"]
        .into_iter()
        .filter_map(|key| config.get(key).and_then(Value::as_array))
        .flatten()
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<HashSet<_>>();
    let mut used_names = names.clone();
    let mut aliases = HashMap::new();
    let mut groups = Vec::new();
    if let Some(rules) = config.get_mut("rules").and_then(Value::as_array_mut) {
        for rule in rules {
            let Some((kind, target)) = rule.as_str().and_then(|rule| rule.split_once(',')) else {
                continue;
            };
            if !kind.trim().eq_ignore_ascii_case("MATCH") {
                continue;
            }
            // Resolve the complete name, never its comma-separated prefix.
            // Unknown targets remain errors; they must not fall back to DIRECT.
            let Some(target) = names.get(target).or_else(|| names.get(target.trim())) else {
                continue;
            };
            if !target.contains(',') && target.trim() == target {
                continue;
            }
            let alias = aliases.entry(target.clone()).or_insert_with(|| {
                let alias = unique_name("NORY-MATCH", &mut used_names);
                groups.push(json!({
                    "name": alias,
                    "type": "select",
                    "proxies": [target]
                }));
                alias
            });
            *rule = json!(format!("MATCH,{alias}"));
        }
    }
    if !groups.is_empty() {
        config
            .entry("proxy-groups")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("proxy-groups Mihomo должен быть массивом")?
            .extend(groups);
    }
    Ok(())
}

fn bypass_rules(settings: &Settings, bypass_processes: &[String]) -> Vec<String> {
    let mut rules = Vec::new();
    let own_processes: &[&str] = if cfg!(target_os = "windows") {
        &[
            "nory.exe",
            "nory-helper.exe",
            "xray.exe",
            "sing-box.exe",
            "mihomo.exe",
        ]
    } else {
        &["nory", "xray", "sing-box", "mihomo"]
    };
    for process in own_processes
        .iter()
        .copied()
        .chain(bypass_processes.iter().map(String::as_str))
    {
        let process = process.trim().replace('\\', "/");
        if process.is_empty() || process.contains([',', '\n', '\r', '\0']) {
            continue;
        }
        if process.contains('/') {
            let process = if cfg!(target_os = "windows") {
                process.replace('/', "\\")
            } else {
                process
            };
            rules.push(format!("PROCESS-PATH,{process},DIRECT"));
        } else {
            rules.push(format!("PROCESS-NAME,{process},DIRECT"));
        }
    }
    for domain in &settings.routing.bypass_domains {
        if let Some(rule) = mihomo_domain_rule(domain) {
            rules.push(rule);
        }
    }
    if settings.routing.bypass_ru {
        rules.push("GEOSITE,category-ru,DIRECT".into());
        rules.push("GEOIP,RU,DIRECT".into());
    }
    rules
}

fn convert_xray_config(raw: &Value) -> Result<(Vec<Value>, Vec<Value>, String)> {
    let object = raw
        .as_object()
        .context("исходная Xray-конфигурация должна быть объектом")?;
    let outbounds = object
        .get("outbounds")
        .and_then(Value::as_array)
        .context("в Xray-конфигурации отсутствуют outbounds")?;
    let mut used_names = HashSet::new();
    let mut converted = Vec::new();
    let mut unsupported = Vec::new();
    for (index, outbound) in outbounds.iter().enumerate() {
        let protocol = text(outbound, "protocol").unwrap_or_default();
        if matches!(
            protocol.as_str(),
            "freedom" | "blackhole" | "dns" | "loopback"
        ) {
            continue;
        }
        let source_tag = text(outbound, "tag")
            .filter(|tag| !tag.is_empty())
            .unwrap_or_else(|| format!("proxy-{index}"));
        let name = unique_name(&source_tag, &mut used_names);
        match convert_xray_outbound(outbound, &name) {
            Ok(value) => converted.push(ConvertedProxy {
                source_tag,
                name,
                value,
            }),
            Err(_) => unsupported.push(protocol.to_string()),
        }
    }
    if converted.is_empty() {
        unsupported.sort();
        unsupported.dedup();
        bail!(
            "Mihomo не поддерживает outbounds из этой конфигурации: {}",
            unsupported.join(", ")
        );
    }

    let source_to_name = converted
        .iter()
        .map(|proxy| (proxy.source_tag.clone(), proxy.name.clone()))
        .collect::<HashMap<_, _>>();
    for proxy in &mut converted {
        let Some(outbound) = outbounds
            .iter()
            .find(|outbound| text(outbound, "tag").as_deref() == Some(proxy.source_tag.as_str()))
        else {
            continue;
        };
        if let Some(detour) = outbound
            .get("proxySettings")
            .and_then(|value| text(value, "tag"))
            .and_then(|tag| source_to_name.get(&tag))
            && let Some(object) = proxy.value.as_object_mut()
        {
            object.insert("dialer-proxy".into(), Value::String(detour.clone()));
        }
    }

    let mut groups = Vec::new();
    let mut balancer_names = HashMap::new();
    if let Some(balancers) = object
        .get("routing")
        .and_then(|routing| routing.get("balancers"))
        .and_then(Value::as_array)
    {
        for (index, balancer) in balancers.iter().enumerate() {
            let source_tag = text(balancer, "tag")
                .filter(|tag| !tag.is_empty())
                .unwrap_or_else(|| format!("balancer-{index}"));
            let selectors = balancer
                .get("selector")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let members = converted
                .iter()
                .filter(|proxy| {
                    selectors.is_empty()
                        || selectors
                            .iter()
                            .any(|selector| proxy.source_tag.starts_with(selector))
                })
                .map(|proxy| proxy.name.clone())
                .collect::<Vec<_>>();
            if members.is_empty() {
                continue;
            }
            let group_name = unique_name(&source_tag, &mut used_names);
            let strategy = balancer
                .get("strategy")
                .and_then(|strategy| text(strategy, "type"))
                .map_or("consistent-hashing", |strategy| match strategy.as_str() {
                    "random" | "roundRobin" => "round-robin",
                    _ => "consistent-hashing",
                });
            groups.push(json!({
                "name": group_name,
                "type": "load-balance",
                "proxies": members,
                "url": "https://www.gstatic.com/generate_204",
                "interval": 300,
                "lazy": true,
                "strategy": strategy
            }));
            balancer_names.insert(source_tag, group_name);
        }
    }

    let routed_target = object
        .get("routing")
        .and_then(|routing| routing.get("rules"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|rule| {
            text(rule, "balancerTag")
                .and_then(|tag| balancer_names.get(&tag).cloned())
                .or_else(|| {
                    text(rule, "outboundTag").and_then(|tag| source_to_name.get(&tag).cloned())
                })
        });
    let target = routed_target
        .or_else(|| balancer_names.values().next().cloned())
        .or_else(|| source_to_name.get("proxy").cloned())
        .unwrap_or_else(|| converted[0].name.clone());
    Ok((
        converted.into_iter().map(|proxy| proxy.value).collect(),
        groups,
        target,
    ))
}

fn convert_mihomo_config(raw: &Value) -> Result<Value> {
    let object = raw
        .as_object()
        .context("конфигурация Mihomo должна быть объектом")?;
    let proxies = object
        .get("proxies")
        .and_then(Value::as_array)
        .filter(|proxies| !proxies.is_empty())
        .context("в конфигурации Mihomo отсутствуют proxies")?;

    let mut outbounds = Vec::new();
    let mut proxy_tags = HashSet::new();
    for proxy in proxies {
        let outbound = convert_mihomo_proxy(proxy)?;
        if let Some(tag) = outbound.get("tag").and_then(Value::as_str) {
            proxy_tags.insert(tag.to_string());
        }
        outbounds.push(outbound);
    }

    let mut balancers = Vec::new();
    let mut balancer_tags = HashSet::new();
    if let Some(groups) = object.get("proxy-groups").and_then(Value::as_array) {
        for group in groups {
            let Some(name) = text(group, "name") else {
                continue;
            };
            let members = group
                .get("proxies")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|member| proxy_tags.contains(*member))
                .map(|member| Value::String(member.to_string()))
                .collect::<Vec<_>>();
            if members.is_empty() {
                continue;
            }
            let strategy = match text(group, "strategy").as_deref() {
                Some("round-robin") | Some("random") => "random",
                _ => "leastLoad",
            };
            balancers.push(json!({
                "tag": name,
                "selector": members,
                "strategy": { "type": strategy }
            }));
            balancer_tags.insert(name);
        }
    }

    let target = object
        .get("rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|rule| rule.strip_prefix("MATCH,"))
        .map(str::trim)
        .find(|target| !target.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| balancer_tags.iter().next().cloned())
        .or_else(|| proxy_tags.iter().next().cloned())
        .context("не удалось определить целевой proxy Mihomo")?;
    let final_rule = if balancer_tags.contains(&target) {
        json!({ "type": "field", "network": "tcp,udp", "balancerTag": target })
    } else {
        json!({ "type": "field", "network": "tcp,udp", "outboundTag": target })
    };

    let observer = (!balancers.is_empty()).then(|| {
        json!({
            "subjectSelector": proxy_tags.into_iter().collect::<Vec<_>>(),
            "pingConfig": {
                "destination": "https://www.gstatic.com/generate_204",
                "interval": "30s",
                "connectivity": "http://connectivitycheck.gstatic.com/generate_204",
                "timeout": "5s",
                "sampling": 3
            }
        })
    });
    let mut converted = json!({
        "outbounds": outbounds,
        "routing": {
            "domainStrategy": "IPIfNonMatch",
            "balancers": balancers,
            "rules": [final_rule]
        },
        "remarks": raw.get("name").cloned().unwrap_or_else(|| json!("Mihomo JSON"))
    });
    if let Some(observer) = observer {
        converted
            .as_object_mut()
            .expect("converted Xray config")
            .insert("burstObservatory".into(), observer);
    }
    Ok(converted)
}

fn convert_mihomo_proxy(proxy: &Value) -> Result<Value> {
    let proxy_type = text(proxy, "type").context("у proxy Mihomo отсутствует type")?;
    let name = text(proxy, "name").context("у proxy Mihomo отсутствует name")?;
    let address = text(proxy, "server").context("у proxy Mihomo отсутствует server")?;
    let port = number(proxy, "port").context("у proxy Mihomo отсутствует port")?;
    let mut outbound = Map::new();
    outbound.insert("tag".into(), json!(name));
    let settings = match proxy_type.as_str() {
        "vless" | "vmess" => {
            outbound.insert("protocol".into(), json!(proxy_type));
            let mut user = Map::new();
            user.insert(
                "id".into(),
                json!(text(proxy, "uuid").context("у proxy Mihomo отсутствует uuid")?),
            );
            if proxy_type == "vless" {
                user.insert(
                    "encryption".into(),
                    json!(text(proxy, "encryption").unwrap_or_else(|| "none".into())),
                );
                insert_nonempty(&mut user, "flow", text(proxy, "flow").as_deref());
                insert_nonempty(
                    &mut user,
                    "packetEncoding",
                    text(proxy, "packet-encoding").as_deref(),
                );
            } else {
                user.insert(
                    "alterId".into(),
                    json!(number(proxy, "alterId").unwrap_or(0)),
                );
                user.insert(
                    "security".into(),
                    json!(text(proxy, "cipher").unwrap_or_else(|| "auto".into())),
                );
            }
            json!({ "vnext": [{ "address": address, "port": port, "users": [user] }] })
        }
        "trojan" | "ss" | "socks5" => {
            let protocol = match proxy_type.as_str() {
                "ss" => "shadowsocks",
                "socks5" => "socks",
                _ => "trojan",
            };
            outbound.insert("protocol".into(), json!(protocol));
            let mut server = Map::new();
            server.insert("address".into(), json!(address));
            server.insert("port".into(), json!(port));
            if proxy_type == "ss" {
                server.insert(
                    "method".into(),
                    json!(text(proxy, "cipher").context("у Shadowsocks отсутствует cipher")?),
                );
                server.insert(
                    "password".into(),
                    json!(text(proxy, "password").context("у Shadowsocks отсутствует password")?),
                );
            } else if proxy_type == "trojan" {
                server.insert(
                    "password".into(),
                    json!(text(proxy, "password").context("у Trojan отсутствует password")?),
                );
            } else if text(proxy, "username").is_some() || text(proxy, "password").is_some() {
                server.insert(
                    "users".into(),
                    json!([{
                        "user": text(proxy, "username").unwrap_or_default(),
                        "pass": text(proxy, "password").unwrap_or_default()
                    }]),
                );
            }
            json!({ "servers": [server] })
        }
        "hysteria2" => {
            outbound.insert("protocol".into(), json!("hysteria"));
            json!({ "version": 2, "address": address, "port": port })
        }
        _ => bail!("тип Mihomo {proxy_type} невозможно преобразовать в Xray"),
    };
    outbound.insert("settings".into(), settings);
    outbound.insert(
        "streamSettings".into(),
        mihomo_stream_to_xray(proxy, proxy_type == "hysteria2"),
    );
    if let Some(detour) = text(proxy, "dialer-proxy") {
        outbound.insert("proxySettings".into(), json!({ "tag": detour }));
    }
    Ok(Value::Object(outbound))
}

fn mihomo_stream_to_xray(proxy: &Value, hysteria: bool) -> Value {
    let network =
        text(proxy, "network").unwrap_or_else(|| if hysteria { "hysteria" } else { "raw" }.into());
    let mut stream = Map::new();
    stream.insert(
        "network".into(),
        json!(match network.as_str() {
            "websocket" => "ws",
            "http-upgrade" => "httpupgrade",
            "h2" => "http",
            other => other,
        }),
    );
    match network.as_str() {
        "ws" | "websocket" => {
            let options = proxy.get("ws-opts").unwrap_or(&Value::Null);
            stream.insert(
                "wsSettings".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "headers": options.get("headers").cloned().unwrap_or_else(|| json!({}))
                }),
            );
        }
        "grpc" => {
            let options = proxy.get("grpc-opts").unwrap_or(&Value::Null);
            stream.insert(
                "grpcSettings".into(),
                json!({ "serviceName": text(options, "grpc-service-name").unwrap_or_default() }),
            );
        }
        "xhttp" => {
            let options = proxy.get("xhttp-opts").unwrap_or(&Value::Null);
            stream.insert(
                "xhttpSettings".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": text(options, "host").unwrap_or_default(),
                    "mode": text(options, "mode").unwrap_or_else(|| "auto".into())
                }),
            );
        }
        "http-upgrade" => {
            let options = proxy.get("http-upgrade-opts").unwrap_or(&Value::Null);
            stream.insert(
                "httpupgradeSettings".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": text(options, "host").unwrap_or_default()
                }),
            );
        }
        "h2" => {
            let options = proxy.get("h2-opts").unwrap_or(&Value::Null);
            stream.insert(
                "httpSettings".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": options.get("host").cloned().unwrap_or_else(|| json!([]))
                }),
            );
        }
        _ => {}
    }
    if hysteria {
        stream.insert(
            "hysteriaSettings".into(),
            json!({
                "version": 2,
                "auth": text(proxy, "password").unwrap_or_default()
            }),
        );
    }

    // Trojan always uses TLS in Mihomo; native subscriptions do not need to
    // carry an extra `tls: true` flag. Never downgrade it during conversion.
    let tls = proxy.get("tls").and_then(Value::as_bool).unwrap_or(false)
        || proxy.get("type").and_then(Value::as_str) == Some("trojan");
    let reality = proxy.get("reality-opts").and_then(Value::as_object);
    if let Some(reality) = reality {
        stream.insert("security".into(), json!("reality"));
        stream.insert(
            "realitySettings".into(),
            json!({
                "serverName": mihomo_sni(proxy).unwrap_or_default(),
                "fingerprint": text(proxy, "client-fingerprint").unwrap_or_else(|| "chrome".into()),
                "publicKey": reality.get("public-key").cloned().unwrap_or(Value::Null),
                "shortId": reality.get("short-id").cloned().unwrap_or(Value::Null)
            }),
        );
    } else if tls || hysteria {
        stream.insert("security".into(), json!("tls"));
        stream.insert(
            "tlsSettings".into(),
            json!({
                "serverName": mihomo_sni(proxy).unwrap_or_default(),
                "fingerprint": text(proxy, "client-fingerprint").unwrap_or_else(|| "chrome".into()),
                "alpn": proxy.get("alpn").cloned().unwrap_or_else(|| json!([])),
                "allowInsecure": proxy.get("skip-cert-verify").and_then(Value::as_bool).unwrap_or(false)
            }),
        );
    } else {
        stream.insert("security".into(), json!("none"));
    }
    Value::Object(stream)
}

fn convert_profile(profile: &Profile, name: &str, insecure: bool) -> Result<Value> {
    let mut proxy = Map::new();
    proxy.insert("name".into(), Value::String(name.into()));
    proxy.insert("server".into(), Value::String(profile.address.clone()));
    proxy.insert("port".into(), json!(profile.port));
    proxy.insert("udp".into(), Value::Bool(true));
    match &profile.connection {
        ConnectionSpec::Vless {
            id,
            encryption,
            flow,
        } => {
            proxy.insert("type".into(), json!("vless"));
            proxy.insert("uuid".into(), json!(id));
            proxy.insert("encryption".into(), json!(encryption));
            insert_nonempty(&mut proxy, "flow", flow.as_deref());
        }
        ConnectionSpec::Vmess {
            id,
            alter_id,
            security,
        } => {
            proxy.insert("type".into(), json!("vmess"));
            proxy.insert("uuid".into(), json!(id));
            proxy.insert("alterId".into(), json!(alter_id));
            proxy.insert("cipher".into(), json!(security));
        }
        ConnectionSpec::Trojan { password } => {
            proxy.insert("type".into(), json!("trojan"));
            proxy.insert("password".into(), json!(password));
        }
        ConnectionSpec::Shadowsocks { method, password } => {
            proxy.insert("type".into(), json!("ss"));
            proxy.insert("cipher".into(), json!(method));
            proxy.insert("password".into(), json!(password));
        }
        ConnectionSpec::Socks { username, password } => {
            proxy.insert("type".into(), json!("socks5"));
            insert_nonempty(&mut proxy, "username", username.as_deref());
            insert_nonempty(&mut proxy, "password", password.as_deref());
        }
        ConnectionSpec::Hysteria2 { auth } => {
            proxy.insert("type".into(), json!("hysteria2"));
            proxy.insert("password".into(), json!(auth));
        }
        ConnectionSpec::XrayJson { name } => {
            bail!("протокол {name} невозможно преобразовать без исходного JSON")
        }
    }
    apply_model_stream(&mut proxy, &profile.stream, insecure);
    Ok(Value::Object(proxy))
}

fn convert_xray_outbound(outbound: &Value, name: &str) -> Result<Value> {
    let protocol = text(outbound, "protocol").context("у outbound отсутствует protocol")?;
    let settings = outbound.get("settings").unwrap_or(&Value::Null);
    let stream = outbound.get("streamSettings").unwrap_or(&Value::Null);
    let mut proxy = Map::new();
    proxy.insert("name".into(), Value::String(name.into()));
    proxy.insert("udp".into(), Value::Bool(true));
    match protocol.as_str() {
        "vless" | "vmess" => {
            let endpoint = settings
                .get("vnext")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .context("в outbound отсутствует vnext")?;
            let user = endpoint
                .get("users")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .context("в outbound отсутствует пользователь")?;
            proxy.insert("type".into(), json!(protocol));
            proxy.insert(
                "server".into(),
                json!(text(endpoint, "address").context("отсутствует адрес сервера")?),
            );
            proxy.insert(
                "port".into(),
                json!(number(endpoint, "port").context("отсутствует порт сервера")?),
            );
            proxy.insert(
                "uuid".into(),
                json!(text(user, "id").context("отсутствует UUID")?),
            );
            if protocol == "vless" {
                insert_nonempty(&mut proxy, "flow", text(user, "flow").as_deref());
                insert_nonempty(
                    &mut proxy,
                    "packet-encoding",
                    text(user, "packetEncoding").as_deref(),
                );
                insert_nonempty(
                    &mut proxy,
                    "encryption",
                    text(user, "encryption").as_deref(),
                );
            } else {
                proxy.insert(
                    "alterId".into(),
                    json!(number(user, "alterId").unwrap_or(0)),
                );
                proxy.insert(
                    "cipher".into(),
                    json!(text(user, "security").unwrap_or_else(|| "auto".into())),
                );
            }
        }
        "trojan" | "shadowsocks" | "socks" => {
            let server = settings
                .get("servers")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .context("в outbound отсутствует servers")?;
            proxy.insert(
                "type".into(),
                json!(match protocol.as_str() {
                    "shadowsocks" => "ss",
                    "socks" => "socks5",
                    _ => "trojan",
                }),
            );
            proxy.insert(
                "server".into(),
                json!(text(server, "address").context("отсутствует адрес сервера")?),
            );
            proxy.insert(
                "port".into(),
                json!(number(server, "port").context("отсутствует порт сервера")?),
            );
            if protocol == "shadowsocks" {
                proxy.insert(
                    "cipher".into(),
                    json!(text(server, "method").context("отсутствует cipher")?),
                );
                proxy.insert(
                    "password".into(),
                    json!(text(server, "password").context("отсутствует пароль")?),
                );
            } else if protocol == "trojan" {
                proxy.insert(
                    "password".into(),
                    json!(text(server, "password").context("отсутствует пароль")?),
                );
            } else if let Some(user) = server
                .get("users")
                .and_then(Value::as_array)
                .and_then(|users| users.first())
            {
                insert_nonempty(&mut proxy, "username", text(user, "user").as_deref());
                insert_nonempty(&mut proxy, "password", text(user, "pass").as_deref());
            }
        }
        "hysteria" | "hysteria2" => {
            proxy.insert("type".into(), json!("hysteria2"));
            proxy.insert(
                "server".into(),
                json!(text(settings, "address").context("отсутствует адрес Hysteria2")?),
            );
            proxy.insert(
                "port".into(),
                json!(number(settings, "port").context("отсутствует порт Hysteria2")?),
            );
            let hysteria = stream.get("hysteriaSettings").unwrap_or(&Value::Null);
            proxy.insert(
                "password".into(),
                json!(text(hysteria, "auth").context("отсутствует auth Hysteria2")?),
            );
        }
        _ => bail!("протокол {protocol} не поддерживается Mihomo"),
    }
    apply_xray_stream(&mut proxy, stream);
    Ok(Value::Object(proxy))
}

fn apply_model_stream(proxy: &mut Map<String, Value>, stream: &StreamSettings, insecure: bool) {
    let mut raw = json!({
        "network": stream.network.as_xray(),
        "security": stream.security.as_xray()
    });
    let object = raw.as_object_mut().expect("stream object");
    if stream.security == TransportSecurity::Reality {
        object.insert(
            "realitySettings".into(),
            json!({
                "serverName": stream.server_name,
                "fingerprint": stream.fingerprint,
                "publicKey": stream.public_key,
                "shortId": stream.short_id
            }),
        );
    } else if stream.security == TransportSecurity::Tls {
        object.insert(
            "tlsSettings".into(),
            json!({
                "serverName": stream.server_name,
                "fingerprint": stream.fingerprint,
                "alpn": stream.alpn,
                "allowInsecure": insecure
            }),
        );
    }
    match stream.network {
        Transport::Websocket => {
            object.insert(
                "wsSettings".into(),
                json!({ "path": stream.path, "headers": { "Host": stream.host } }),
            );
        }
        Transport::Grpc => {
            object.insert(
                "grpcSettings".into(),
                json!({ "serviceName": stream.service_name, "mode": stream.mode }),
            );
        }
        Transport::Xhttp => {
            object.insert(
                "xhttpSettings".into(),
                json!({ "path": stream.path, "host": stream.host, "mode": stream.mode }),
            );
        }
        Transport::Httpupgrade => {
            object.insert(
                "httpupgradeSettings".into(),
                json!({ "path": stream.path, "host": stream.host }),
            );
        }
        _ => {}
    }
    apply_xray_stream(proxy, &raw);
}

fn apply_xray_stream(proxy: &mut Map<String, Value>, stream: &Value) {
    let network = text(stream, "network").unwrap_or_else(|| "tcp".into());
    match network.as_str() {
        "ws" | "websocket" => {
            proxy.insert("network".into(), json!("ws"));
            let options = stream.get("wsSettings").unwrap_or(&Value::Null);
            proxy.insert(
                "ws-opts".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "headers": options.get("headers").cloned().unwrap_or_else(|| json!({}))
                }),
            );
        }
        "grpc" => {
            proxy.insert("network".into(), json!("grpc"));
            let options = stream.get("grpcSettings").unwrap_or(&Value::Null);
            proxy.insert(
                "grpc-opts".into(),
                json!({ "grpc-service-name": text(options, "serviceName").unwrap_or_default() }),
            );
        }
        "xhttp" | "splithttp" => {
            proxy.insert("network".into(), json!("xhttp"));
            let options = stream
                .get("xhttpSettings")
                .or_else(|| stream.get("splithttpSettings"))
                .unwrap_or(&Value::Null);
            proxy.insert(
                "xhttp-opts".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": text(options, "host").unwrap_or_default(),
                    "mode": text(options, "mode").unwrap_or_else(|| "auto".into())
                }),
            );
        }
        "httpupgrade" | "http-upgrade" => {
            proxy.insert("network".into(), json!("http-upgrade"));
            let options = stream.get("httpupgradeSettings").unwrap_or(&Value::Null);
            proxy.insert(
                "http-upgrade-opts".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": text(options, "host").unwrap_or_default()
                }),
            );
        }
        "http" | "h2" => {
            proxy.insert("network".into(), json!("h2"));
            let options = stream.get("httpSettings").unwrap_or(&Value::Null);
            proxy.insert(
                "h2-opts".into(),
                json!({
                    "path": text(options, "path").unwrap_or_else(|| "/".into()),
                    "host": options.get("host").cloned().unwrap_or_else(|| json!([]))
                }),
            );
        }
        _ => {}
    }

    let security = text(stream, "security").unwrap_or_default();
    if security == "tls" || security == "reality" {
        proxy.insert("tls".into(), Value::Bool(true));
        let tls = if security == "reality" {
            stream.get("realitySettings").unwrap_or(&Value::Null)
        } else {
            stream.get("tlsSettings").unwrap_or(&Value::Null)
        };
        let sni_key = match proxy.get("type").and_then(Value::as_str) {
            Some("hysteria2" | "trojan" | "socks5") => "sni",
            _ => "servername",
        };
        insert_nonempty(proxy, sni_key, text(tls, "serverName").as_deref());
        insert_nonempty(
            proxy,
            "client-fingerprint",
            text(tls, "fingerprint").as_deref(),
        );
        if let Some(alpn) = tls.get("alpn").and_then(Value::as_array) {
            proxy.insert("alpn".into(), Value::Array(alpn.clone()));
        }
        proxy.insert(
            "skip-cert-verify".into(),
            Value::Bool(
                tls.get("allowInsecure")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ),
        );
        if security == "reality" {
            proxy.insert(
                "reality-opts".into(),
                json!({
                    "public-key": text(tls, "publicKey").unwrap_or_default(),
                    "short-id": text(tls, "shortId").unwrap_or_default()
                }),
            );
        }
    }
}

fn mihomo_sni(proxy: &Value) -> Option<String> {
    match proxy.get("type").and_then(Value::as_str) {
        Some("hysteria2" | "trojan" | "socks5") => {
            text(proxy, "sni").or_else(|| text(proxy, "servername"))
        }
        _ => text(proxy, "servername"),
    }
}

fn mihomo_domain_rule(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains([',', '\0', '\r', '\n']) {
        return None;
    }
    let (kind, body) = value.split_once(':').unwrap_or(("domain", value));
    let kind = match kind {
        "full" => "DOMAIN",
        "keyword" => "DOMAIN-KEYWORD",
        "domain" => "DOMAIN-SUFFIX",
        _ => return None,
    };
    let body = body.trim_start_matches("*.").trim_start_matches('.');
    (!body.is_empty()).then(|| format!("{kind},{body},DIRECT"))
}

fn safe_name(value: &str, fallback: &str) -> String {
    let value = value.trim().chars().take(120).collect::<String>();
    if value.is_empty()
        || value.eq_ignore_ascii_case("DIRECT")
        || value.eq_ignore_ascii_case("REJECT")
    {
        fallback.into()
    } else {
        value
    }
}

fn unique_name(value: &str, used: &mut HashSet<String>) -> String {
    let base = safe_name(value, "NORY proxy");
    let mut name = base.clone();
    let mut suffix = 2;
    while !used.insert(name.clone()) {
        name = format!("{base} #{suffix}");
        suffix += 1;
    }
    name
}

fn insert_nonempty(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        object.insert(key.into(), Value::String(value.into()));
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| {
        value
            .as_str()
            .map(ToOwned::to_owned)
            .or_else(|| value.as_u64().map(|number| number.to_string()))
    })
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    })
}

fn mihomo_log_level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::None | LogLevel::Error => "error",
        LogLevel::Warning => "warning",
        LogLevel::Info => "info",
    }
}
