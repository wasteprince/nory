pub mod app_updater;
pub mod applications;
pub mod config;
pub mod core;
pub mod desktop;
#[cfg(target_os = "windows")]
pub mod flag_assets;
pub mod geodata;
pub mod latency;
pub mod mihomo;
mod routing;
#[cfg(target_os = "linux")]
mod traffic;
pub mod models;
pub mod network;
#[cfg(target_os = "linux")]
pub mod privileged;
#[cfg(target_os = "windows")]
#[path = "privileged_windows.rs"]
pub mod privileged;
pub mod process;
pub mod share;
pub mod singbox_updater;
pub mod storage;
pub mod subscription;
pub mod updater;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
mod windows_service;
#[cfg(target_os = "windows")]
mod windows_tun_config;
#[cfg(target_os = "windows")]
mod windows_tun_state;
