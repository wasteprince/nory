//! Translate routing selectors, preserving order and conjunctions. A selector
//! unsupported by the other core is an error, never an unannounced route change.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

fn strings(value: &Value) -> Result<Vec<&str>> {
    value
        .as_array()
        .context("ожидается массив условий маршрутизации")?
        .iter()
        .map(|v| {
            v.as_str()
                .context("условие маршрутизации должно быть строкой")
        })
        .collect()
}

fn atom(kind: &str, value: &str) -> Result<String> {
    if value.is_empty() || value.contains([',', '\n', '\r', '\0']) {
        bail!("условие {kind} нельзя представить в формате правил Mihomo");
    }
    Ok(format!("{kind},{value}"))
}
fn logical(kind: &str, parts: Vec<String>) -> String {
    if parts.len() == 1 {
        return parts[0].clone();
    }
    format!(
        "{kind},({})",
        parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn domain(value: &str) -> Result<String> {
    for (prefix, kind) in [
        ("domain:", "DOMAIN-SUFFIX"),
        ("full:", "DOMAIN"),
        ("keyword:", "DOMAIN-KEYWORD"),
        ("regexp:", "DOMAIN-REGEX"),
        ("geosite:", "GEOSITE"),
    ] {
        if let Some(value) = value.strip_prefix(prefix) {
            return atom(kind, value);
        }
    }
    if value.starts_with("ext:") {
        bail!("внешний GeoSite-файл Xray требует ядро Xray");
    }
    atom("DOMAIN-KEYWORD", value)
}
fn ip(value: &str, source: bool) -> Result<String> {
    if let Some(tag) = value.strip_prefix("geoip:") {
        let (inverse, tag) = tag.strip_prefix('!').map_or((false, tag), |v| (true, v));
        let rule = atom(if source { "SRC-GEOIP" } else { "GEOIP" }, tag)?;
        return Ok(if inverse {
            format!("NOT,(({rule}))")
        } else {
            rule
        });
    }
    if value.starts_with("ext:") {
        bail!("внешний GeoIP-файл Xray требует ядро Xray");
    }
    let value = if value.contains('/') {
        value.into()
    } else {
        format!("{value}/{}", if value.contains(':') { 128 } else { 32 })
    };
    atom(if source { "SRC-IP-CIDR" } else { "IP-CIDR" }, &value)
}

pub(crate) fn xray_to_mihomo(
    raw: &Value,
    targets: &HashMap<String, String>,
) -> Result<Vec<String>> {
    let Some(rules) = raw.pointer("/routing/rules") else {
        return Ok(Vec::new());
    };
    rules.as_array().context("routing.rules должен быть массивом")?.iter().enumerate().map(|(index, rule)| {
        let result = (|| -> Result<String> {
            let object = rule.as_object().context("правило Xray должно быть объектом")?;
            let mut parts = Vec::new();
            for (key, value) in object {
                let part = match key.as_str() {
                    "type" | "ruleTag" | "outboundTag" | "balancerTag" => continue,
                    "domain" => strings(value)?.into_iter().map(domain).collect::<Result<Vec<_>>>()?,
                    "ip" | "source" => strings(value)?.into_iter().map(|v| ip(v, key == "source")).collect::<Result<Vec<_>>>()?,
                    "network" => {
                        let networks = value.as_str().context("network должен быть строкой")?.split(',').map(str::trim).collect::<Vec<_>>();
                        if networks.contains(&"tcp") && networks.contains(&"udp") && networks.len() == 2 { continue; }
                        networks.into_iter().map(|v| atom("NETWORK", &v.to_uppercase())).collect::<Result<Vec<_>>>()?
                    }
                    "port" | "sourcePort" => {
                        let ports = value.as_str().map(ToOwned::to_owned).or_else(|| value.as_u64().map(|n| n.to_string())).context("неверное условие порта")?;
                        ports.split(',').map(|v| atom(if key == "port" { "DST-PORT" } else { "SRC-PORT" }, v.trim())).collect::<Result<Vec<_>>>()?
                    }
                    "inboundTag" => {
                        // All provider client listeners are replaced by NORY's TUN.
                        let inbounds = raw.get("inbounds").and_then(Value::as_array);
                        let tags = strings(value)?;
                        if !tags.is_empty() && tags.iter().all(|tag| *tag == "proxy-in" || inbounds.is_some_and(|items| items.iter().any(|i| i["tag"] == *tag && i["protocol"] != "dokodemo-door"))) { continue; }
                        bail!("ограничение inboundTag требует исходное ядро Xray");
                    }
                    _ => bail!("условие «{key}» не поддерживается Mihomo; выберите Xray для этого JSON"),
                };
                if !part.is_empty() { parts.push(logical("OR", part)); }
            }
            let tag = rule.get("outboundTag").or_else(|| rule.get("balancerTag")).and_then(Value::as_str).context("не задана цель правила")?;
            let target = targets.get(tag).with_context(|| format!("цель «{tag}» недоступна в Mihomo"))?;
            Ok(if parts.is_empty() { format!("MATCH,{target}") } else { format!("{},{target}", logical("AND", parts)) })
        })();
        result.with_context(|| format!("Правило Xray №{} не перенесено; подключение отменено, чтобы не потерять маршрутизацию", index + 1))
    }).collect()
}

pub(crate) fn mihomo_to_xray(
    raw: &Value,
    proxies: &HashSet<String>,
    balancers: &HashSet<String>,
) -> Result<Vec<Value>> {
    let Some(rules) = raw.get("rules") else {
        return Ok(Vec::new());
    };
    rules.as_array().context("rules должен быть массивом")?.iter().enumerate().map(|(index, value)| {
        let result = (|| -> Result<Value> {
            let text = value.as_str().context("правило Mihomo должно быть строкой")?;
            let (kind, rest) = text.split_once(',').context("неверное правило Mihomo")?;
            let kind = kind.trim().to_uppercase();
            let (payload, target) = if kind == "MATCH" { ("", rest.trim()) }
                else { rest.rsplit_once(',').context("не задана цель правила")? };
            let target = target.trim();
            let mut rule = json!({"type": "field"});
            if balancers.contains(target) { rule["balancerTag"] = json!(target); }
            else if proxies.contains(target) || matches!(target, "DIRECT" | "REJECT" | "REJECT-DROP") { rule["outboundTag"] = json!(target); }
            else { bail!("цель «{target}» недоступна в Xray (параметры no-resolve и внешние providers требуют Mihomo)"); }
            let (key, value) = match kind.as_str() {
                "MATCH" => ("network", json!("tcp,udp")),
                "DOMAIN" => ("domain", json!([format!("full:{payload}")])),
                "DOMAIN-SUFFIX" => ("domain", json!([format!("domain:{payload}")])),
                "DOMAIN-KEYWORD" => ("domain", json!([format!("keyword:{payload}")])),
                "DOMAIN-REGEX" => ("domain", json!([format!("regexp:{payload}")])),
                "GEOSITE" => ("domain", json!([format!("geosite:{payload}")])),
                "GEOIP" => ("ip", json!([format!("geoip:{payload}")])),
                "SRC-GEOIP" => ("source", json!([format!("geoip:{payload}")])),
                "IP-CIDR" | "IP-CIDR6" => ("ip", json!([payload])),
                "SRC-IP-CIDR" => ("source", json!([payload])),
                "DST-PORT" => ("port", json!(payload)),
                "SRC-PORT" => ("sourcePort", json!(payload)),
                "NETWORK" => ("network", json!(payload.to_lowercase())),
                _ => bail!("условие «{kind}» требует Mihomo; выберите его для этого JSON"),
            };
            rule[key] = value;
            Ok(rule)
        })();
        result.with_context(|| format!("Правило Mihomo №{} не перенесено; подключение отменено, чтобы не потерять маршрутизацию", index + 1))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xray_keeps_domains_geoip_order_and_conjunctions() {
        let raw = json!({"routing":{"rules":[
            {"domain":["domain:example.org","full:exact.test"],"network":"tcp","outboundTag":"direct"},
            {"ip":["geoip:direct","192.0.2.0/24"],"outboundTag":"direct"},
            {"domain":["geosite:whitelist"],"outboundTag":"direct"},
            {"network":"tcp,udp","balancerTag":"auto"}
        ]}});
        let targets = HashMap::from([
            ("direct".into(), "DIRECT".into()),
            ("auto".into(), "AUTO".into()),
        ]);
        assert_eq!(
            xray_to_mihomo(&raw, &targets).unwrap(),
            [
                "AND,((OR,((DOMAIN-SUFFIX,example.org),(DOMAIN,exact.test))),(NETWORK,TCP)),DIRECT",
                "OR,((GEOIP,direct),(IP-CIDR,192.0.2.0/24)),DIRECT",
                "GEOSITE,whitelist,DIRECT",
                "MATCH,AUTO"
            ]
        );
    }
    #[test]
    fn mihomo_keeps_bypass_and_fallback_targets() {
        let raw = json!({"rules":["DOMAIN-SUFFIX,example.org,DIRECT","GEOSITE,whitelist,DIRECT","GEOIP,direct,DIRECT","MATCH,AUTO"]});
        let rules = mihomo_to_xray(&raw, &HashSet::new(), &HashSet::from(["AUTO".into()])).unwrap();
        assert_eq!(rules[0]["domain"], json!(["domain:example.org"]));
        assert_eq!(rules[1]["domain"], json!(["geosite:whitelist"]));
        assert_eq!(rules[2]["ip"], json!(["geoip:direct"]));
        assert_eq!(rules[2]["outboundTag"], "DIRECT");
        assert_eq!(rules[3]["balancerTag"], "AUTO");
    }
    #[test]
    fn unsupported_conditions_never_silently_broaden_rules() {
        let raw = json!({"routing":{"rules":[{"domain":["domain:example.org"],"protocol":["bittorrent"],"outboundTag":"direct"}]}});
        assert!(
            xray_to_mihomo(&raw, &HashMap::from([("direct".into(), "DIRECT".into())])).is_err()
        );
        assert!(
            mihomo_to_xray(
                &json!({"rules":["GEOIP,direct,DIRECT,no-resolve"]}),
                &HashSet::new(),
                &HashSet::new()
            )
            .is_err()
        );
    }
}
