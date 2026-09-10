//! The desktop client never starts a privileged TUN core itself.
use crate::storage::Paths;
use crate::updater::{self, CoreInstallation};
use crate::windows_service::{self, Request};
pub(crate) use crate::windows_tun_config::{sing_box_config, valid_interface};
use anyhow::{Context, Result, bail};
use serde_json::Value;

pub fn helper_available() -> bool {
    windows_service::call(&Request::Ping).is_ok()
}

pub fn discover_core(paths: &Paths) -> Result<Option<CoreInstallation>> {
    updater::installed_core(paths)
}

pub fn prepare_tun_core() -> Result<CoreInstallation> {
    windows_service::call(&Request::Ping)?;
    updater::installed_core(&Paths::discover()?)?.context("Xray не установлен. Переустановите NORY")
}

pub fn setup_system_access() -> Result<CoreInstallation> {
    prepare_tun_core()
}

pub fn request_system_update() -> Result<CoreInstallation> {
    bail!("Системные ядра Windows обновляются вместе с установщиком NORY")
}

pub fn configure_tun(
    name: &str,
    enable_ipv6: bool,
    auto_route: bool,
    mtu: u16,
    socks_port: u16,
    bypass_processes: &[String],
) -> Result<String> {
    // Only Xray + sing-box needs this additional, session-scoped UAC consent.
    // The WebView and Xray itself remain unelevated; the service owns Wintun.
    windows_service::call(&Request::Ping)?;
    if windows_service::call(&Request::CheckXrayAccess).is_err() {
        crate::windows::request_xray_access()?;
        windows_service::call(&Request::CheckXrayAccess)
            .context("Доступ администратора для Xray TUN не подтверждён")?;
    }
    windows_service::start_tunnel(&Request::StartTun {
        name: name.into(),
        enable_ipv6,
        auto_route,
        mtu,
        socks_port,
        bypass_processes: bypass_processes.to_vec(),
    })
}

pub fn configure_mihomo(config: &Value) -> Result<String> {
    windows_service::start_tunnel(&Request::StartMihomo {
        config: config.clone(),
    })
}

pub fn cleanup_tun() -> Result<()> {
    windows_service::call(&Request::Stop)
}

pub(crate) fn tunnel_alive(name: &str) -> Result<bool> {
    if crate::windows::interface_row(name).is_err() {
        return Ok(false);
    }
    windows_service::tunnel_alive(name)
}

pub fn run_helper(command: &str) -> Result<()> {
    if command == "authorize-xray" {
        let pid = std::env::args()
            .nth(2)
            .context("Не указан процесс NORY")?
            .parse::<u32>()
            .context("Некорректный процесс NORY")?;
        return windows_service::authorize_xray(pid);
    }
    if command == "service-install" {
        return windows_service::install();
    }
    if command != "service" {
        bail!("Запускайте NoryTunnel через диспетчер служб Windows");
    }
    windows_service::run()
}
