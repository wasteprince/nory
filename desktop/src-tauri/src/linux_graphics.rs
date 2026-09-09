//! WebKitGTK / NVIDIA Wayland compatibility, applied before GTK starts.
//!
//! https://v2.tauri.app/develop/debug/linux-graphics/
//! https://bugs.webkit.org/show_bug.cgi?id=280210

use std::{env, path::Path};

fn needs_sync_workaround(
    nvidia_loaded: bool,
    wayland_session: bool,
    gdk_backend: Option<&str>,
    explicit_sync_configured: bool,
) -> bool {
    let can_use_wayland = gdk_backend.is_none_or(|backends| {
        backends
            .split(',')
            .any(|b| matches!(b.trim(), "wayland" | "*"))
    });
    nvidia_loaded && wayland_session && can_use_wayland && !explicit_sync_configured
}

/// # Safety
/// Call only at the very beginning of main, before starting any threads or
/// initializing GTK/Tauri. Modifying the environment later is not thread-safe.
pub unsafe fn prepare_before_threads() {
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland");
    // Check the loaded NVIDIA driver, not just the PCI vendor: nouveau and
    // Intel/AMD-only machines should retain their default renderer settings.
    let nvidia = Path::new("/sys/module/nvidia").exists()
        || Path::new("/proc/driver/nvidia/version").exists();
    if needs_sync_workaround(
        nvidia,
        wayland,
        env::var("GDK_BACKEND").ok().as_deref(),
        env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_some(),
    ) {
        // Keep hardware acceleration and DMABUF; only avoid NVIDIA's broken
        // explicit-sync path. Do not change the desktop session, sandbox,
        // drivers, global environment or a user's explicit override.
        unsafe { env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1") };
        eprintln!("NORY: включена совместимость WebKitGTK с NVIDIA/Wayland (explicit sync)");
    }
}
