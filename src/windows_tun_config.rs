//! Windows TUN configuration without Win32 calls: the actual broker generator
//! is also tested on Linux against the bundled sing-box runtime.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn valid_interface(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 64 && !name.contains(['\0', '\r', '\n', '/', '\\'])
}

pub(crate) fn sing_box_config(
    name: &str,
    enable_ipv6: bool,
    auto_route: bool,
    mtu: u16,
    socks_port: u16,
    matchers: &[String],
) -> Result<Value> {
    if !valid_interface(name) {
        bail!("Некорректное имя TUN-интерфейса");
    }
    if !(1280..=9000).contains(&mtu) {
        bail!("MTU должен быть от 1280 до 9000");
    }
    if socks_port < 1024 {
        bail!("SOCKS-порт должен быть не ниже 1024");
    }
    let mut names = BTreeSet::from([
        "nory.exe".to_string(),
        "nory-helper.exe".into(),
        "xray.exe".into(),
        "sing-box.exe".into(),
        "mihomo.exe".into(),
    ]);
    let mut paths = BTreeSet::new();
    for matcher in matchers {
        let matcher = matcher.trim().replace('/', "\\");
        if matcher.is_empty() || matcher.contains(['\0', '\r', '\n']) {
            bail!("Некорректное правило обхода приложения");
        }
        if matcher.contains('\\') {
            paths.insert(matcher);
        } else {
            names.insert(matcher);
        }
    }
    let mut addresses = vec!["172.29.172.1/30"];
    if enable_ipv6 {
        addresses.push("fd17:2917:2::1/126");
    }
    // `bypass` is a Linux auto_redirect action, not Windows process routing.
    let mut rules = vec![json!({
        "action": "route", "outbound": "direct", "process_name": names,
    })];
    if !paths.is_empty() {
        rules.push(json!({ "action": "route", "outbound": "direct", "process_path": paths }));
    }
    rules.push(json!({ "action": "sniff" }));
    rules.push(json!({ "action": "hijack-dns", "protocol": "dns" }));
    // Typed DNS servers already use a direct dialer when detour is omitted.
    // sing-box 1.13 rejects detour="direct" when that outbound has no dial
    // options. Keep the direct outbound for process bypass rules, not DNS.
    Ok(json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": { "servers": [{
            "type": "udp", "tag": "dns-direct", "server": "1.1.1.1",
            "server_port": 53
        }]},
        "inbounds": [{
            "type": "tun", "tag": "tun-in", "interface_name": name,
            "address": addresses, "auto_route": auto_route,
            "strict_route": auto_route, "stack": "mixed", "mtu": mtu
        }],
        "outbounds": [
            { "type": "socks", "tag": "proxy", "server": "127.0.0.1",
              "server_port": socks_port,
              "domain_resolver": { "server": "dns-direct", "strategy": "prefer_ipv4" } },
            { "type": "direct", "tag": "direct" }
        ],
        "route": { "auto_detect_interface": true, "rules": rules, "final": "proxy" }
    }))
}
