use crate::mihomo::{self, MihomoInstallation};
use crate::singbox_updater::{self, SingBoxInstallation};
use crate::storage::Paths;
use crate::updater::{self, CoreInstallation};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub const HELPER_PATH: &str = "/usr/lib/nory/nory-helper";
pub const HELPER_SOCKET: &str = "/run/nory/helper.sock";
const AUTHORIZED_DIR: &str = "/var/lib/nory/authorized-users";
const MAX_REQUEST_BYTES: usize = 12 * 1024 * 1024;
const MAX_MIHOMO_CONFIG_BYTES: usize = 8 * 1024 * 1024;
const SING_BOX_CONFIG: &str = "/run/nory/sing-box.json";
const MIHOMO_CONFIG: &str = "/run/nory/mihomo.json";
const MIHOMO_HOME: &str = "/var/cache/nory/mihomo";
const LEGACY_TUN_TABLE: &str = "20220";

#[derive(Debug, Serialize, Deserialize)]
struct HelperResponse {
    ok: bool,
    version: Option<String>,
    error: Option<String>,
}

struct TunRuntime {
    child: Child,
}

struct SystemCoreInstallations {
    xray: CoreInstallation,
    sing_box: SingBoxInstallation,
    mihomo: MihomoInstallation,
}

pub fn helper_available() -> bool {
    Path::new(HELPER_PATH).is_file()
}

pub fn discover_core(user_paths: &Paths) -> Result<Option<CoreInstallation>> {
    if let Some(system) = updater::installed_core(&Paths::system())? {
        return Ok(Some(system));
    }
    updater::installed_core(user_paths)
}

/// Returns the cores bundled with the NORY package. The first call may show
/// polkit once to authorize the current user for the root TUN helper.
pub fn prepare_tun_core() -> Result<CoreInstallation> {
    let paths = Paths::system();
    if !helper_available() {
        bail!("для режима TUN установите системный helper NORY");
    }
    let xray = updater::installed_core(&paths)?.context("в пакете NORY отсутствует Xray")?;
    singbox_updater::installed_sing_box(&paths)?.context("в пакете NORY отсутствует sing-box")?;
    mihomo::installed_mihomo(&paths)?.context("в пакете NORY отсутствует Mihomo")?;
    if send_helper_request("status", Duration::from_secs(3)).is_ok() {
        return Ok(xray);
    }
    setup_system_access()
}

/// Performs the only interactive privilege request. The helper accepts no paths or commands.
pub fn setup_system_access() -> Result<CoreInstallation> {
    if !helper_available() {
        bail!("системный helper NORY не установлен");
    }
    let output = Command::new("/usr/bin/pkexec")
        .args([HELPER_PATH, "setup"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("не удалось запросить одноразовые системные права")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("системная настройка отменена: {}", detail.trim());
    }
    let paths = Paths::system();
    singbox_updater::installed_sing_box(&paths)?.context("в пакете отсутствует sing-box")?;
    mihomo::installed_mihomo(&paths)?.context("в пакете отсутствует Mihomo")?;
    updater::installed_core(&paths)?.context("в пакете отсутствует Xray")
}

pub fn request_system_update() -> Result<CoreInstallation> {
    bail!("ядра обновляются только вместе с новой версией NORY")
}

pub fn configure_tun(
    name: &str,
    enable_ipv6: bool,
    auto_route: bool,
    mtu: u16,
    socks_port: u16,
    bypass_processes: &[String],
) -> Result<String> {
    if !valid_interface_name(name) {
        bail!("некорректное имя TUN-интерфейса");
    }
    let bypass_processes = encode_process_matchers(bypass_processes)?;
    send_helper_request(
        &format!(
            "tun-up {name} {} {} {mtu} {socks_port} {bypass_processes}",
            u8::from(enable_ipv6),
            u8::from(auto_route)
        ),
        Duration::from_secs(15),
    )?;
    Ok(name.into())
}

pub fn cleanup_tun() -> Result<()> {
    send_helper_request("tun-down", Duration::from_secs(10))?;
    Ok(())
}

pub fn configure_mihomo(config: &Value) -> Result<String> {
    let name = config["tun"]["device"].as_str().context("Нет имени TUN")?;
    let bytes = serde_json::to_vec(config)?;
    if bytes.len() > MAX_MIHOMO_CONFIG_BYTES {
        bail!("конфигурация Mihomo превышает 8 МБ");
    }
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    send_helper_request(&format!("mihomo-up {encoded}"), Duration::from_secs(20))?;
    Ok(name.into())
}

fn send_helper_request(request: &str, timeout: Duration) -> Result<HelperResponse> {
    let stream = UnixStream::connect(HELPER_SOCKET)
        .context("системный сервис NORY недоступен; запустите nory-helper.socket")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut writer = stream.try_clone()?;
    writer.write_all(request.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(64 * 1024)
        .read_line(&mut response)?;
    let response: HelperResponse =
        serde_json::from_str(response.trim()).context("некорректный ответ системного helper")?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .as_deref()
                .unwrap_or("операция helper отклонена")
        );
    }
    Ok(response)
}
pub fn run_helper(command: &str) -> Result<()> {
    require_root()?;
    match command {
        "setup" => helper_setup(),
        "serve" => helper_serve(),
        "cleanup" => {
            cleanup_system_tun();
            Ok(())
        }
        _ => bail!("неподдерживаемая команда helper"),
    }
}

fn helper_setup() -> Result<()> {
    let uid = invoking_uid()?;
    let installations = bundled_system_cores()?;
    fs::create_dir_all(AUTHORIZED_DIR)?;
    fs::set_permissions(AUTHORIZED_DIR, fs::Permissions::from_mode(0o711))?;
    let marker = Path::new(AUTHORIZED_DIR).join(uid.to_string());
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&marker)?;
    writeln!(file, "authorized")?;
    file.sync_all()?;
    println!(
        "Встроенные Xray {}, sing-box {} и Mihomo {} готовы; uid {uid} авторизован",
        installations.xray.version, installations.sing_box.version, installations.mihomo.version
    );
    Ok(())
}

fn helper_serve() -> Result<()> {
    let listener = systemd_listener().or_else(|_| bind_fallback_socket())?;
    let mut tun_runtime = None;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => handle_client(stream, &mut tun_runtime),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn handle_client(mut stream: UnixStream, tun_runtime: &mut Option<TunRuntime>) {
    let response = (|| -> Result<Option<String>> {
        let uid = peer_uid(&stream)?;
        verify_authorized(uid)?;
        let mut request = String::new();
        BufReader::new(stream.try_clone()?)
            .take(MAX_REQUEST_BYTES as u64)
            .read_line(&mut request)?;
        let parts = request.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["status"] => {
                let installations = bundled_system_cores()?;
                Ok(Some(format!(
                    "Xray {}; sing-box {}; Mihomo {}",
                    installations.xray.version,
                    installations.sing_box.version,
                    installations.mihomo.version
                )))
            }
            ["tun-up", name, ipv6, auto_route, mtu, socks_port, encoded]
                if valid_interface_name(name) =>
            {
                let mtu = mtu.parse::<u16>().context("некорректный MTU")?;
                let socks_port = socks_port
                    .parse::<u16>()
                    .context("некорректный SOCKS-порт")?;
                let matchers = decode_process_matchers(encoded)?;
                start_sing_box(
                    tun_runtime,
                    name,
                    *ipv6 == "1",
                    *auto_route == "1",
                    mtu,
                    socks_port,
                    &matchers,
                )?;
                Ok(None)
            }
            ["tun-down"] => {
                stop_tun_runtime(tun_runtime);
                cleanup_system_tun();
                Ok(None)
            }
            ["mihomo-up", encoded] => {
                let bytes = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .context("повреждена конфигурация Mihomo")?;
                if bytes.len() > MAX_MIHOMO_CONFIG_BYTES {
                    bail!("конфигурация Mihomo превышает 8 МБ");
                }
                let config: Value =
                    serde_json::from_slice(&bytes).context("некорректная конфигурация Mihomo")?;
                start_mihomo(tun_runtime, &config)?;
                Ok(None)
            }
            _ => bail!("неподдерживаемый запрос"),
        }
    })();
    let payload = match response {
        Ok(version) => HelperResponse {
            ok: true,
            version,
            error: None,
        },
        Err(error) => HelperResponse {
            ok: false,
            version: None,
            error: Some(error.to_string()),
        },
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = writeln!(stream, "{json}");
    }
}

const BYPASS_NFT_TABLE: &str = "nory_bypass";
const LEGACY_KILL_SWITCH_TABLE: &str = "nory_killswitch";

fn valid_interface_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'_' | b'-'))
}

fn encode_process_matchers(matchers: &[String]) -> Result<String> {
    let matchers = validate_process_matchers(matchers.to_vec())?;
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&matchers)?))
}

fn decode_process_matchers(encoded: &str) -> Result<Vec<String>> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("повреждён список процессов обхода")?;
    let matchers: Vec<String> =
        serde_json::from_slice(&bytes).context("некорректный список процессов обхода")?;
    validate_process_matchers(matchers)
}

fn validate_process_matchers(mut matchers: Vec<String>) -> Result<Vec<String>> {
    if matchers.len() > 1024 {
        bail!("слишком много процессов обхода");
    }
    for matcher in &mut matchers {
        *matcher = matcher.trim().replace('\\', "/");
        if matcher.is_empty() || matcher.len() > 4096 || matcher.contains('\0') {
            bail!("некорректный процесс обхода");
        }
    }
    matchers.sort();
    matchers.dedup();
    Ok(matchers)
}

fn start_sing_box(
    runtime: &mut Option<TunRuntime>,
    name: &str,
    enable_ipv6: bool,
    auto_route: bool,
    mtu: u16,
    socks_port: u16,
    matchers: &[String],
) -> Result<()> {
    if !(1280..=9000).contains(&mtu) {
        bail!("MTU должен быть от 1280 до 9000");
    }
    if socks_port < 1024 {
        bail!("SOCKS-порт должен быть не ниже 1024");
    }
    let sing_box = singbox_updater::installed_sing_box(&Paths::system())?
        .context("sing-box не установлен; обновите ядра NORY")?;
    // Versions up to 0.2.15 installed a separate nftables kill switch. Happ
    // relies on sing-box strict routing instead, so remove the legacy table
    // before starting the new TUN session.
    cleanup_legacy_kill_switch();
    stop_tun_runtime(runtime);
    cleanup_tun_routes();
    let config = sing_box_config(name, enable_ipv6, auto_route, mtu, socks_port, matchers);
    write_sing_box_config(&config).context("не удалось записать TUN-конфигурацию")?;
    let check = Command::new(&sing_box.binary)
        .args(["check", "-c", SING_BOX_CONFIG])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("не удалось проверить конфигурацию sing-box")?;
    if !check.status.success() {
        bail!(
            "sing-box отклонил TUN-конфигурацию: {}",
            String::from_utf8_lossy(&check.stderr).trim()
        );
    }
    let mut child = Command::new(&sing_box.binary)
        .args(["run", "-c", SING_BOX_CONFIG, "--disable-color"])
        .current_dir(&sing_box.directory)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("не удалось запустить sing-box")?;
    for _ in 0..60 {
        match child.try_wait() {
            Ok(Some(status)) => {
                cleanup_tun_routes();
                bail!("sing-box завершился при запуске ({status})");
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_tun_routes();
                return Err(error).context("не удалось проверить состояние sing-box");
            }
        }
        if Path::new("/sys/class/net").join(name).exists() {
            *runtime = Some(TunRuntime { child });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    cleanup_tun_routes();
    bail!("TUN-интерфейс {name} не появился после запуска sing-box")
}

fn start_mihomo(runtime: &mut Option<TunRuntime>, config: &Value) -> Result<()> {
    validate_mihomo_config(config)?;
    let mihomo = mihomo::installed_mihomo(&Paths::system())?
        .context("Mihomo не установлен; обновите NORY")?;
    let interface = config
        .get("tun")
        .and_then(|tun| tun.get("device"))
        .and_then(Value::as_str)
        .context("в конфигурации Mihomo отсутствует имя TUN")?;
    // The helper owns exactly one TUN process. Stop and clean the previous
    // backend before writing the new runtime configuration.
    stop_tun_runtime(runtime);
    cleanup_system_tun();
    fs::create_dir_all(MIHOMO_HOME).context("не удалось создать рабочий каталог Mihomo")?;
    fs::set_permissions(MIHOMO_HOME, fs::Permissions::from_mode(0o700))?;
    let uses_ru = config
        .get("rules")
        .and_then(Value::as_array)
        .is_some_and(|rules| {
            rules
                .iter()
                .any(|rule| rule.as_str() == Some("GEOSITE,category-ru,DIRECT"))
        });
    if uses_ru {
        let xray = updater::installed_core(&Paths::system())?
            .context("в пакете NORY отсутствуют GeoData для обхода России")?;
        install_mihomo_geodata(&xray.directory, Path::new(MIHOMO_HOME))?;
    }
    write_root_json(MIHOMO_CONFIG, config).context("не удалось записать конфигурацию Mihomo")?;
    let check = Command::new(&mihomo.binary)
        .args(["-t", "-d", MIHOMO_HOME, "-f", MIHOMO_CONFIG])
        .current_dir(MIHOMO_HOME)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("не удалось проверить конфигурацию Mihomo")?;
    if !check.status.success() {
        bail!(
            "Mihomo отклонил конфигурацию: {}",
            mihomo_check_error(&check.stdout, &check.stderr)
        );
    }

    let mut child = Command::new(&mihomo.binary)
        .args(["-d", MIHOMO_HOME, "-f", MIHOMO_CONFIG])
        .current_dir(MIHOMO_HOME)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("не удалось запустить Mihomo")?;
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(status)) => {
                cleanup_tun_routes();
                bail!("Mihomo завершился при запуске ({status})");
            }
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_tun_routes();
                return Err(error).context("не удалось проверить состояние Mihomo");
            }
        }
        if Path::new("/sys/class/net").join(interface).exists() {
            *runtime = Some(TunRuntime { child });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    cleanup_tun_routes();
    bail!("TUN-интерфейс {interface} не появился после запуска Mihomo")
}

fn install_mihomo_geodata(source: &Path, destination: &Path) -> Result<()> {
    // Only package-owned data, never paths or download URLs from provider JSON.
    // Atomic replacement prevents partial datasets after an interrupted copy.
    for (name, target) in [("geoip.dat", "GeoIP.dat"), ("geosite.dat", "GeoSite.dat")] {
        let input = source.join(name);
        let metadata = fs::metadata(&input).with_context(|| format!("нет встроенного {name}"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
            bail!("некорректный встроенный {name}");
        }
        let temporary = destination.join(format!(".nory-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temporary)?;
            std::io::copy(&mut fs::File::open(&input)?, &mut output)?;
            output.sync_all()?;
            fs::rename(&temporary, destination.join(target))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.with_context(|| format!("не удалось подготовить {name} для Mihomo"))?;
    }
    Ok(())
}

fn mihomo_check_error(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    // Mihomo writes configuration errors to stdout; prefer the actual error
    // over its generic "configuration file test failed" line.
    stdout
        .lines()
        .chain(stderr.lines())
        .find(|line| line.contains("level=error") || line.contains("level=fatal"))
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))
        .or_else(|| stdout.lines().rev().find(|line| !line.trim().is_empty()))
        .unwrap_or("ядро не сообщило подробности ошибки")
        .chars()
        .take(2000)
        .collect()
}

fn validate_mihomo_config(config: &Value) -> Result<()> {
    let object = config
        .as_object()
        .context("конфигурация Mihomo должна быть объектом")?;
    const ALLOWED_TOP_LEVEL: &[&str] = &[
        "mode",
        "log-level",
        "ipv6",
        "allow-lan",
        "find-process-mode",
        "unified-delay",
        "tcp-concurrent",
        "external-controller",
        "profile",
        "dns",
        "tun",
        "hosts",
        "sniffer",
        "ntp",
        "interface-name",
        "routing-mark",
        "geodata-mode",
        "geox-url",
        "geo-auto-update",
        "geo-update-interval",
        "global-client-fingerprint",
        "keep-alive-interval",
        "keep-alive-idle",
        "disable-keep-alive",
        "etag-support",
        "proxies",
        "proxy-groups",
        "proxy-providers",
        "rule-providers",
        "rules",
        "sub-rules",
        "experimental",
    ];
    if let Some(key) = object
        .keys()
        .find(|key| !ALLOWED_TOP_LEVEL.contains(&key.as_str()))
    {
        bail!("запрещённый параметр Mihomo: {key}");
    }
    if object.get("allow-lan").and_then(Value::as_bool) != Some(false) {
        bail!("Mihomo не должен открывать порты в LAN");
    }
    if object
        .get("external-controller")
        .and_then(Value::as_str)
        .is_none_or(|address| !address.starts_with("127.0.0.1:"))
    {
        bail!("API Mihomo должен слушать только localhost");
    }
    let tun = object
        .get("tun")
        .and_then(Value::as_object)
        .context("отсутствует безопасная конфигурация TUN Mihomo")?;
    let interface = tun
        .get("device")
        .and_then(Value::as_str)
        .context("отсутствует имя TUN Mihomo")?;
    if !valid_interface_name(interface)
        || tun.get("enable").and_then(Value::as_bool) != Some(true)
        || tun.get("auto-route").and_then(Value::as_bool) != Some(true)
        || tun.get("strict-route").and_then(Value::as_bool) != Some(true)
    {
        bail!("небезопасная конфигурация TUN Mihomo");
    }
    let has_proxies = object
        .get("proxies")
        .and_then(Value::as_array)
        .is_some_and(|proxies| !proxies.is_empty());
    let has_providers = object
        .get("proxy-providers")
        .and_then(Value::as_object)
        .is_some_and(|providers| !providers.is_empty());
    if !has_proxies && !has_providers {
        bail!("в конфигурации Mihomo нет прокси");
    }
    Ok(())
}

fn cleanup_system_tun() {
    cleanup_legacy_kill_switch();
    cleanup_process_bypass();
    cleanup_tun_routes();
    let _ = fs::remove_file(SING_BOX_CONFIG);
    let _ = fs::remove_file(MIHOMO_CONFIG);
}

fn cleanup_tun_routes() {
    for family in ["-4", "-6"] {
        for priority in [
            "8200", "8201", "8202", "9000", "9001", "9002", "9003", "9010",
        ] {
            for _ in 0..4 {
                let _ = Command::new("/usr/bin/ip")
                    .args([family, "rule", "delete", "priority", priority])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _ = Command::new("/usr/bin/ip")
            .args([family, "route", "flush", "table", LEGACY_TUN_TABLE])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("/usr/bin/ip")
            .args([family, "route", "flush", "table", "2022"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn sing_box_config(
    name: &str,
    enable_ipv6: bool,
    auto_route: bool,
    mtu: u16,
    socks_port: u16,
    matchers: &[String],
) -> Value {
    // NORY must always retain a direct path so subscription refreshes do not
    // recurse through (or get blocked by) the currently active TUN tunnel.
    let mut process_names = BTreeSet::from([
        "nory".to_string(),
        "nory.exe".to_string(),
        "xray".to_string(),
        "xray.exe".to_string(),
        "sing-box".to_string(),
        "sing-box.exe".to_string(),
    ]);
    let mut process_paths = BTreeSet::new();
    for matcher in matchers {
        if matcher.contains('/') {
            if !matcher.ends_with('/') {
                process_paths.insert(matcher.clone());
            }
        } else {
            process_names.insert(matcher.to_string());
        }
    }
    let mut addresses = vec!["172.29.172.1/30"];
    if enable_ipv6 {
        addresses.push("fd17:2917:2::1/126");
    }
    let mut rules = vec![json!({
        "action": "bypass",
        "outbound": "direct",
        "process_name": process_names.into_iter().collect::<Vec<_>>()
    })];
    if !process_paths.is_empty() {
        rules.push(json!({
            "action": "bypass",
            "outbound": "direct",
            "process_path": process_paths.into_iter().collect::<Vec<_>>()
        }));
    }
    rules.push(json!({ "action": "sniff" }));
    rules.push(json!({ "action": "hijack-dns", "protocol": "dns" }));
    json!({
        "log": { "level": "info", "timestamp": true },
        "dns": {
            "servers": [{
                "type": "udp",
                "tag": "dns-direct",
                "server": "8.8.8.8",
                "server_port": 53,
                "detour": "direct"
            }]
        },
        "inbounds": [{
            "type": "tun",
            "tag": "tun-in",
            "interface_name": name,
            "address": addresses,
            "auto_route": auto_route,
            "auto_redirect": auto_route,
            "strict_route": auto_route,
            "stack": "system",
            "mtu": mtu
        }],
        "outbounds": [
            {
                "type": "socks",
                "tag": "proxy",
                "server": "127.0.0.1",
                "server_port": socks_port,
                "domain_resolver": { "server": "dns-direct", "strategy": "prefer_ipv4" }
            },
            {
                "type": "direct",
                "tag": "direct",
                "domain_resolver": { "server": "dns-direct", "strategy": "prefer_ipv4" }
            }
        ],
        "route": {
            "auto_detect_interface": true,
            "final": "proxy",
            "rules": rules
        }
    })
}

fn write_sing_box_config(config: &Value) -> Result<()> {
    write_root_json(SING_BOX_CONFIG, config)
}

fn write_root_json(path: &str, config: &Value) -> Result<()> {
    fs::create_dir_all("/run/nory")?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, config)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn stop_tun_runtime(runtime: &mut Option<TunRuntime>) {
    let Some(mut runtime) = runtime.take() else {
        return;
    };
    // SAFETY: the PID belongs to the child process owned by this helper.
    let _ = unsafe { libc::kill(runtime.child.id() as i32, libc::SIGTERM) };
    for _ in 0..30 {
        if runtime.child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = runtime.child.kill();
    let _ = runtime.child.wait();
}

fn cleanup_legacy_kill_switch() {
    let _ = Command::new("/usr/bin/nft")
        .args(["delete", "table", "inet", LEGACY_KILL_SWITCH_TABLE])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn cleanup_process_bypass() {
    let _ = Command::new("/usr/bin/nft")
        .args(["destroy", "table", "inet", BYPASS_NFT_TABLE])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn bundled_system_cores() -> Result<SystemCoreInstallations> {
    let paths = Paths::system();
    let xray = updater::installed_core(&paths)?.context("в пакете NORY отсутствует Xray")?;
    let sing_box = singbox_updater::installed_sing_box(&paths)?
        .context("в пакете NORY отсутствует sing-box")?;
    let mihomo = mihomo::installed_mihomo(&paths)?.context("в пакете NORY отсутствует Mihomo")?;
    Ok(SystemCoreInstallations {
        xray,
        sing_box,
        mihomo,
    })
}

fn verify_authorized(uid: u32) -> Result<()> {
    let marker = Path::new(AUTHORIZED_DIR).join(uid.to_string());
    let metadata =
        fs::symlink_metadata(&marker).context("пользователь не прошёл одноразовую настройку")?;
    if !metadata.file_type().is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        bail!("повреждён маркер авторизации NORY");
    }
    Ok(())
}

fn systemd_listener() -> Result<UnixListener> {
    let listen_pid: u32 = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let listen_fds: u32 = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if listen_pid != std::process::id() || listen_fds != 1 {
        bail!("нет systemd socket activation");
    }
    // SAFETY: systemd guarantees that the single inherited listening socket is fd 3.
    Ok(unsafe { UnixListener::from_raw_fd(3) })
}

fn bind_fallback_socket() -> Result<UnixListener> {
    fs::create_dir_all("/run/nory")?;
    let socket = PathBuf::from(HELPER_SOCKET);
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: pointers refer to a correctly-sized ucred and socklen_t for the duration of the call.
    let result = unsafe {
        libc::getsockopt(
            std::os::fd::AsRawFd::as_raw_fd(stream),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(credentials.uid)
}

fn invoking_uid() -> Result<u32> {
    std::env::var("PKEXEC_UID")
        .context("helper должен запускаться только через pkexec")?
        .parse()
        .context("некорректный PKEXEC_UID")
}

fn require_root() -> Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        bail!("helper должен работать от root");
    }
    Ok(())
}
