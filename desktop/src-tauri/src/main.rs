#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(target_os = "linux")]
mod linux_graphics;

use nory::desktop::{Action, Desktop};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

struct Runtime {
    desktop: Arc<Desktop>,
    quitting: AtomicBool,
    ready: AtomicBool,
    show_when_ready: AtomicBool,
    started_at: std::time::Instant,
}

#[tauri::command]
fn ui_ready(app: tauri::AppHandle, runtime: tauri::State<'_, Runtime>) {
    if !runtime.ready.swap(true, Ordering::AcqRel) {
        eprintln!(
            "NORY: интерфейс готов за {} мс",
            runtime.started_at.elapsed().as_millis()
        );
        if runtime.show_when_ready.swap(false, Ordering::AcqRel) {
            show(&app);
        }
    }
}

#[tauri::command]
fn show_update(app: tauri::AppHandle) {
    show(&app);
}

#[tauri::command]
async fn request(
    action: Action,
    runtime: tauri::State<'_, Runtime>,
) -> Result<serde_json::Value, String> {
    let desktop = runtime.desktop.clone();
    tauri::async_runtime::spawn_blocking(move || {
        desktop.handle(action).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn install_update(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Runtime>,
) -> Result<(), String> {
    let desktop = runtime.desktop.clone();
    let emitter = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        desktop
            .install_update(|p| {
                let value = match p {
                    nory::app_updater::AppUpdateProgress::Downloading { received, total } => {
                        serde_json::json!({"received":received,"total":total})
                    }
                    nory::app_updater::AppUpdateProgress::Verifying => {
                        serde_json::json!({"verifying":true})
                    }
                };
                let _ = emitter.emit("update-progress", value);
            })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())??;
    #[cfg(target_os = "windows")]
    {
        runtime.quitting.store(true, Ordering::Release);
        app.exit(0);
    }
    #[cfg(target_os = "linux")]
    {
        nory::app_updater::restart_after_update().map_err(|e| e.to_string())?;
        runtime.quitting.store(true, Ordering::Release);
        app.exit(0);
    }
    Ok(())
}

#[tauri::command]
fn developer_channel() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        nory::windows::open_path(std::path::Path::new("https://t.me/linuxset"))
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg("https://t.me/linuxset")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn show(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let _ = w.emit("window-visibility", true);
    }
}

fn main() {
    let started_at = std::time::Instant::now();
    // SAFETY: first operation in main, before GTK, Tauri or any worker threads.
    #[cfg(target_os = "linux")]
    unsafe {
        linux_graphics::prepare_before_threads();
    }
    #[cfg(target_os = "windows")]
    if let Err(e) = nory::windows::require_windows_11() {
        nory::windows::show_error(&e.to_string());
        return;
    }
    let desktop = match nory::storage::Paths::discover().and_then(Desktop::new) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("NORY: {e:#}");
            return;
        }
    };
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_dialog::init())
        .manage(Runtime {
            desktop: desktop.clone(),
            quitting: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            show_when_ready: AtomicBool::new(false),
            started_at,
        })
        .invoke_handler(tauri::generate_handler![
            request,
            install_update,
            ui_ready,
            show_update,
            developer_channel
        ])
        .setup(move |app| {
            #[cfg(target_os = "linux")]
            if let Some(window) = app.get_webview_window("main") {
                window.with_webview(|webview| {
                    use webkit2gtk::{HardwareAccelerationPolicy, SettingsExt, WebViewExt};
                    if let Some(settings) = webview.inner().settings() {
                        settings
                            .set_hardware_acceleration_policy(HardwareAccelerationPolicy::Always);
                        eprintln!(
                            "NORY: политика GPU-ускорения WebKitGTK: {:?}",
                            settings.hardware_acceleration_policy()
                        );
                    }
                })?;
            }
            let open = MenuItem::with_id(app, "show", "Открыть NORY", true, None::<&str>)?;
            let toggle = MenuItem::with_id(
                app,
                "toggle",
                "Подключиться / отключиться",
                true,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &toggle, &quit])?;
            let icon = tauri::image::Image::from_bytes(include_bytes!(
                "../../../assets/icons/io.nory.NORY-32.png"
            ))?;
            let tray = TrayIconBuilder::with_id("nory")
                .icon(icon)
                .tooltip("NORY — VPN выключен")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show(app),
                    "toggle" => {
                        let app = app.clone();
                        let d = app.state::<Runtime>().desktop.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            let action = if d.can_disconnect() {
                                Action::Disconnect
                            } else {
                                Action::Connect
                            };
                            if let Err(e) = d.handle(action) {
                                let _ = app.emit("operation-error", format!("{e:#}"));
                            }
                        });
                    }
                    "quit" => {
                        let app = app.clone();
                        let d = app.state::<Runtime>().desktop.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            let _ = d.shutdown();
                            app.state::<Runtime>()
                                .quitting
                                .store(true, Ordering::Release);
                            app.exit(0);
                        });
                    }
                    _ => {}
                })
                .build(app);
            let has_tray = tray.is_ok();
            if let Err(e) = tray {
                eprintln!("Трей недоступен: {e}");
            }
            let settings = desktop.settings();
            app.state::<Runtime>()
                .show_when_ready
                .store(!settings.start_minimized || !has_tray, Ordering::Release);
            // Normally Vue reveals the window as soon as local data and DOM are
            // ready. A failed frontend must not leave an invisible, trapped app.
            let fallback = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(4));
                let runtime = fallback.state::<Runtime>();
                if !runtime.ready.load(Ordering::Acquire)
                    && !runtime.quitting.load(Ordering::Acquire)
                    && runtime.show_when_ready.swap(false, Ordering::AcqRel)
                {
                    eprintln!("NORY: ожидание интерфейса превысило 4 с; показываем окно загрузки");
                    show(&fallback);
                }
            });
            let background = desktop.clone();
            let events = app.handle().clone();
            let startup = settings.clone();
            std::thread::spawn(move || {
                if startup.should_auto_ping() {
                    let _ = background.handle(Action::Ping {
                        subscription_id: background.selected_subscription(),
                    });
                    let _ = events.emit("data-changed", ());
                }
                if startup.auto_connect {
                    if let Err(e) = background.handle(Action::Connect) {
                        let _ = events.emit("operation-error", format!("{e:#}"));
                    }
                }
                let mut refreshed = std::collections::HashMap::new();
                let mut last_retry = std::time::Instant::now();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if events.state::<Runtime>().quitting.load(Ordering::Acquire) {
                        break;
                    }
                    if last_retry.elapsed().as_secs()
                        >= u64::from(background.settings().reconnect_delay_seconds.max(1))
                    {
                        let _ = background.retry_connection();
                        last_retry = std::time::Instant::now();
                    }
                    for id in background.due_subscriptions() {
                        if refreshed
                            .get(&id)
                            .is_some_and(|at: &std::time::Instant| at.elapsed().as_secs() < 600)
                        {
                            continue;
                        }
                        refreshed.insert(id, std::time::Instant::now());
                        match background.handle(Action::RefreshSubscription { id }) {
                            Ok(_) => {
                                let _ = events.emit("data-changed", ());
                            }
                            Err(e) => {
                                let _ = events.emit(
                                    "operation-error",
                                    format!("Автообновление подписки: {e:#}"),
                                );
                            }
                        }
                    }
                }
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let mut connected = false;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if handle.state::<Runtime>().quitting.load(Ordering::Acquire) {
                        break;
                    }
                    let now =
                        desktop.core.status().phase == nory::models::ConnectionPhase::Connected;
                    if now != connected {
                        connected = now;
                        if let Some(tray) = handle.tray_by_id("nory") {
                            let bytes = if now {
                                include_bytes!(
                                    "../../../assets/icons/io.nory.NORY-connected-32.png"
                                )
                                .as_slice()
                            } else {
                                include_bytes!("../../../assets/icons/io.nory.NORY-32.png")
                                    .as_slice()
                            };
                            if let Ok(icon) = tauri::image::Image::from_bytes(bytes) {
                                let _ = tray.set_icon(Some(icon));
                            }
                            let _ = tray.set_tooltip(Some(if now {
                                "NORY — VPN включён"
                            } else {
                                "NORY — VPN выключен"
                            }));
                        }
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let runtime = app.state::<Runtime>();
                if runtime.quitting.load(Ordering::Acquire) {
                    return;
                }
                api.prevent_close();
                if runtime.desktop.settings().close_to_tray && app.tray_by_id("nory").is_some() {
                    let _ = window.hide();
                    let _ = window.emit("window-visibility", false);
                } else {
                    let app = app.clone();
                    let d = runtime.desktop.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let _ = d.shutdown();
                        app.state::<Runtime>()
                            .quitting
                            .store(true, Ordering::Release);
                        app.exit(0);
                    });
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("Не удалось запустить интерфейс NORY");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !app.state::<Runtime>().quitting.load(Ordering::Acquire) {
                api.prevent_exit();
                let app = app.clone();
                let d = app.state::<Runtime>().desktop.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let _ = d.shutdown();
                    app.state::<Runtime>()
                        .quitting
                        .store(true, Ordering::Release);
                    app.exit(0);
                });
            }
        }
    });
}
