use adw::prelude::*;
use anyhow::{Context, Result};
use gtk::{Align, Orientation};
use gtk4 as gtk;
#[cfg(target_os = "linux")]
use ksni::blocking::TrayMethods;
use nory::app_updater::{self, AppRelease, AppUpdateProgress};
use nory::applications;
use nory::core::{CoreManager, test_latency};
#[cfg(target_os = "windows")]
use nory::flag_assets::bundled_country_flag;
use nory::models::{
    AppData, ApplicationRule, ApplicationSource, ConnectionMode, ConnectionPhase, ConnectionSpec,
    DomainStrategy, GeoDataKind, GeoDataRule, LogLevel, Profile, Settings, TransportSecurity,
};
use nory::privileged;
use nory::share::parse_many;
use nory::storage::{Paths, load_or_create_hwid, load_state, save_state};
use nory::subscription::{self, FetchResult};
use nory::updater::{self, CoreInstallation};
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use uuid::Uuid;

const APP_CSS: &str = include_str!("../assets/style.css");

/// Offline packaging smoke check: no subscriptions, HWID creation or TUN.
#[cfg(target_os = "windows")]
pub fn check_windows_runtime() -> Result<()> {
    gtk::init().context("GTK не загрузился")?;
    configure_text_rendering();
    let display = gtk::gdk::Display::default().context("нет дисплея")?;
    let theme = gtk::IconTheme::for_display(&display);
    for icon in [
        "go-next-symbolic",
        "window-close-symbolic",
        "dialog-warning-symbolic",
    ] {
        anyhow::ensure!(theme.has_icon(icon), "В комплекте отсутствует {icon}");
    }
    let mut flags = 0;
    for a in b'A'..=b'Z' {
        for b in b'A'..=b'Z' {
            let code = String::from_utf8(vec![a, b])?;
            if let Some(png) = bundled_country_flag(&code) {
                gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_static(png))
                    .with_context(|| format!("Не удалось декодировать флаг {code}"))?;
                flags += 1;
            }
        }
    }
    anyhow::ensure!(flags == 251, "Неполный набор флагов Windows");
    let label = gtk::Label::new(Some("NORY · Серверы · Подключение"));
    anyhow::ensure!(
        label.pango_context().metrics(None, None).height() > 0,
        "Не загрузились шрифты"
    );
    Ok(())
}

#[path = "world_map.rs"]
mod world_map;

const TUN_CONNECTION_ERROR_HINT: &str = "Возможно, сервер недоступен. Попробуйте ещё раз.";

#[derive(Debug, Clone)]
struct CoreReleases {
    app: Option<AppRelease>,
    manual: bool,
}

enum UiEvent {
    CoreChecked(std::result::Result<CoreReleases, String>, bool),
    CoreProgress(AppUpdateProgress),
    CoreInstalled(std::result::Result<std::path::PathBuf, String>),
    UpdateApplied(std::result::Result<(), String>),
    ConnectionChanged(std::result::Result<(), String>),
    ProfileSelected { name: String, changed: bool },
    Latencies(Vec<(Uuid, std::result::Result<u32, String>)>),
    SubscriptionAdded(Uuid, std::result::Result<FetchResult, String>),
    SubscriptionUpdated(Uuid, std::result::Result<FetchResult, String>),
    DataChanged,
    Notice(String),
    RoutingChanged(String),
    Traffic(u64, u64),
    TrayShow,
    TrayToggle,
    TrayQuit,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BannerAction {
    #[default]
    Dismiss,
    UpdateApplication,
    RetrySubscription(Uuid),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ReconnectReason {
    #[default]
    None,
    Routing,
    Profile,
}

#[cfg(target_os = "linux")]
struct NORYTray {
    events: mpsc::Sender<UiEvent>,
    connected: bool,
}

#[derive(Clone, Copy)]
enum LineIcon {
    Globe,
    Add,
    Refresh,
    Settings,
    Logs,
    Bolt,
    Power,
    Pause,
    Star,
    Server,
    ChevronLeft,
    Chevron,
    Check,
    Download,
    Trash,
}

fn line_icon(icon: LineIcon, size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(size);
    area.set_content_height(size);
    area.set_draw_func(move |widget, cr, width, height| {
        cr.set_antialias(gtk::cairo::Antialias::Best);
        let scale = f64::from(size) / 24.0;
        let color = widget.style_context().color();
        cr.translate(
            (f64::from(width) - 24.0 * scale) / 2.0,
            (f64::from(height) - 24.0 * scale) / 2.0,
        );
        cr.scale(scale, scale);
        cr.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            f64::from(color.alpha()),
        );
        cr.set_line_width(1.7);
        if matches!(icon, LineIcon::Power | LineIcon::Pause) {
            cr.set_line_width(2.15);
        }
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        match icon {
            LineIcon::Globe => {
                cr.arc(12.0, 12.0, 9.0, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
                cr.move_to(3.0, 12.0);
                cr.line_to(21.0, 12.0);
                cr.move_to(12.0, 3.0);
                cr.curve_to(16.5, 7.7, 16.5, 16.3, 12.0, 21.0);
                cr.move_to(12.0, 3.0);
                cr.curve_to(7.5, 7.7, 7.5, 16.3, 12.0, 21.0);
            }
            LineIcon::Add => {
                cr.move_to(6.0, 3.0);
                cr.line_to(18.0, 3.0);
                cr.curve_to(20.0, 3.0, 21.0, 4.0, 21.0, 6.0);
                cr.line_to(21.0, 18.0);
                cr.curve_to(21.0, 20.0, 20.0, 21.0, 18.0, 21.0);
                cr.line_to(6.0, 21.0);
                cr.curve_to(4.0, 21.0, 3.0, 20.0, 3.0, 18.0);
                cr.line_to(3.0, 6.0);
                cr.curve_to(3.0, 4.0, 4.0, 3.0, 6.0, 3.0);
                cr.close_path();
                let _ = cr.stroke();
                cr.move_to(8.0, 12.0);
                cr.line_to(16.0, 12.0);
                cr.move_to(12.0, 8.0);
                cr.line_to(12.0, 16.0);
            }
            LineIcon::Refresh => {
                cr.arc(12.0, 12.0, 8.0, -2.55, 0.25);
                cr.move_to(20.0, 5.0);
                cr.line_to(20.0, 10.0);
                cr.line_to(15.0, 10.0);
                let _ = cr.stroke();
                cr.arc(12.0, 12.0, 8.0, 0.59, 3.39);
                cr.move_to(4.0, 19.0);
                cr.line_to(4.0, 14.0);
                cr.line_to(9.0, 14.0);
            }
            LineIcon::Settings => {
                for (y, knob) in [(6.0, 9.0), (12.0, 16.0), (18.0, 7.0)] {
                    cr.move_to(3.0, y);
                    cr.line_to(knob - 2.0, y);
                    cr.move_to(knob + 2.0, y);
                    cr.line_to(21.0, y);
                    cr.move_to(knob, y - 2.0);
                    cr.line_to(knob, y + 2.0);
                }
            }
            LineIcon::Logs => {
                for (y, width) in [(6.0, 15.0), (12.0, 18.0), (18.0, 12.0)] {
                    cr.move_to(3.0, y);
                    cr.line_to(3.0 + width, y);
                }
            }
            LineIcon::Bolt => {
                cr.move_to(14.0, 2.5);
                cr.line_to(5.5, 13.0);
                cr.line_to(11.0, 13.0);
                cr.line_to(9.5, 21.5);
                cr.line_to(18.5, 10.0);
                cr.line_to(13.0, 10.0);
                cr.close_path();
            }
            LineIcon::Power => {
                cr.move_to(12.0, 3.0);
                cr.line_to(12.0, 11.0);
                let _ = cr.stroke();
                cr.arc(12.0, 13.0, 8.0, -0.85, 3.99);
            }
            LineIcon::Pause => {
                cr.move_to(8.5, 6.5);
                cr.line_to(8.5, 17.5);
                cr.move_to(15.5, 6.5);
                cr.line_to(15.5, 17.5);
            }
            LineIcon::Star => {
                cr.move_to(12.0, 3.0);
                cr.line_to(14.8, 8.8);
                cr.line_to(21.0, 9.7);
                cr.line_to(16.5, 14.1);
                cr.line_to(17.6, 20.5);
                cr.line_to(12.0, 17.5);
                cr.line_to(6.4, 20.5);
                cr.line_to(7.5, 14.1);
                cr.line_to(3.0, 9.7);
                cr.line_to(9.2, 8.8);
                cr.close_path();
            }
            LineIcon::Server => {
                for y in [4.0, 14.0] {
                    cr.rectangle(3.0, y, 18.0, 7.0);
                    let _ = cr.stroke();
                    cr.arc(7.0, y + 3.5, 0.8, 0.0, std::f64::consts::TAU);
                    let _ = cr.stroke();
                }
                return;
            }
            LineIcon::Chevron => {
                cr.move_to(9.0, 5.0);
                cr.line_to(16.0, 12.0);
                cr.line_to(9.0, 19.0);
            }
            LineIcon::ChevronLeft => {
                cr.move_to(15.0, 5.0);
                cr.line_to(8.0, 12.0);
                cr.line_to(15.0, 19.0);
            }
            LineIcon::Check => {
                cr.move_to(4.0, 12.0);
                cr.line_to(9.5, 17.5);
                cr.line_to(20.0, 6.5);
            }
            LineIcon::Download => {
                cr.move_to(12.0, 3.0);
                cr.line_to(12.0, 15.0);
                cr.move_to(7.0, 10.0);
                cr.line_to(12.0, 15.0);
                cr.line_to(17.0, 10.0);
                cr.move_to(4.0, 20.0);
                cr.line_to(20.0, 20.0);
            }
            LineIcon::Trash => {
                cr.move_to(5.0, 7.0);
                cr.line_to(19.0, 7.0);
                cr.move_to(9.0, 7.0);
                cr.line_to(9.0, 4.0);
                cr.line_to(15.0, 4.0);
                cr.line_to(15.0, 7.0);
                cr.move_to(7.0, 7.0);
                cr.line_to(8.0, 20.0);
                cr.line_to(16.0, 20.0);
                cr.line_to(17.0, 7.0);
                cr.move_to(10.0, 10.0);
                cr.line_to(10.5, 17.0);
                cr.move_to(14.0, 10.0);
                cr.line_to(13.5, 17.0);
            }
        }
        let _ = cr.stroke();
    });
    area
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn vpn_power_glyph(connected: bool) -> gtk::DrawingArea {
    let glyph = line_icon(
        if connected {
            LineIcon::Pause
        } else {
            LineIcon::Power
        },
        34,
    );
    glyph.add_css_class("power-glyph");
    glyph.set_size_request(34, 34);
    glyph.set_halign(Align::Center);
    glyph.set_valign(Align::Center);
    glyph
}

#[cfg(target_os = "linux")]
fn country_flag(code: &str) -> gtk::Widget {
    let code = code.trim().to_ascii_uppercase();
    let flag = if code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        code.bytes()
            .map(|byte| char::from_u32(0x1F1E6 + u32::from(byte - b'A')).unwrap_or('\u{FFFD}'))
            .collect::<String>()
    } else {
        "🌐".to_string()
    };
    let label = gtk::Label::new(Some(&flag));
    label.add_css_class("country-flag-emoji");
    label.set_halign(Align::Center);
    label.set_valign(Align::Center);
    label.set_tooltip_text(Some(&code));
    label.upcast()
}

#[cfg(target_os = "linux")]
fn location_globe(_size: i32) -> gtk::Widget {
    let label = gtk::Label::new(Some("🌐"));
    label.add_css_class("system-globe-emoji");
    label.set_halign(Align::Center);
    label.set_valign(Align::Center);
    label.upcast()
}

#[cfg(target_os = "windows")]
fn location_globe(size: i32) -> gtk::Widget {
    line_icon(LineIcon::Globe, size).upcast()
}

#[cfg(target_os = "windows")]
fn country_flag(code: &str) -> gtk::Widget {
    let code = code.trim().to_ascii_uppercase();
    if let Some(flag) = bundled_country_flag(&code) {
        let bytes = gtk::glib::Bytes::from_static(flag);
        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
            let picture = gtk::Picture::for_paintable(&texture);
            picture.add_css_class("country-flag-frame");
            picture.set_size_request(28, 19);
            picture.set_can_shrink(true);
            picture.set_halign(Align::Center);
            picture.set_valign(Align::Center);
            picture.set_tooltip_text(Some(&code));
            return picture.upcast();
        }
    }
    location_globe(19)
}

fn world_map_background() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.add_css_class("world-map");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    let dots = world_map::dots();
    area.set_draw_func(move |_, cr, width, height| {
        cr.set_antialias(gtk::cairo::Antialias::Best);
        if width <= 0 || height <= 0 {
            return;
        }
        let (width, height) = (f64::from(width), f64::from(height));
        paint_ambient_background(cr, width, height);
        let world_map::MapRect {
            left,
            top,
            width: map_width,
            height: map_height,
        } = world_map::fit(width, height);
        let ink = gtk::cairo::LinearGradient::new(left, top, left, top + map_height);
        ink.add_color_stop_rgba(0.0, 0.62, 0.68, 0.76, 0.06);
        ink.add_color_stop_rgba(0.4, 0.62, 0.68, 0.76, 0.12);
        ink.add_color_stop_rgba(1.0, 0.62, 0.68, 0.76, 0.04);
        let _ = cr.set_source(&ink);
        let radius = (map_width / 1400.0).clamp(0.65, 1.35);
        for &(x, y) in &dots {
            cr.new_sub_path();
            cr.arc(
                left + x * map_width,
                top + y * map_height,
                radius,
                0.0,
                std::f64::consts::TAU,
            );
        }
        let _ = cr.fill();
    });
    area
}

fn paint_ambient_background(cr: &gtk::cairo::Context, width: f64, height: f64) {
    let base = gtk::cairo::LinearGradient::new(0.0, 0.0, width * 0.45, height);
    base.add_color_stop_rgb(0.0, 0.085, 0.096, 0.112);
    base.add_color_stop_rgb(0.55, 0.055, 0.065, 0.080);
    base.add_color_stop_rgb(1.0, 0.045, 0.050, 0.060);
    let _ = cr.set_source(&base);
    let _ = cr.paint();

    // Soft illumination, not an animated shader or a full-window blur.
    let light = gtk::cairo::RadialGradient::new(
        width * 0.18,
        height * -0.10,
        0.0,
        width * 0.18,
        height * -0.10,
        width.max(height) * 0.85,
    );
    light.add_color_stop_rgba(0.0, 0.58, 0.62, 0.70, 0.18);
    light.add_color_stop_rgba(0.5, 0.38, 0.43, 0.51, 0.065);
    light.add_color_stop_rgba(1.0, 0.30, 0.34, 0.40, 0.0);
    let _ = cr.set_source(&light);
    let _ = cr.paint();

    // A few broad, quiet contour ribbons provide depth in otherwise empty margins.
    let _ = cr.save();
    cr.scale(width / 1200.0, height / 800.0);
    for (offset, opacity) in [(0.0, 0.027), (94.0, 0.022), (188.0, 0.015)] {
        cr.move_to(-150.0, 610.0 + offset);
        cr.curve_to(
            220.0,
            820.0 + offset,
            640.0,
            55.0 + offset,
            1400.0,
            245.0 + offset,
        );
        cr.line_to(1400.0, 295.0 + offset);
        cr.curve_to(
            640.0,
            105.0 + offset,
            220.0,
            870.0 + offset,
            -150.0,
            660.0 + offset,
        );
        cr.close_path();
        cr.set_source_rgba(0.58, 0.64, 0.74, opacity);
        let _ = cr.fill();
        cr.move_to(-150.0, 610.0 + offset);
        cr.curve_to(
            220.0,
            820.0 + offset,
            640.0,
            55.0 + offset,
            1400.0,
            245.0 + offset,
        );
        cr.set_line_width(0.7);
        cr.set_source_rgba(0.64, 0.69, 0.78, opacity * 1.6);
        let _ = cr.stroke();
    }
    let _ = cr.restore();
}

fn app_surface(content: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    let surface = gtk::Overlay::new();
    surface.add_css_class("content-root");
    surface.set_child(Some(&world_map_background()));
    surface.add_overlay(content);
    surface.set_measure_overlay(content, true);
    surface
}

#[cfg(target_os = "linux")]
impl ksni::Tray for NORYTray {
    fn id(&self) -> String {
        "io.nory.NORY".into()
    }

    fn title(&self) -> String {
        if self.connected {
            "NORY — подключён".into()
        } else {
            "NORY — отключён".into()
        }
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![
            ksni::Icon {
                width: 16,
                height: 16,
                data: tray_icon(
                    include_bytes!("../assets/icons/io.nory.NORY-16.argb"),
                    self.connected,
                ),
            },
            ksni::Icon {
                width: 32,
                height: 32,
                data: tray_icon(
                    include_bytes!("../assets/icons/io.nory.NORY-32.argb"),
                    self.connected,
                ),
            },
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(UiEvent::TrayShow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Открыть NORY".into(),
                icon_data: include_bytes!("../assets/tray/open.png").to_vec(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(UiEvent::TrayShow);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.connected {
                    "Отключиться".into()
                } else {
                    "Подключиться".into()
                },
                icon_data: include_bytes!("../assets/tray/vpn.png").to_vec(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(UiEvent::TrayToggle);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Выйти".into(),
                icon_data: include_bytes!("../assets/tray/exit.png").to_vec(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(UiEvent::TrayQuit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
fn tray_icon(source: &[u8], connected: bool) -> Vec<u8> {
    let accent = if connected {
        [0x58, 0xc7, 0x7f]
    } else {
        [0x72, 0x74, 0x7c]
    };
    let mut icon = source.to_vec();
    for pixel in icon.chunks_exact_mut(4) {
        if pixel[0] == 0 {
            continue;
        }
        let is_glyph = pixel[1] < 80 && pixel[2] < 80 && pixel[3] < 80;
        if !is_glyph {
            pixel[1] = accent[0];
            pixel[2] = accent[1];
            pixel[3] = accent[2];
        }
    }
    icon
}

#[cfg(target_os = "linux")]
type PlatformTray = ksni::blocking::Handle<NORYTray>;

#[cfg(target_os = "linux")]
fn spawn_tray(events: mpsc::Sender<UiEvent>) -> Option<PlatformTray> {
    NORYTray {
        events,
        connected: false,
    }
    .disable_dbus_name(true)
    .spawn()
    .ok()
}

#[cfg(target_os = "linux")]
fn update_tray(tray: &PlatformTray, connected: bool) {
    let _ = tray.update(|tray| tray.connected = connected);
}

#[cfg(target_os = "windows")]
struct PlatformTray {
    icon: tray_icon::TrayIcon,
    toggle: tray_icon::menu::MenuItem,
}

#[cfg(target_os = "windows")]
fn spawn_tray(events: mpsc::Sender<UiEvent>) -> Option<PlatformTray> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = Menu::new();
    let open = MenuItem::new("Открыть NORY", true, None);
    let toggle = MenuItem::new("Подключиться", true, None);
    let separator = PredefinedMenuItem::separator();
    let quit = MenuItem::new("Выйти", true, None);
    menu.append_items(&[&open, &toggle, &separator, &quit])
        .ok()?;

    let open_id = open.id().clone();
    let toggle_id = toggle.id().clone();
    let quit_id = quit.id().clone();
    let menu_events = events.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id() == &open_id {
            UiEvent::TrayShow
        } else if event.id() == &toggle_id {
            UiEvent::TrayToggle
        } else if event.id() == &quit_id {
            UiEvent::TrayQuit
        } else {
            return;
        };
        let _ = menu_events.send(action);
    }));
    let tray_events = events;
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        ) {
            let _ = tray_events.send(UiEvent::TrayShow);
        }
    }));

    let icon = TrayIconBuilder::new()
        .with_tooltip("NORY — отключён")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(windows_tray_icon(false).ok()?)
        .build()
        .ok()?;
    Some(PlatformTray { icon, toggle })
}

#[cfg(target_os = "windows")]
fn update_tray(tray: &PlatformTray, connected: bool) {
    tray.toggle.set_text(if connected {
        "Отключиться"
    } else {
        "Подключиться"
    });
    let _ = tray.icon.set_tooltip(Some(if connected {
        "NORY — подключён"
    } else {
        "NORY — отключён"
    }));
    if let Ok(icon) = windows_tray_icon(connected) {
        let _ = tray.icon.set_icon(Some(icon));
    }
}

#[cfg(target_os = "windows")]
fn windows_tray_icon(connected: bool) -> Result<tray_icon::Icon> {
    let accent = if connected {
        [0x58, 0xc7, 0x7f]
    } else {
        [0x72, 0x74, 0x7c]
    };
    let source = include_bytes!("../assets/icons/io.nory.NORY-32.argb");
    let mut rgba = Vec::with_capacity(source.len());
    for pixel in source.chunks_exact(4) {
        let alpha = pixel[0];
        let is_glyph = pixel[1] < 80 && pixel[2] < 80 && pixel[3] < 80;
        let color = if alpha != 0 && !is_glyph {
            accent
        } else {
            [pixel[1], pixel[2], pixel[3]]
        };
        rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
    }
    tray_icon::Icon::from_rgba(rgba, 32, 32)
        .map_err(|error| anyhow::anyhow!("не удалось загрузить иконку трея: {error}"))
}

struct Ui {
    application: adw::Application,
    window: gtk::ApplicationWindow,
    data: Arc<Mutex<AppData>>,
    paths: Paths,
    hwid: String,
    core: Arc<CoreManager>,
    installation: Arc<Mutex<Option<CoreInstallation>>>,
    pending_release: Arc<Mutex<Option<AppRelease>>>,
    prompted_update: RefCell<Option<String>>,
    events: mpsc::Sender<UiEvent>,
    receiver: Mutex<mpsc::Receiver<UiEvent>>,
    tray: Option<PlatformTray>,
    refreshing: Cell<bool>,
    subscription_updates_pending: Cell<usize>,
    last_core_check: Cell<i64>,
    last_traffic_refresh: Cell<i64>,
    last_reconnect_attempt: Cell<i64>,
    last_connection_phase: Cell<ConnectionPhase>,
    last_logs_revision: Cell<u64>,
    last_logs_query: RefCell<String>,
    reconnect_pending: Cell<ReconnectReason>,
    traffic_busy: Arc<AtomicBool>,
    ping_busy: Arc<AtomicBool>,
    update_check_busy: Arc<AtomicBool>,
    toast_overlay: adw::ToastOverlay,
    banner: gtk::Revealer,
    banner_box: gtk::Box,
    banner_title: gtk::Label,
    banner_detail: gtk::Label,
    banner_progress: gtk::ProgressBar,
    banner_button: gtk::Button,
    banner_close_button: gtk::Button,
    banner_action: Cell<BannerAction>,
    power: gtk::Button,
    connection_label: gtk::Label,
    connection_detail: gtk::Label,
    session_server: gtk::Label,
    selected_marker: gtk::Box,
    selected_label: gtk::Label,
    duration_label: gtk::Label,
    download_label: gtk::Label,
    upload_label: gtk::Label,
    home_mode_dropdown: gtk::DropDown,
    home_profiles_grid: gtk::FlowBox,
    home_profiles_scroll: gtk::ScrolledWindow,
    home_ping: gtk::Button,
    home_update: gtk::Button,
    home_delete: gtk::Button,
    home_subscription_previous: gtk::Button,
    home_subscription_next: gtk::Button,
    home_subscription_box: gtk::Box,
    home_subscription_title: gtk::Label,
    home_subscription_detail: gtk::Label,
    home_subscription_notice: gtk::Label,
    subscriptions_list: gtk::ListBox,
    applications_list: gtk::ListBox,
    applications_search: gtk::SearchEntry,
    bypass_domains: gtk::TextView,
    bypass_ru: gtk::Switch,
    geodata_list: gtk::ListBox,
    logs_view: gtk::TextView,
    logs_search: gtk::SearchEntry,
    mode_dropdown: gtk::DropDown,
    socks_port: gtk::SpinButton,
    http_port: gtk::SpinButton,
    api_port: gtk::SpinButton,
    mtu: gtk::SpinButton,
    allow_lan: gtk::Switch,
    enable_ipv6: gtk::Switch,
    sniffing: gtk::Switch,
    sniffing_route_only: gtk::Switch,
    socks_udp: gtk::Switch,
    dns_servers: gtk::Entry,
    domain_strategy: gtk::DropDown,
    mux_enabled: gtk::Switch,
    mux_concurrency: gtk::SpinButton,
    tls_allow_insecure: gtk::Switch,
    tun_interface_name: gtk::Entry,
    tun_auto_route: gtk::Switch,
    auto_connect: gtk::Switch,
    auto_ping: gtk::Switch,
    auto_select_fastest: gtk::Switch,
    ping_timeout: gtk::SpinButton,
    ping_parallelism: gtk::SpinButton,
    traffic_refresh: gtk::SpinButton,
    auto_reconnect: gtk::Switch,
    reconnect_delay: gtk::SpinButton,
    auto_update_subscriptions: gtk::Switch,
    subscription_update_hours: gtk::SpinButton,
    log_level: gtk::DropDown,
    start_minimized: gtk::Switch,
    minimize_on_connect: gtk::Switch,
    start_at_login: gtk::Switch,
    close_to_tray: gtk::Switch,
}

pub fn present(application: &adw::Application) -> Result<()> {
    gtk::Window::set_default_icon_name("io.nory.NORY");
    configure_text_rendering();
    if let Some(window) = application.windows().first() {
        window.present();
        return Ok(());
    }
    let paths = Paths::discover()?;
    let hwid = load_or_create_hwid(&paths)?;
    let mut loaded_data = load_state(&paths)?;
    let previous_rule_count = loaded_data.settings.routing.applications.len();
    loaded_data
        .settings
        .routing
        .applications
        .retain(|application| application.source != ApplicationSource::Steam);
    if loaded_data.settings.routing.applications.len() != previous_rule_count {
        save_state(&paths, &loaded_data)?;
    }
    let data = Arc::new(Mutex::new(loaded_data));
    let installation = Arc::new(Mutex::new(privileged::discover_core(&paths)?));
    let core = Arc::new(CoreManager::new(paths.clone()));
    let application_shutdown_core = Arc::clone(&core);
    application.connect_shutdown(move |_| {
        let _ = application_shutdown_core.disconnect();
        // Also clear a stale helper session left by an earlier abnormal exit.
        let _ = privileged::cleanup_tun();
    });
    let (sender, receiver) = mpsc::channel();
    let tray = spawn_tray(sender.clone());
    let tray_available = tray.is_some();

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("NORY")
        .icon_name("io.nory.NORY")
        .default_width(980)
        .default_height(740)
        .width_request(800)
        .height_request(580)
        .build();
    let shutdown_application = application.clone();
    let shutdown_core = Arc::clone(&core);
    let shutdown_data = Arc::clone(&data);
    window.connect_close_request(move |window| {
        let close_to_tray = shutdown_data
            .lock()
            .is_ok_and(|data| data.settings.close_to_tray);
        if tray_available && close_to_tray {
            window.hide();
            return gtk::glib::Propagation::Stop;
        }
        let _ = shutdown_core.disconnect();
        shutdown_application.quit();
        gtk::glib::Propagation::Proceed
    });
    let header = gtk::HeaderBar::builder().show_title_buttons(true).build();
    header.set_title_widget(Some(&gtk::Label::new(Some("NORY"))));
    window.set_titlebar(Some(&header));

    let toast_overlay = adw::ToastOverlay::new();
    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(180)
        .build();
    let bottom_nav = navigation_bar(&stack);
    // Reserve space for navigation instead of covering the last list rows.
    let layout = gtk::Box::new(Orientation::Vertical, 0);
    layout.append(&stack);
    let navigation_footer = gtk::Box::new(Orientation::Horizontal, 0);
    navigation_footer.add_css_class("navigation-footer");
    navigation_footer.set_halign(Align::Fill);
    bottom_nav.set_hexpand(true);
    navigation_footer.append(&bottom_nav);
    layout.append(&navigation_footer);
    let root = app_surface(&layout);
    toast_overlay.set_child(Some(&root));
    window.set_child(Some(&toast_overlay));

    let banner = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .reveal_child(false)
        .build();
    let banner_box = gtk::Box::new(Orientation::Horizontal, 12);
    banner_box.add_css_class("update-banner");
    let banner_copy = gtk::Box::new(Orientation::Vertical, 2);
    banner_copy.set_hexpand(true);
    let banner_title = label_x("Проверяем обновления NORY…", 0.0);
    banner_title.add_css_class("settings-title");
    let banner_detail = label_x("", 0.0);
    banner_detail.add_css_class("muted");
    banner_detail.set_wrap(true);
    banner_detail.set_max_width_chars(64);
    banner_copy.append(&banner_title);
    banner_copy.append(&banner_detail);
    let banner_progress = gtk::ProgressBar::new();
    banner_progress.set_visible(false);
    banner_progress.set_size_request(150, -1);
    let banner_button = gtk::Button::with_label("Обновить NORY");
    banner_button.set_valign(Align::Center);
    banner_button.add_css_class("primary");
    banner_button.set_visible(false);
    let banner_close_button = gtk::Button::with_label("Закрыть");
    banner_close_button.set_valign(Align::Center);
    banner_close_button.add_css_class("toolbar-button");
    banner_close_button.set_visible(false);
    banner_box.append(&banner_copy);
    banner_box.append(&banner_progress);
    banner_box.append(&banner_close_button);
    banner_box.append(&banner_button);
    banner.set_child(Some(&banner_box));

    let power = gtk::Button::new();
    power.set_child(Some(&vpn_power_glyph(false)));
    power.set_tooltip_text(Some("Включить VPN"));
    power.add_css_class("power");
    power.set_size_request(88, 88);
    power.set_hexpand(false);
    power.set_vexpand(false);
    let connection_label = label_x("Не подключено", 0.5);
    connection_label.add_css_class("hero-title");
    let connection_detail = label_x("Выберите сервер и включите VPN", 0.5);
    connection_detail.add_css_class("muted");
    connection_detail.add_css_class("connection-detail");
    connection_detail.set_wrap(true);
    connection_detail.set_max_width_chars(36);
    let session_server = label_x("Сервер не выбран", 0.0);
    session_server.add_css_class("session-server");
    session_server.set_ellipsize(gtk::pango::EllipsizeMode::End);
    session_server.set_max_width_chars(38);
    let selected_marker = gtk::Box::new(Orientation::Horizontal, 0);
    selected_marker.add_css_class("selected-marker");
    selected_marker.set_halign(Align::Center);
    selected_marker.set_valign(Align::Center);
    let selected_label = label_x("Сервер не выбран", 0.5);
    selected_label.add_css_class("selected-server");
    selected_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    selected_label.set_max_width_chars(38);
    let duration_label = label_x("00:00:00", 0.5);
    duration_label.add_css_class("connection-time");
    let download_label = label_x("0 Б", 0.0);
    download_label.add_css_class("stat-value");
    let upload_label = label_x("0 Б", 0.0);
    upload_label.add_css_class("stat-value");
    let home_mode_dropdown =
        gtk::DropDown::from_strings(&["TUN · Xray (sing-box)", "TUN · Mihomo"]);
    home_mode_dropdown.add_css_class("mode-picker");
    let home_profiles_grid = server_grid();
    let home_ping = compact_icon_button(LineIcon::Bolt);
    home_ping.add_css_class("subscription-tool");
    home_ping.set_tooltip_text(Some("Проверить пинг всех серверов"));
    let home_add = action_button("Подписка", LineIcon::Add);
    home_add.set_tooltip_text(Some("Добавить подписку"));
    let home_update = compact_icon_button(LineIcon::Refresh);
    home_update.add_css_class("subscription-tool");
    home_update.set_tooltip_text(Some("Обновить подписки"));
    let home_delete = compact_icon_button(LineIcon::Trash);
    home_delete.add_css_class("subscription-tool");
    home_delete.add_css_class("danger");
    home_delete.set_tooltip_text(Some("Удалить подписку"));
    let home_subscription_previous = compact_icon_button(LineIcon::ChevronLeft);
    home_subscription_previous.add_css_class("subscription-switch");
    home_subscription_previous.set_tooltip_text(Some("Предыдущая подписка"));
    let home_subscription_next = compact_icon_button(LineIcon::Chevron);
    home_subscription_next.add_css_class("subscription-switch");
    home_subscription_next.set_tooltip_text(Some("Следующая подписка"));
    let home_subscription_box = gtk::Box::new(Orientation::Vertical, 2);
    home_subscription_box.add_css_class("subscription-summary");
    let home_subscription_title = label_x("", 0.0);
    home_subscription_title.add_css_class("settings-title");
    home_subscription_title.set_visible(false);
    home_subscription_title.set_hexpand(true);
    home_subscription_title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let home_subscription_detail = label_x("", 0.0);
    home_subscription_detail.add_css_class("muted");
    let home_subscription_notice = label_x("", 0.0);
    home_subscription_notice.add_css_class("subscription-notice");
    home_subscription_notice.set_wrap(true);
    home_subscription_box.append(&home_subscription_detail);
    home_subscription_box.append(&home_subscription_notice);
    let (home, home_profiles_scroll) = home_page(
        &banner,
        &home_profiles_grid,
        &home_add,
        &home_ping,
        &home_update,
        &home_delete,
        &home_subscription_previous,
        &home_subscription_next,
        &home_subscription_box,
        &home_mode_dropdown,
        &power,
        &connection_label,
        &connection_detail,
        &session_server,
        &selected_marker,
        &selected_label,
        &duration_label,
        &download_label,
        &upload_label,
    );
    stack.add_named(&home, Some("home"));

    let subscriptions_list = gtk::ListBox::new();
    subscriptions_list.set_selection_mode(gtk::SelectionMode::None);

    let applications_list = gtk::ListBox::new();
    applications_list.set_selection_mode(gtk::SelectionMode::None);
    let applications_search = gtk::SearchEntry::builder()
        .placeholder_text("Найти приложение или процесс")
        .build();
    let bypass_domains = gtk::TextView::new();
    let bypass_ru = gtk::Switch::new();
    let geodata_list = gtk::ListBox::new();
    geodata_list.set_selection_mode(gtk::SelectionMode::None);
    let (
        applications_page,
        installed_application,
        add_application,
        add_process,
        save_domains,
        add_geodata,
    ) = applications_page(
        &applications_list,
        &applications_search,
        &bypass_domains,
        &geodata_list,
        &bypass_ru,
    );
    stack.add_named(&page_clamp(&applications_page), Some("applications"));

    let logs_view = gtk::TextView::new();
    let logs_search = gtk::SearchEntry::builder()
        .placeholder_text("Поиск по логам")
        .build();
    let (logs_page, clear_logs, copy_logs) = logs_page(&logs_view, &logs_search);
    stack.add_named(&page_clamp(&logs_page), Some("logs"));

    let mode_dropdown = gtk::DropDown::from_strings(&["TUN · Xray (sing-box)", "TUN · Mihomo"]);
    let socks_port = spin(1024.0, 65535.0);
    let http_port = spin(1024.0, 65535.0);
    let api_port = spin(1024.0, 65535.0);
    let mtu = spin(1280.0, 9000.0);
    let allow_lan = gtk::Switch::new();
    let enable_ipv6 = gtk::Switch::new();
    let sniffing = gtk::Switch::new();
    let sniffing_route_only = gtk::Switch::new();
    let socks_udp = gtk::Switch::new();
    let dns_servers = gtk::Entry::builder()
        .placeholder_text("Системные DNS или 1.1.1.1, 8.8.8.8")
        .build();
    let domain_strategy = gtk::DropDown::from_strings(&["AsIs", "IPIfNonMatch", "IPOnDemand"]);
    let mux_enabled = gtk::Switch::new();
    let mux_concurrency = spin(1.0, 1024.0);
    let tls_allow_insecure = gtk::Switch::new();
    let tun_interface_name = gtk::Entry::builder().max_length(15).build();
    let tun_auto_route = gtk::Switch::new();
    let auto_connect = gtk::Switch::new();
    let auto_ping = gtk::Switch::new();
    let auto_select_fastest = gtk::Switch::new();
    let ping_timeout = spin(1.0, 30.0);
    let ping_parallelism = spin(1.0, 32.0);
    let traffic_refresh = spin(1.0, 10.0);
    let auto_reconnect = gtk::Switch::new();
    let reconnect_delay = spin(2.0, 60.0);
    let auto_update_subscriptions = gtk::Switch::new();
    let subscription_update_hours = spin(1.0, 168.0);
    let log_level =
        gtk::DropDown::from_strings(&["Отключён", "Только ошибки", "Предупреждения", "Подробный"]);
    let start_minimized = gtk::Switch::new();
    let minimize_on_connect = gtk::Switch::new();
    let start_at_login = gtk::Switch::new();
    let close_to_tray = gtk::Switch::new();
    let settings_add_subscription = primary_button("Добавить подписку");
    let (settings_page, save_settings, open_data, check_core, import_profile) = settings_page(
        &mode_dropdown,
        &socks_port,
        &http_port,
        &api_port,
        &mtu,
        &allow_lan,
        &enable_ipv6,
        &sniffing,
        &sniffing_route_only,
        &socks_udp,
        &dns_servers,
        &domain_strategy,
        &mux_enabled,
        &mux_concurrency,
        &tls_allow_insecure,
        &tun_interface_name,
        &tun_auto_route,
        &auto_connect,
        &auto_ping,
        &auto_select_fastest,
        &ping_timeout,
        &ping_parallelism,
        &traffic_refresh,
        &auto_reconnect,
        &reconnect_delay,
        &auto_update_subscriptions,
        &subscription_update_hours,
        &log_level,
        &start_minimized,
        &minimize_on_connect,
        &start_at_login,
        &close_to_tray,
        &subscriptions_list,
        &settings_add_subscription,
    );
    stack.add_named(&page_clamp(&settings_page), Some("settings"));

    stack.set_visible_child_name("home");

    let ui = Rc::new(Ui {
        application: application.clone(),
        window,
        data,
        paths,
        hwid,
        core,
        installation,
        pending_release: Arc::new(Mutex::new(None)),
        prompted_update: RefCell::new(None),
        events: sender,
        receiver: Mutex::new(receiver),
        tray,
        refreshing: Cell::new(false),
        subscription_updates_pending: Cell::new(0),
        last_core_check: Cell::new(0),
        last_traffic_refresh: Cell::new(0),
        last_reconnect_attempt: Cell::new(0),
        last_connection_phase: Cell::new(ConnectionPhase::Disconnected),
        last_logs_revision: Cell::new(u64::MAX),
        last_logs_query: RefCell::new(String::new()),
        reconnect_pending: Cell::new(ReconnectReason::None),
        traffic_busy: Arc::new(AtomicBool::new(false)),
        ping_busy: Arc::new(AtomicBool::new(false)),
        update_check_busy: Arc::new(AtomicBool::new(false)),
        toast_overlay,
        banner,
        banner_box,
        banner_title,
        banner_detail,
        banner_progress,
        banner_button,
        banner_close_button,
        banner_action: Cell::new(BannerAction::Dismiss),
        power,
        connection_label,
        connection_detail,
        session_server,
        selected_marker,
        selected_label,
        duration_label,
        download_label,
        upload_label,
        home_mode_dropdown,
        home_profiles_grid,
        home_profiles_scroll,
        home_ping,
        home_update: home_update.clone(),
        home_delete: home_delete.clone(),
        home_subscription_previous,
        home_subscription_next,
        home_subscription_box,
        home_subscription_title,
        home_subscription_detail,
        home_subscription_notice,
        subscriptions_list,
        applications_list,
        applications_search,
        bypass_domains,
        bypass_ru,
        geodata_list,
        logs_view,
        logs_search,
        mode_dropdown,
        socks_port,
        http_port,
        api_port,
        mtu,
        allow_lan,
        enable_ipv6,
        sniffing,
        sniffing_route_only,
        socks_udp,
        dns_servers,
        domain_strategy,
        mux_enabled,
        mux_concurrency,
        tls_allow_insecure,
        tun_interface_name,
        tun_auto_route,
        auto_connect,
        auto_ping,
        auto_select_fastest,
        ping_timeout,
        ping_parallelism,
        traffic_refresh,
        auto_reconnect,
        reconnect_delay,
        auto_update_subscriptions,
        subscription_update_hours,
        log_level,
        start_minimized,
        minimize_on_connect,
        start_at_login,
        close_to_tray,
    });

    install_css();
    wire_actions(
        &ui,
        home_add,
        settings_add_subscription,
        home_update,
        installed_application,
        add_application,
        add_process,
        save_domains,
        add_geodata,
        clear_logs,
        copy_logs,
        save_settings,
        open_data,
        check_core,
        import_profile,
    );
    ui.refresh();
    ui.start_event_loop();
    ui.check_app_update(false);
    ui.window.present();
    ui.run_startup_automations(tray_available);
    Ok(())
}

impl Ui {
    fn run_startup_automations(self: &Rc<Self>, tray_available: bool) {
        let settings = self
            .data
            .lock()
            .map(|data| data.settings.clone())
            .unwrap_or_default();
        if settings.auto_update_subscriptions {
            self.update_all_subscriptions();
        }
        if settings.should_auto_ping() {
            self.test_all_profiles();
        }
        if settings.auto_connect {
            let weak = Rc::downgrade(self);
            gtk::glib::timeout_add_local_once(Duration::from_millis(700), move || {
                if let Some(ui) = weak.upgrade()
                    && ui.core.status().phase == ConnectionPhase::Disconnected
                {
                    ui.power.emit_clicked();
                }
            });
        }
        let should_start_minimized = settings.start_minimized;
        if should_start_minimized && tray_available {
            let window = self.window.clone();
            gtk::glib::idle_add_local_once(move || window.hide());
        }
    }

    fn test_all_profiles(&self) {
        if self.ping_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        self.home_ping.set_sensitive(false);
        let spinner = gtk::Spinner::new();
        spinner.start();
        self.home_ping.set_child(Some(&spinner));
        let (profiles, timeout, parallelism) = self
            .data
            .lock()
            .map(|data| {
                let subscription = active_subscription_id(&data);
                (
                    data.profiles
                        .iter()
                        .filter(|profile| {
                            subscription.map_or(profile.subscription_id.is_none(), |id| {
                                profile.subscription_id == Some(id)
                            })
                        })
                        .cloned()
                        .collect(),
                    Duration::from_secs(u64::from(data.settings.ping_timeout_seconds)),
                    usize::from(data.settings.ping_parallelism.max(1)),
                )
            })
            .unwrap_or_else(|_| (Vec::new(), Duration::from_secs(3), 8));
        if profiles.is_empty() {
            self.ping_busy.store(false, Ordering::Release);
            self.home_ping.set_sensitive(false);
            self.home_ping
                .set_child(Some(&line_icon(LineIcon::Bolt, 16)));
            return;
        }
        let tx = self.events.clone();
        let started = std::thread::Builder::new()
            .name("nory-latency".into())
            .spawn(move || {
                let worker_count = parallelism.clamp(1, 32).min(profiles.len());
                let queue = Arc::new(Mutex::new(VecDeque::from(profiles)));
                let workers = (0..worker_count)
                    .filter_map(|index| {
                        let queue = Arc::clone(&queue);
                        std::thread::Builder::new()
                            .name(format!("nory-latency-{index}"))
                            .spawn(move || {
                                let mut results = Vec::new();
                                loop {
                                    let profile =
                                        queue.lock().ok().and_then(|mut queue| queue.pop_front());
                                    let Some(profile) = profile else {
                                        break;
                                    };
                                    let id = profile.id;
                                    let result = test_latency(&profile, timeout)
                                        .map_err(|error| error.to_string());
                                    results.push((id, result));
                                }
                                results
                            })
                            .ok()
                    })
                    .collect::<Vec<_>>();
                let results = workers
                    .into_iter()
                    .filter_map(|worker| worker.join().ok())
                    .flatten()
                    .collect();
                let _ = tx.send(UiEvent::Latencies(results));
            });
        if started.is_err() {
            self.ping_busy.store(false, Ordering::Release);
            self.home_ping.set_sensitive(true);
            self.home_ping
                .set_child(Some(&line_icon(LineIcon::Bolt, 16)));
            self.toast_overlay
                .add_toast(adw::Toast::new("Не удалось запустить проверку серверов"));
        }
    }

    fn update_all_subscriptions(&self) {
        if self.subscription_updates_pending.get() != 0 {
            return;
        }
        let ids = self
            .data
            .lock()
            .map(|data| {
                data.subscriptions
                    .iter()
                    .map(|subscription| subscription.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            self.home_update.set_sensitive(true);
            self.home_update
                .set_child(Some(&line_icon(LineIcon::Refresh, 16)));
            self.toast_overlay
                .add_toast(adw::Toast::new("Сначала добавьте подписку"));
            return;
        }
        self.subscription_updates_pending.set(ids.len());
        self.home_update.set_sensitive(false);
        let spinner = gtk::Spinner::new();
        spinner.start();
        self.home_update.set_child(Some(&spinner));
        for id in ids {
            update_subscription_background(
                Arc::clone(&self.data),
                id,
                self.events.clone(),
                false,
                self.hwid.clone(),
            );
        }
    }

    fn update_active_subscription(&self) {
        if self.subscription_updates_pending.get() != 0 {
            return;
        }
        let id = self
            .data
            .lock()
            .ok()
            .and_then(|data| active_subscription_id(&data));
        let Some(id) = id else {
            self.toast_overlay
                .add_toast(adw::Toast::new("Сначала добавьте подписку"));
            return;
        };
        self.subscription_updates_pending.set(1);
        self.home_update.set_sensitive(false);
        let spinner = gtk::Spinner::new();
        spinner.start();
        self.home_update.set_child(Some(&spinner));
        update_subscription_background(
            Arc::clone(&self.data),
            id,
            self.events.clone(),
            false,
            self.hwid.clone(),
        );
    }

    fn refresh(&self) {
        self.refreshing.set(true);
        self.refresh_home();
        self.refresh_subscriptions();
        self.refresh_applications();
        self.refresh_routing_rules();
        self.refresh_logs();
        self.refresh_settings();
        self.refreshing.set(false);
    }

    fn refresh_home(&self) {
        let status = self.core.status();
        let connected = status.phase == ConnectionPhase::Connected;
        let Ok(data) = self.data.lock() else {
            return;
        };
        let active_subscription_id = active_subscription_id(&data);
        let active_subscription = active_subscription_id.and_then(|id| {
            data.subscriptions
                .iter()
                .find(|subscription| subscription.id == id)
                .cloned()
        });
        let selected = data.selected_profile.and_then(|id| {
            data.profiles
                .iter()
                .find(|profile| {
                    profile.id == id
                        && active_subscription_id.is_none_or(|subscription| {
                            profile.subscription_id == Some(subscription)
                        })
                })
                .cloned()
        });
        let profiles_empty = !data.profiles.iter().any(|profile| {
            active_subscription_id.map_or(profile.subscription_id.is_none(), |subscription| {
                profile.subscription_id == Some(subscription)
            })
        });
        let subscriptions_empty = data.subscriptions.is_empty();
        let subscription_title = active_subscription.as_ref().map_or_else(
            || "Локальные серверы".to_string(),
            |subscription| subscription.name.clone(),
        );
        let profile_count = data
            .profiles
            .iter()
            .filter(|profile| {
                active_subscription_id.map_or(profile.subscription_id.is_none(), |subscription| {
                    profile.subscription_id == Some(subscription)
                })
            })
            .count();
        let used = active_subscription.as_ref().map_or(0, |subscription| {
            subscription
                .upload_bytes
                .unwrap_or(0)
                .saturating_add(subscription.download_bytes.unwrap_or(0))
        });
        let has_unlimited = active_subscription
            .as_ref()
            .is_none_or(|subscription| subscription.total_bytes.is_none_or(|total| total == 0));
        let total = active_subscription
            .as_ref()
            .and_then(|subscription| subscription.total_bytes)
            .unwrap_or(0);
        let subscription_count = data.subscriptions.len();
        let subscription_position = active_subscription_id
            .and_then(|id| {
                data.subscriptions
                    .iter()
                    .position(|subscription| subscription.id == id)
            })
            .map_or(0, |position| position + 1);
        let subscription_notices = active_subscription
            .as_ref()
            .and_then(|subscription| subscription.description.clone())
            .unwrap_or_default();
        drop(data);
        self.session_server.set_text(&selected.as_ref().map_or_else(
            || "Сервер не выбран".to_string(),
            |profile| profile_marker_and_name(&profile.name).1,
        ));
        self.session_server
            .set_tooltip_text(selected.as_ref().map(|profile| profile.name.as_str()));
        match status.phase {
            ConnectionPhase::Connected => {
                self.connection_label.set_text("VPN включён");
                self.connection_label.remove_css_class("status-error");
                self.connection_label.add_css_class("status-connected");
                self.connection_detail.set_text(&format!(
                    "{} · {}",
                    status.profile_name.as_deref().unwrap_or("Xray"),
                    status.mode.map_or("", |mode| match mode {
                        ConnectionMode::Tun => "TUN · Xray",
                        ConnectionMode::MihomoTun => "TUN · Mihomo",
                    })
                ));
                self.power.add_css_class("connected");
                self.power.set_child(Some(&vpn_power_glyph(true)));
                self.power.set_tooltip_text(Some("Отключить VPN"));
            }
            ConnectionPhase::Connecting | ConnectionPhase::Disconnecting => {
                self.connection_label
                    .set_text(if status.phase == ConnectionPhase::Connecting {
                        "Подключаемся…"
                    } else {
                        "Отключаемся…"
                    });
                self.power.set_sensitive(false);
            }
            ConnectionPhase::Error => {
                self.connection_label.set_text("Ошибка подключения");
                self.connection_label.add_css_class("status-error");
                if status.mode == Some(ConnectionMode::MihomoTun) {
                    self.connection_detail
                        .set_text(status.error.as_deref().unwrap_or(TUN_CONNECTION_ERROR_HINT));
                } else if status.mode.is_some() {
                    self.connection_detail.set_text(TUN_CONNECTION_ERROR_HINT);
                } else {
                    self.connection_detail
                        .set_text(status.error.as_deref().unwrap_or("Проверьте журнал"));
                }
                self.power.remove_css_class("connected");
                self.power.set_child(Some(&vpn_power_glyph(false)));
                self.power.set_tooltip_text(Some("Включить VPN"));
            }
            ConnectionPhase::Disconnected => {
                self.connection_label.set_text("Не подключено");
                self.connection_label.remove_css_class("status-connected");
                self.connection_label.remove_css_class("status-error");
                self.connection_detail
                    .set_text("Выберите сервер и включите VPN");
                self.power.remove_css_class("connected");
                self.power.set_child(Some(&vpn_power_glyph(false)));
                self.power.set_tooltip_text(Some("Включить VPN"));
                self.download_label.set_text("0 Б");
                self.upload_label.set_text("0 Б");
            }
        }
        self.power.set_sensitive(
            !matches!(
                status.phase,
                ConnectionPhase::Connecting | ConnectionPhase::Disconnecting
            ) && selected.is_some()
                && self.installation.lock().is_ok_and(|core| core.is_some()),
        );
        if let Some(subscription) = active_subscription.as_ref() {
            clear_box(&self.selected_marker);
            self.selected_marker.append(&location_globe(18));
            self.selected_label.set_text(&subscription.name);
            self.selected_label
                .set_tooltip_text(Some(&subscription.name));
        } else if let Some(profile) = selected.as_ref() {
            let (marker, name) = profile_marker_and_name(&profile.name);
            clear_box(&self.selected_marker);
            if let Some(marker) = marker {
                if marker == "🌐" {
                    self.selected_marker.append(&location_globe(18));
                } else {
                    self.selected_marker.append(&country_flag(&marker));
                }
            } else {
                self.selected_marker
                    .append(&line_icon(LineIcon::Server, 18));
            }
            self.selected_label.set_text(&name);
        } else {
            clear_box(&self.selected_marker);
            self.selected_label.set_text("Сервер не выбран");
        }
        self.home_ping
            .set_sensitive(!profiles_empty && !self.ping_busy.load(Ordering::Acquire));
        self.home_delete.set_sensitive(!subscriptions_empty);
        self.home_subscription_previous
            .set_visible(subscription_count > 1);
        self.home_subscription_next
            .set_visible(subscription_count > 1);
        if subscriptions_empty {
            self.home_subscription_box.set_visible(false);
        } else {
            self.home_subscription_box.set_visible(true);
            self.home_subscription_title.set_text(&subscription_title);
            self.home_subscription_title
                .set_tooltip_text(Some(&subscription_title));
            let limit = if has_unlimited {
                "∞".into()
            } else {
                format_bytes(total)
            };
            let traffic = format!(" · {} / {limit}", format_bytes(used));
            let position = if subscription_count > 1 {
                format!("Подписка {subscription_position}/{subscription_count} · ")
            } else {
                String::new()
            };
            self.home_subscription_detail
                .set_text(&format!("{position}{profile_count} серверов{traffic}"));
            self.home_subscription_notice
                .set_text(&subscription_notices);
            self.home_subscription_notice
                .set_visible(!subscription_notices.is_empty());
        }
        if let Some(tray) = &self.tray {
            update_tray(tray, connected);
        }
        self.refresh_home_profiles();
    }

    fn refresh_logs(&self) {
        let query = self.logs_search.text().trim().to_lowercase();
        let revision = self.core.logs_revision();
        if revision == self.last_logs_revision.get()
            && self.last_logs_query.borrow().as_str() == query
        {
            return;
        }
        let lines = self
            .core
            .logs()
            .into_iter()
            .filter(|entry| {
                query.is_empty()
                    || entry.level.to_lowercase().contains(&query)
                    || entry.message.to_lowercase().contains(&query)
            })
            .map(|entry| {
                format!(
                    "[{}] {:<7} {}",
                    format_time(entry.at),
                    entry.level.to_uppercase(),
                    entry.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = if lines.is_empty() {
            if query.is_empty() {
                "Лог пока пуст. Записи Xray появятся после подключения.".to_string()
            } else {
                "По этому запросу ничего не найдено.".to_string()
            }
        } else {
            lines
        };
        let buffer = self.logs_view.buffer();
        let current = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        if current.as_str() != text {
            buffer.set_text(&text);
        }
        self.last_logs_revision.set(revision);
        self.last_logs_query.replace(query);
    }

    fn refresh_home_profiles(&self) {
        let scroll_position = self.home_profiles_scroll.vadjustment().value();
        let (profiles, selected_profile) = {
            let Ok(data) = self.data.lock() else {
                return;
            };
            let profiles = data
                .profiles
                .iter()
                .filter(|profile| {
                    let subscription = active_subscription_id(&data);
                    subscription.map_or(profile.subscription_id.is_none(), |id| {
                        profile.subscription_id == Some(id)
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            (profiles, data.selected_profile)
        };
        while let Some(child) = self.home_profiles_grid.first_child() {
            self.home_profiles_grid.remove(&child);
        }
        if profiles.is_empty() {
            let empty = gtk::Box::new(Orientation::Vertical, 6);
            empty.add_css_class("server-empty");
            empty.append(&label_x("Серверов пока нет", 0.0));
            let hint = label_x("Добавьте подписку", 0.0);
            hint.add_css_class("muted");
            empty.append(&hint);
            self.home_profiles_grid.insert(&empty, -1);
            return;
        }
        for profile in profiles {
            self.home_profiles_grid.insert(
                &self.home_profile_card(&profile, selected_profile == Some(profile.id)),
                -1,
            );
        }
        let adjustment = self.home_profiles_scroll.vadjustment();
        gtk::glib::idle_add_local_once(move || {
            let maximum = (adjustment.upper() - adjustment.page_size()).max(0.0);
            adjustment.set_value(scroll_position.clamp(0.0, maximum));
        });
    }

    fn home_profile_card(&self, profile: &Profile, selected: bool) -> gtk::Button {
        let button = server_card(profile, selected);
        let (_, display_name) = profile_marker_and_name(&profile.name);
        let id = profile.id;
        let data = Arc::clone(&self.data);
        let paths = self.paths.clone();
        let tx = self.events.clone();
        let selected_name = display_name;
        button.connect_clicked(move |_| {
            let mut changed = false;
            if let Ok(mut data) = data.lock() {
                changed = data.selected_profile != Some(id);
                if changed {
                    data.selected_profile = Some(id);
                    let _ = save_state(&paths, &data);
                }
            }
            let _ = tx.send(UiEvent::ProfileSelected {
                name: selected_name.clone(),
                changed,
            });
        });
        button
    }

    fn refresh_subscriptions(&self) {
        clear_list(&self.subscriptions_list);
        let subscriptions = {
            let Ok(data) = self.data.lock() else {
                return;
            };
            data.subscriptions
                .iter()
                .cloned()
                .map(|subscription| {
                    let count = data
                        .profiles
                        .iter()
                        .filter(|profile| profile.subscription_id == Some(subscription.id))
                        .count();
                    (subscription, count)
                })
                .collect::<Vec<_>>()
        };
        if subscriptions.is_empty() {
            self.subscriptions_list.append(&empty_row(
                "Подписок пока нет",
                "Добавьте URL подписки для автоматического обновления серверов",
            ));
            return;
        }
        for (subscription, count) in subscriptions {
            let row = gtk::ListBoxRow::new();
            let content = gtk::Box::new(Orientation::Horizontal, 12);
            content.add_css_class("server-row");
            let icon = line_icon(LineIcon::Download, 19);
            let copy = gtk::Box::new(Orientation::Vertical, 2);
            copy.set_hexpand(true);
            copy.append(&label_x(&subscription.name, 0.0));
            let detail = label_x(
                &format!(
                    "{count} серверов · {}{}",
                    subscription
                        .updated_at
                        .map_or("ещё не обновлялась".into(), |time| {
                            format_time(time)
                        }),
                    subscription
                        .provider_update_interval_hours
                        .map_or_else(String::new, |hours| format!(
                            " · интервал провайдера {hours} ч"
                        ))
                ),
                0.0,
            );
            detail.add_css_class("muted");
            copy.append(&detail);
            let update = gtk::Button::new();
            update.set_child(Some(&line_icon(LineIcon::Refresh, 17)));
            update.set_tooltip_text(Some("Обновить"));
            let id = subscription.id;
            let data = Arc::clone(&self.data);
            let tx = self.events.clone();
            let hwid = self.hwid.clone();
            update.connect_clicked(move |button| {
                button.set_sensitive(false);
                let spinner = gtk::Spinner::new();
                spinner.start();
                button.set_child(Some(&spinner));
                update_subscription_background(
                    Arc::clone(&data),
                    id,
                    tx.clone(),
                    false,
                    hwid.clone(),
                );
            });
            let hwid_label = label_x("HWID", 1.0);
            hwid_label.add_css_class("muted");
            let send_hwid = gtk::Switch::new();
            send_hwid.set_active(subscription.send_hwid);
            send_hwid.set_valign(Align::Center);
            send_hwid.set_tooltip_text(Some("Передавать X-HWID этой подписке"));
            let id = subscription.id;
            let state = Arc::clone(&self.data);
            let paths = self.paths.clone();
            let tx = self.events.clone();
            send_hwid.connect_active_notify(move |switch| {
                if let Ok(mut data) = state.lock() {
                    if let Some(subscription) = data
                        .subscriptions
                        .iter_mut()
                        .find(|subscription| subscription.id == id)
                    {
                        subscription.send_hwid = switch.is_active();
                    }
                    let _ = save_state(&paths, &data);
                }
                let status = if switch.is_active() {
                    "Передача HWID включена"
                } else {
                    "Передача HWID выключена"
                };
                let _ = tx.send(UiEvent::Notice(status.into()));
            });
            let delete = gtk::Button::new();
            delete.set_child(Some(&line_icon(LineIcon::Trash, 17)));
            delete.set_tooltip_text(Some("Удалить подписку"));
            delete.add_css_class("danger");
            let id = subscription.id;
            let name = subscription.name.clone();
            let state = Arc::clone(&self.data);
            let paths = self.paths.clone();
            let tx = self.events.clone();
            let window = self.window.clone();
            delete.connect_clicked(move |_| {
                let dialog = gtk::MessageDialog::builder()
                    .transient_for(&window)
                    .modal(true)
                    .message_type(gtk::MessageType::Question)
                    .buttons(gtk::ButtonsType::YesNo)
                    .text(format!("Удалить подписку «{name}»?"))
                    .secondary_text("Серверы этой подписки также будут удалены.")
                    .build();
                let state = Arc::clone(&state);
                let paths = paths.clone();
                let tx = tx.clone();
                dialog.connect_response(move |dialog, response| {
                    if response == gtk::ResponseType::Yes
                        && let Ok(mut data) = state.lock()
                    {
                        let old_position = data
                            .subscriptions
                            .iter()
                            .position(|subscription| subscription.id == id)
                            .unwrap_or(0);
                        data.subscriptions
                            .retain(|subscription| subscription.id != id);
                        data.profiles
                            .retain(|profile| profile.subscription_id != Some(id));
                        if data.selected_subscription == Some(id) {
                            data.selected_subscription = if data.subscriptions.is_empty() {
                                None
                            } else {
                                Some(
                                    data.subscriptions
                                        [old_position.min(data.subscriptions.len() - 1)]
                                    .id,
                                )
                            };
                        }
                        data.selected_profile =
                            data.selected_subscription.and_then(|subscription| {
                                data.profiles
                                    .iter()
                                    .find(|profile| profile.subscription_id == Some(subscription))
                                    .map(|profile| profile.id)
                            });
                        let _ = save_state(&paths, &data);
                        let _ = tx.send(UiEvent::DataChanged);
                        let _ = tx.send(UiEvent::Notice("Подписка удалена".into()));
                    }
                    dialog.close();
                });
                dialog.present();
            });
            content.append(&icon);
            content.append(&copy);
            content.append(&hwid_label);
            content.append(&send_hwid);
            content.append(&update);
            content.append(&delete);
            row.set_child(Some(&content));
            self.subscriptions_list.append(&row);
        }
    }

    fn refresh_applications(&self) {
        clear_list(&self.applications_list);
        let query = self.applications_search.text().trim().to_lowercase();
        let Ok(data) = self.data.lock() else {
            return;
        };
        let mut applications: Vec<_> = data
            .settings
            .routing
            .applications
            .iter()
            .filter(|application| {
                query.is_empty()
                    || application.name.to_lowercase().contains(&query)
                    || application
                        .processes
                        .iter()
                        .any(|process| process.to_lowercase().contains(&query))
            })
            .cloned()
            .collect();
        drop(data);
        applications.sort_by_key(|application| application.name.to_lowercase());
        if applications.is_empty() {
            self.applications_list.append(&empty_row(
                if query.is_empty() {
                    "Правил обхода пока нет"
                } else {
                    "Ничего не найдено"
                },
                if query.is_empty() {
                    "Выберите приложение из списка или добавьте запущенный процесс"
                } else {
                    "Измените поисковый запрос"
                },
            ));
            return;
        }
        for application in applications {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            let content = gtk::Box::new(Orientation::Horizontal, 10);
            content.add_css_class("server-row");
            let icon = line_icon(LineIcon::Server, 19);
            let copy = gtk::Box::new(Orientation::Vertical, 1);
            copy.set_hexpand(true);
            let name = label_x(&application.name, 0.0);
            name.add_css_class("settings-title");
            let detail = label_x(
                &format!(
                    "Приложение / процесс · {}",
                    application.processes.join(", ")
                ),
                0.0,
            );
            detail.add_css_class("muted");
            detail.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            copy.append(&name);
            copy.append(&detail);
            let bypass = gtk::Switch::new();
            bypass.set_tooltip_text(Some("Обход VPN"));
            bypass.set_active(application.bypass);
            bypass.set_valign(Align::Center);
            let id = application.id.clone();
            let state = Arc::clone(&self.data);
            let paths = self.paths.clone();
            let tx = self.events.clone();
            bypass.connect_active_notify(move |switch| {
                if let Ok(mut data) = state.lock() {
                    if let Some(application) = data
                        .settings
                        .routing
                        .applications
                        .iter_mut()
                        .find(|application| application.id == id)
                    {
                        application.bypass = switch.is_active();
                    }
                    let _ = save_state(&paths, &data);
                }
                let message = if switch.is_active() {
                    "Обход VPN включён"
                } else {
                    "Обход VPN выключен"
                };
                let _ = tx.send(UiEvent::RoutingChanged(message.into()));
            });
            content.append(&icon);
            content.append(&copy);
            content.append(&bypass);
            if application.source == ApplicationSource::Manual {
                let delete = gtk::Button::new();
                delete.set_child(Some(&line_icon(LineIcon::Trash, 16)));
                delete.add_css_class("flat");
                delete.set_tooltip_text(Some("Удалить правило"));
                let id = application.id.clone();
                let state = Arc::clone(&self.data);
                let paths = self.paths.clone();
                let tx = self.events.clone();
                delete.connect_clicked(move |_| {
                    if let Ok(mut data) = state.lock() {
                        data.settings
                            .routing
                            .applications
                            .retain(|application| application.id != id);
                        let _ = save_state(&paths, &data);
                    }
                    let _ = tx.send(UiEvent::DataChanged);
                    let _ = tx.send(UiEvent::RoutingChanged("Правило обхода удалено".into()));
                });
                content.append(&delete);
            }
            row.set_child(Some(&content));
            self.applications_list.append(&row);
        }
    }

    fn refresh_routing_rules(&self) {
        clear_list(&self.geodata_list);
        let Ok(data) = self.data.lock() else {
            return;
        };
        let domains = data.settings.routing.bypass_domains.join("\n");
        self.bypass_domains.buffer().set_text(&domains);
        let rules = data.settings.routing.bypass_geodata.clone();
        let bypass_ru = data.settings.routing.bypass_ru;
        drop(data);
        let was_refreshing = self.refreshing.replace(true);
        self.bypass_ru.set_active(bypass_ru);
        self.refreshing.set(was_refreshing);
        if rules.is_empty() {
            self.geodata_list.append(&empty_row(
                "GeoData-файлы не добавлены",
                "Добавьте .dat и укажите его тип и теги",
            ));
            return;
        }
        for rule in rules {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            let content = gtk::Box::new(Orientation::Horizontal, 10);
            content.add_css_class("server-row");
            let copy = gtk::Box::new(Orientation::Vertical, 2);
            copy.set_hexpand(true);
            let title = label_x(&rule.display_name, 0.0);
            title.add_css_class("settings-title");
            let kind = match rule.kind {
                GeoDataKind::GeoSite => "GeoSite · домены",
                GeoDataKind::GeoIp => "GeoIP · IP-диапазоны",
            };
            let detail = label_x(&format!("{kind} · {}", rule.tags.join(", ")), 0.0);
            detail.add_css_class("muted");
            detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
            copy.append(&title);
            copy.append(&detail);
            let delete = gtk::Button::new();
            delete.set_child(Some(&line_icon(LineIcon::Trash, 16)));
            delete.add_css_class("flat");
            delete.add_css_class("danger");
            delete.set_tooltip_text(Some("Удалить GeoData-правило"));
            let id = rule.id.clone();
            let state = Arc::clone(&self.data);
            let paths = self.paths.clone();
            let tx = self.events.clone();
            delete.connect_clicked(move |_| {
                let removed = if let Ok(mut data) = state.lock() {
                    let removed = data
                        .settings
                        .routing
                        .bypass_geodata
                        .iter()
                        .find(|item| item.id == id)
                        .cloned();
                    data.settings
                        .routing
                        .bypass_geodata
                        .retain(|item| item.id != id);
                    let _ = save_state(&paths, &data);
                    removed
                } else {
                    None
                };
                if let Some(removed) = removed
                    && let Some(path) = managed_geodata_path(&paths, &removed.file_name)
                {
                    let _ = fs::remove_file(path);
                }
                let _ = tx.send(UiEvent::DataChanged);
                let _ = tx.send(UiEvent::RoutingChanged("GeoData-правило удалено".into()));
            });
            content.append(&line_icon(LineIcon::Globe, 19));
            content.append(&copy);
            content.append(&delete);
            row.set_child(Some(&content));
            self.geodata_list.append(&row);
        }
    }

    fn refresh_settings(&self) {
        let Ok(data) = self.data.lock() else {
            return;
        };
        let s = data.settings.clone();
        drop(data);
        let mode = if s.mode == ConnectionMode::MihomoTun {
            1
        } else {
            0
        };
        self.mode_dropdown.set_selected(mode);
        self.home_mode_dropdown.set_selected(mode);
        self.socks_port.set_value(s.socks_port as f64);
        self.http_port.set_value(s.http_port as f64);
        self.api_port.set_value(s.api_port as f64);
        self.mtu.set_value(s.mtu as f64);
        self.allow_lan.set_active(s.allow_lan);
        self.enable_ipv6.set_active(s.enable_ipv6);
        self.sniffing.set_active(s.sniffing);
        self.sniffing_route_only.set_active(s.sniffing_route_only);
        self.socks_udp.set_active(s.socks_udp);
        self.dns_servers.set_text(&s.dns_servers);
        self.domain_strategy.set_selected(match s.domain_strategy {
            DomainStrategy::AsIs => 0,
            DomainStrategy::IpIfNonMatch => 1,
            DomainStrategy::IpOnDemand => 2,
        });
        self.mux_enabled.set_active(s.mux_enabled);
        self.mux_concurrency.set_value(f64::from(s.mux_concurrency));
        self.tls_allow_insecure.set_active(s.tls_allow_insecure);
        self.tun_interface_name.set_text(&s.tun_interface_name);
        self.tun_auto_route.set_active(true);
        self.tun_auto_route.set_sensitive(false);
        self.auto_connect.set_active(s.auto_connect);
        self.auto_ping.set_active(s.auto_ping);
        self.auto_select_fastest.set_active(s.auto_select_fastest);
        self.ping_timeout
            .set_value(f64::from(s.ping_timeout_seconds));
        self.ping_parallelism
            .set_value(f64::from(s.ping_parallelism));
        self.traffic_refresh
            .set_value(f64::from(s.traffic_refresh_seconds));
        self.auto_reconnect.set_active(s.auto_reconnect);
        self.reconnect_delay
            .set_value(f64::from(s.reconnect_delay_seconds));
        self.auto_update_subscriptions
            .set_active(s.auto_update_subscriptions);
        self.subscription_update_hours
            .set_value(f64::from(s.subscription_update_interval_hours));
        self.log_level.set_selected(match s.log_level {
            LogLevel::None => 0,
            LogLevel::Error => 1,
            LogLevel::Warning => 2,
            LogLevel::Info => 3,
        });
        self.start_minimized.set_active(s.start_minimized);
        self.minimize_on_connect.set_active(s.minimize_on_connect);
        self.start_at_login.set_active(s.start_at_login);
        self.close_to_tray.set_active(s.close_to_tray);
    }

    fn start_event_loop(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            for _ in 0..64 {
                let event = ui
                    .receiver
                    .lock()
                    .ok()
                    .and_then(|receiver| receiver.try_recv().ok());
                let Some(event) = event else {
                    break;
                };
                ui.handle_event(event);
            }
            gtk::glib::ControlFlow::Continue
        });
        let ui = Rc::clone(self);
        gtk::glib::timeout_add_local(Duration::from_secs(1), move || {
            let status = ui.core.status();
            ui.refresh_logs();
            if ui.last_connection_phase.replace(status.phase) != status.phase {
                ui.refresh_home();
            }
            if let Some(started) = status.started_at {
                let seconds = started.elapsed().as_secs();
                ui.duration_label.set_text(&format!(
                    "{:02}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                ));
            } else {
                ui.duration_label.set_text("00:00:00");
            }
            let now = unix_now();
            let (traffic_interval, auto_reconnect, reconnect_delay) = ui
                .data
                .lock()
                .map(|data| {
                    (
                        i64::from(data.settings.traffic_refresh_seconds.max(1)),
                        data.settings.auto_reconnect,
                        i64::from(data.settings.reconnect_delay_seconds.max(2)),
                    )
                })
                .unwrap_or((1, false, 5));
            if status.phase == ConnectionPhase::Connected
                && now.saturating_sub(ui.last_traffic_refresh.get()) >= traffic_interval
                && !ui.traffic_busy.swap(true, Ordering::AcqRel)
            {
                ui.last_traffic_refresh.set(now);
                let core = Arc::clone(&ui.core);
                let tx = ui.events.clone();
                let busy = Arc::clone(&ui.traffic_busy);
                std::thread::spawn(move || {
                    if let Ok((up, down)) = core.traffic() {
                        let _ = tx.send(UiEvent::Traffic(up, down));
                    }
                    busy.store(false, Ordering::Release);
                });
            }
            if status.phase == ConnectionPhase::Error
                && auto_reconnect
                && now.saturating_sub(ui.last_reconnect_attempt.get()) >= reconnect_delay
            {
                ui.last_reconnect_attempt.set(now);
                ui.power.emit_clicked();
            }
            gtk::glib::ControlFlow::Continue
        });
        let ui = Rc::clone(self);
        gtk::glib::timeout_add_local(Duration::from_secs(60), move || {
            let due = ui.data.lock().is_ok_and(|data| {
                if !data.settings.auto_update_subscriptions || data.subscriptions.is_empty() {
                    return false;
                }
                data.subscriptions.iter().any(|subscription| {
                    let interval = i64::from(
                        subscription
                            .provider_update_interval_hours
                            .unwrap_or(data.settings.subscription_update_interval_hours),
                    ) * 3600;
                    subscription
                        .updated_at
                        .is_none_or(|updated| unix_now().saturating_sub(updated) >= interval)
                })
            });
            if due {
                ui.update_all_subscriptions();
            }
            let core_due = unix_now().saturating_sub(ui.last_core_check.get()) >= 10 * 60;
            if core_due {
                ui.check_app_update(false);
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    fn handle_event(&self, event: UiEvent) {
        match event {
            UiEvent::CoreChecked(Ok(releases), _) => {
                self.update_check_busy.store(false, Ordering::Release);
                if let Some(release) = releases.app {
                    if let Ok(mut pending_release) = self.pending_release.lock() {
                        *pending_release = Some(release.clone());
                    }
                    self.banner_action.set(BannerAction::UpdateApplication);
                    let already_prompted = self
                        .prompted_update
                        .borrow()
                        .as_deref()
                        .is_some_and(|version| version == release.version);
                    if releases.manual || !already_prompted {
                        self.prompted_update.replace(Some(release.version.clone()));
                        self.show_update_prompt(&release);
                    }
                } else {
                    if let Ok(mut pending_release) = self.pending_release.lock() {
                        *pending_release = None;
                    }
                    if self.banner_action.get() == BannerAction::UpdateApplication {
                        self.hide_banner();
                    }
                    if releases.manual {
                        self.toast_overlay
                            .add_toast(adw::Toast::new("Установлена последняя версия NORY"));
                    }
                }
            }
            UiEvent::CoreChecked(Err(error), true) => {
                self.update_check_busy.store(false, Ordering::Release);
                self.show_banner_error_with_action(
                    "Не удалось проверить обновление NORY",
                    &error,
                    BannerAction::UpdateApplication,
                );
            }
            UiEvent::CoreChecked(Err(_), false) => {
                self.update_check_busy.store(false, Ordering::Release);
            }
            UiEvent::CoreProgress(progress) => self.show_progress(progress),
            UiEvent::CoreInstalled(Ok(installer)) => {
                self.banner_progress.set_fraction(1.0);
                self.banner_title.set_text("Обновление проверено");
                self.banner_detail
                    .set_text("Запускаем системную установку новой версии NORY");
                self.banner_box.add_css_class("success-banner");
                #[cfg(target_os = "linux")]
                let _ = self.core.disconnect();
                self.banner_title.set_text("Устанавливаем обновление NORY");
                self.banner_detail
                    .set_text("Подтвердите системный запрос и дождитесь завершения установки");
                self.banner_progress.pulse();
                let tx = self.events.clone();
                std::thread::spawn(move || {
                    let result =
                        app_updater::launch_update(&installer).map_err(|error| error.to_string());
                    let _ = tx.send(UiEvent::UpdateApplied(result));
                });
            }
            UiEvent::CoreInstalled(Err(error)) => self.show_banner_error_with_action(
                "Не удалось загрузить обновление NORY",
                &error,
                BannerAction::UpdateApplication,
            ),
            UiEvent::UpdateApplied(Ok(())) => {
                let _ = self.core.disconnect();
                if let Err(error) = app_updater::restart_after_update() {
                    self.show_banner_error_with_action(
                        "NORY обновлён, но не перезапущен",
                        &error.to_string(),
                        BannerAction::Dismiss,
                    );
                }
            }
            UiEvent::UpdateApplied(Err(error)) => self.show_banner_error_with_action(
                "Не удалось установить обновление NORY",
                &error,
                BannerAction::UpdateApplication,
            ),
            UiEvent::ConnectionChanged(result) => {
                self.refresh_home();
                match result {
                    Ok(()) => {
                        let connected = self.core.status().phase == ConnectionPhase::Connected;
                        let reconnect = self.reconnect_pending.get();
                        if reconnect != ReconnectReason::None {
                            if connected {
                                self.connection_label.set_text("Переключаем сервер…");
                                self.connection_detail
                                    .set_text("Завершаем текущее подключение");
                            } else {
                                self.reconnect_pending.set(ReconnectReason::None);
                                self.toast_overlay
                                    .add_toast(adw::Toast::new(match reconnect {
                                        ReconnectReason::Routing => {
                                            "Применяем новые правила обхода"
                                        }
                                        ReconnectReason::Profile => "Подключаем выбранный сервер",
                                        ReconnectReason::None => unreachable!(),
                                    }));
                            }
                            self.power.emit_clicked();
                            return;
                        }
                        if connected
                            && self
                                .data
                                .lock()
                                .is_ok_and(|data| data.settings.should_auto_ping())
                        {
                            self.test_all_profiles();
                        }
                        if connected
                            && self
                                .data
                                .lock()
                                .is_ok_and(|data| data.settings.minimize_on_connect)
                            && self.tray.is_some()
                        {
                            self.window.hide();
                        }
                    }
                    Err(error) => {
                        self.reconnect_pending.set(ReconnectReason::None);
                        self.connection_label.set_text("Ошибка подключения");
                        self.connection_label.add_css_class("status-error");
                        let status = self.core.status();
                        let detail = if status.mode == Some(ConnectionMode::MihomoTun) {
                            error.as_str()
                        } else {
                            TUN_CONNECTION_ERROR_HINT
                        };
                        self.connection_detail.set_text(detail);
                        self.toast_overlay
                            .add_toast(adw::Toast::new(&format!("Ошибка подключения: {detail}")));
                    }
                }
            }
            UiEvent::ProfileSelected { name, changed } => {
                self.refresh_home();
                self.toast_overlay
                    .add_toast(adw::Toast::new(&format!("Выбран сервер: {name}")));
                if changed {
                    let phase = self.core.status().phase;
                    if matches!(
                        phase,
                        ConnectionPhase::Connected
                            | ConnectionPhase::Connecting
                            | ConnectionPhase::Disconnecting
                    ) {
                        self.reconnect_pending.set(ReconnectReason::Profile);
                        self.toast_overlay
                            .add_toast(adw::Toast::new("Переподключаем VPN к новому серверу"));
                        if phase == ConnectionPhase::Connected {
                            self.power.emit_clicked();
                        }
                    }
                }
            }
            UiEvent::Latencies(results) => {
                self.ping_busy.store(false, Ordering::Release);
                if let Ok(mut data) = self.data.lock() {
                    if data.settings.auto_select_fastest
                        && let Some((id, _)) = results
                            .iter()
                            .filter_map(|(id, result)| {
                                result.as_ref().ok().map(|value| (*id, *value))
                            })
                            .min_by_key(|(_, value)| *value)
                    {
                        data.selected_profile = Some(id);
                    }
                    for (id, result) in results {
                        if let Some(profile) =
                            data.profiles.iter_mut().find(|profile| profile.id == id)
                        {
                            profile.latency_ms = result.ok();
                        }
                    }
                    let _ = save_state(&self.paths, &data);
                }
                self.home_ping.set_sensitive(true);
                self.home_ping
                    .set_child(Some(&line_icon(LineIcon::Bolt, 16)));
                self.toast_overlay
                    .add_toast(adw::Toast::new("Проверка всех серверов завершена"));
                self.refresh_home_profiles();
            }
            UiEvent::SubscriptionAdded(id, result) | UiEvent::SubscriptionUpdated(id, result) => {
                match result {
                    Ok(fetch) => {
                        let mut applied = None;
                        if let Ok(mut data) = self.data.lock() {
                            match subscription::apply_fetch(&mut data, id, fetch) {
                                Ok(count) => {
                                    applied = Some(count);
                                    if data.selected_subscription.is_none() {
                                        data.selected_subscription = Some(id);
                                    }
                                    if data.selected_subscription == Some(id)
                                        && data.selected_profile.is_none_or(|selected| {
                                            !data.profiles.iter().any(|profile| {
                                                profile.id == selected
                                                    && profile.subscription_id == Some(id)
                                            })
                                        })
                                    {
                                        data.selected_profile = data
                                            .profiles
                                            .iter()
                                            .find(|profile| profile.subscription_id == Some(id))
                                            .map(|profile| profile.id);
                                    }
                                    let _ = save_state(&self.paths, &data);
                                }
                                Err(error) => self.show_banner_error_with_action(
                                    "Не удалось применить подписку",
                                    &error.to_string(),
                                    BannerAction::RetrySubscription(id),
                                ),
                            }
                        }
                        if let Some(count) = applied {
                            if self.banner_action.get() == BannerAction::RetrySubscription(id) {
                                self.hide_banner();
                            }
                            self.toast_overlay.add_toast(adw::Toast::new(&format!(
                                "Подписка обновлена · {count} серверов"
                            )));
                        }
                    }
                    Err(error) => self.show_banner_error_with_action(
                        "Не удалось обновить подписку",
                        &error,
                        BannerAction::RetrySubscription(id),
                    ),
                }
                let pending = self.subscription_updates_pending.get();
                if pending > 1 {
                    self.subscription_updates_pending.set(pending - 1);
                } else {
                    self.subscription_updates_pending.set(0);
                    self.home_update.set_sensitive(true);
                    self.home_update
                        .set_child(Some(&line_icon(LineIcon::Refresh, 16)));
                    self.refresh_home();
                    self.refresh_subscriptions();
                }
            }
            UiEvent::DataChanged => {
                self.refresh_home();
                self.refresh_subscriptions();
                self.refresh_applications();
                self.refresh_routing_rules();
            }
            UiEvent::Notice(message) => {
                self.toast_overlay.add_toast(adw::Toast::new(&message));
            }
            UiEvent::RoutingChanged(message) => {
                if self.core.status().phase == ConnectionPhase::Connected {
                    self.reconnect_pending.set(ReconnectReason::Routing);
                    self.toast_overlay
                        .add_toast(adw::Toast::new(&format!("{message}. Перезапускаем VPN…")));
                    self.power.emit_clicked();
                } else {
                    self.toast_overlay.add_toast(adw::Toast::new(&format!(
                        "{message}. Применится при следующем подключении"
                    )));
                }
            }
            UiEvent::Traffic(up, down) => {
                self.upload_label.set_text(&format_bytes(up));
                self.download_label.set_text(&format_bytes(down));
            }
            UiEvent::TrayShow => self.window.present(),
            UiEvent::TrayToggle => {
                self.power.emit_clicked();
            }
            UiEvent::TrayQuit => {
                let _ = self.core.disconnect();
                self.application.quit();
            }
        }
    }

    fn check_app_update(&self, manual: bool) {
        if self.update_check_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        self.last_core_check.set(unix_now());
        let tx = self.events.clone();
        std::thread::spawn(move || {
            let result = app_updater::check_for_update()
                .map(|app| CoreReleases { app, manual })
                .map_err(|error| error.to_string());
            let _ = tx.send(UiEvent::CoreChecked(result, manual));
        });
    }

    fn show_update_prompt(&self, release: &AppRelease) {
        let dialog = gtk::Dialog::builder()
            .title("Обновление NORY")
            .transient_for(&self.window)
            .modal(true)
            .resizable(false)
            .default_width(520)
            .build();
        dialog.add_button("Позже", gtk::ResponseType::Close);
        dialog.add_button("Обновить", gtk::ResponseType::Accept);
        dialog.set_default_response(gtk::ResponseType::Accept);
        if let Some(button) = dialog.widget_for_response(gtk::ResponseType::Accept) {
            button.add_css_class("primary");
        }
        let body = gtk::Box::new(Orientation::Vertical, 14);
        body.add_css_class("dialog-box");
        let hero = gtk::Box::new(Orientation::Horizontal, 14);
        hero.append(&app_icon(66));
        let copy = gtk::Box::new(Orientation::Vertical, 4);
        copy.set_valign(Align::Center);
        let title = label_x("Доступна новая версия NORY", 0.0);
        title.add_css_class("update-dialog-title");
        let version = label_x(
            &format!("{}  →  {}", env!("CARGO_PKG_VERSION"), release.version),
            0.0,
        );
        version.add_css_class("update-version");
        copy.append(&title);
        copy.append(&version);
        hero.append(&copy);
        body.append(&hero);
        let notes = label_x(
            if release.notes.trim().is_empty() {
                "Обновление приложения и встроенных компонентов VPN"
            } else {
                release.notes.trim()
            },
            0.0,
        );
        notes.set_wrap(true);
        notes.set_max_width_chars(58);
        body.append(&notes);
        let detail = label_x(
            &format!(
                "Размер: {} · файл подписан и будет проверен перед установкой",
                format_bytes(release.size)
            ),
            0.0,
        );
        detail.add_css_class("muted");
        detail.set_wrap(true);
        body.append(&detail);
        dialog.content_area().append(&body);
        let install = self.banner_button.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Accept {
                install.emit_clicked();
            }
        });
        dialog.present();
    }

    fn install_app_update(&self) {
        let Some(release) = self
            .pending_release
            .lock()
            .ok()
            .and_then(|release| release.clone())
        else {
            return;
        };
        self.banner_button.set_visible(false);
        self.banner_close_button.set_visible(false);
        self.banner_progress.set_visible(true);
        let paths = self.paths.clone();
        let tx = self.events.clone();
        std::thread::spawn(move || {
            let result = app_updater::download_update(&release, &paths.cache_dir, |progress| {
                let _ = tx.send(UiEvent::CoreProgress(progress));
            })
            .map_err(|error| error.to_string());
            let _ = tx.send(UiEvent::CoreInstalled(result));
        });
    }

    fn show_progress(&self, progress: AppUpdateProgress) {
        self.banner.set_reveal_child(true);
        self.banner_box.remove_css_class("error-banner");
        self.banner_box.remove_css_class("success-banner");
        self.banner_close_button.set_visible(false);
        match progress {
            AppUpdateProgress::Downloading { received, total } => {
                self.banner_title.set_text("Загружаем обновление NORY");
                self.banner_progress
                    .set_fraction((received as f64 / total.max(1) as f64).clamp(0.0, 1.0));
                self.banner_detail.set_text(&format!(
                    "{} из {}",
                    format_bytes(received),
                    format_bytes(total)
                ));
            }
            AppUpdateProgress::Verifying => {
                self.banner_title.set_text("Проверяем подпись и SHA-256");
                self.banner_detail
                    .set_text("Подменённое или повреждённое обновление не будет установлено");
                self.banner_progress.pulse();
            }
        }
    }

    fn show_banner_error(&self, title: &str, detail: &str) {
        self.show_banner_error_with_action(title, detail, BannerAction::Dismiss);
    }

    fn show_banner_error_with_action(&self, title: &str, detail: &str, action: BannerAction) {
        self.banner.set_reveal_child(true);
        self.banner_box.remove_css_class("success-banner");
        self.banner_box.add_css_class("error-banner");
        self.banner_title.set_text(title);
        self.banner_detail.set_text(detail);
        self.banner_progress.set_visible(false);
        self.banner_action.set(action);
        self.banner_button
            .set_label(if action == BannerAction::Dismiss {
                "Закрыть"
            } else {
                "Повторить"
            });
        self.banner_close_button
            .set_visible(action != BannerAction::Dismiss);
        self.banner_button.set_visible(true);
        self.toast_overlay
            .add_toast(adw::Toast::new(&format!("{title}: {detail}")));
    }

    fn hide_banner(&self) {
        self.banner.set_reveal_child(false);
        self.banner_box.remove_css_class("error-banner");
        self.banner_box.remove_css_class("success-banner");
        self.banner_button.set_visible(false);
        self.banner_close_button.set_visible(false);
        self.banner_progress.set_visible(false);
        self.banner_action.set(BannerAction::Dismiss);
    }
}

#[allow(clippy::too_many_arguments)]
fn wire_actions(
    ui: &Rc<Ui>,
    home_add: gtk::Button,
    settings_add_subscription: gtk::Button,
    home_update: gtk::Button,
    installed_application: gtk::Button,
    add_application: gtk::Button,
    add_process: gtk::Button,
    save_domains: gtk::Button,
    add_geodata: gtk::Button,
    clear_logs: gtk::Button,
    copy_logs: gtk::Button,
    save_settings: gtk::Button,
    open_data: gtk::Button,
    check_core: gtk::Button,
    import_profile: gtk::Button,
) {
    let weak = Rc::downgrade(ui);
    ui.bypass_ru.connect_state_set(move |switch, enabled| {
        let Some(ui) = weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        if ui.refreshing.get() {
            return gtk::glib::Propagation::Proceed;
        }
        if matches!(
            ui.core.status().phase,
            ConnectionPhase::Connecting | ConnectionPhase::Disconnecting
        ) {
            ui.toast_overlay.add_toast(adw::Toast::new(
                "Дождитесь завершения подключения или отключения VPN",
            ));
            ui.refreshing.set(true);
            switch.set_active(!enabled);
            ui.refreshing.set(false);
            return gtk::glib::Propagation::Stop;
        }
        let result = (|| -> Result<bool> {
            let mut data = ui
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("настройки недоступны"))?;
            if data.settings.routing.bypass_ru == enabled {
                return Ok(false);
            }
            let previous = data.settings.routing.bypass_ru;
            data.settings.routing.bypass_ru = enabled;
            if let Err(error) = save_state(&ui.paths, &data) {
                data.settings.routing.bypass_ru = previous;
                return Err(error);
            }
            Ok(true)
        })();
        match result {
            Ok(true) => {
                let message = if enabled {
                    "Обход России включён"
                } else {
                    "Обход России выключен"
                };
                let _ = ui.events.send(UiEvent::RoutingChanged(message.into()));
                gtk::glib::Propagation::Proceed
            }
            Ok(false) => gtk::glib::Propagation::Proceed,
            Err(error) => {
                ui.toast_overlay.add_toast(adw::Toast::new(&format!(
                    "Не удалось сохранить обход: {error}"
                )));
                ui.refreshing.set(true);
                switch.set_active(!enabled);
                ui.refreshing.set(false);
                gtk::glib::Propagation::Stop
            }
        }
    });
    let weak = Rc::downgrade(ui);
    home_add.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_subscription_dialog(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    installed_application.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_installed_application_picker(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    add_application.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_application_picker(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    add_process.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_process_picker(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    save_domains.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let buffer = ui.bypass_domains.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        match parse_bypass_domains(&text) {
            Ok(domains) => {
                if let Ok(mut data) = ui.data.lock() {
                    data.settings.routing.bypass_domains = domains;
                    if let Err(error) = save_state(&ui.paths, &data) {
                        ui.show_banner_error("Домены не сохранены", &error.to_string());
                        return;
                    }
                }
                let _ = ui
                    .events
                    .send(UiEvent::RoutingChanged("Правила доменов сохранены".into()));
            }
            Err(error) => ui.show_banner_error("Проверьте список доменов", &error.to_string()),
        }
    });
    let weak = Rc::downgrade(ui);
    add_geodata.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_geodata_picker(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.applications_search.connect_search_changed(move |entry| {
        let expected = entry.text().to_string();
        let weak = weak.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(180), move || {
            if let Some(ui) = weak.upgrade()
                && ui.applications_search.text().as_str() == expected
            {
                ui.refresh_applications();
            }
        });
    });
    let weak = Rc::downgrade(ui);
    ui.logs_search.connect_search_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.refresh_logs();
        }
    });
    let weak = Rc::downgrade(ui);
    clear_logs.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.core.clear_logs();
            ui.refresh_logs();
            ui.toast_overlay.add_toast(adw::Toast::new("Логи очищены"));
        }
    });
    let weak = Rc::downgrade(ui);
    copy_logs.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let buffer = ui.logs_view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(&text);
            ui.toast_overlay
                .add_toast(adw::Toast::new("Логи скопированы"));
        }
    });
    let weak = Rc::downgrade(ui);
    ui.home_mode_dropdown
        .connect_selected_notify(move |dropdown| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.refreshing.get() {
                return;
            }
            if let Ok(mut data) = ui.data.lock() {
                data.settings.mode = if dropdown.selected() == 1 {
                    ConnectionMode::MihomoTun
                } else {
                    ConnectionMode::Tun
                };
                let _ = save_state(&ui.paths, &data);
            }
            ui.mode_dropdown.set_selected(dropdown.selected());
            ui.toast_overlay
                .add_toast(adw::Toast::new(if dropdown.selected() == 1 {
                    "Выбран TUN · Mihomo"
                } else {
                    "Выбран TUN · Xray (sing-box)"
                }));
        });
    let weak = Rc::downgrade(ui);
    ui.home_ping.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.test_all_profiles();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.banner_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            let action = ui.banner_action.get();
            match action {
                BannerAction::UpdateApplication => {
                    ui.hide_banner();
                    if ui
                        .pending_release
                        .lock()
                        .is_ok_and(|release| release.is_some())
                    {
                        ui.install_app_update();
                    } else {
                        ui.check_app_update(true);
                    }
                }
                BannerAction::RetrySubscription(id) => {
                    ui.banner_box.remove_css_class("error-banner");
                    ui.banner_title.set_text("Обновляем подписку…");
                    ui.banner_detail.set_text("Повторная попытка");
                    ui.banner_button.set_visible(false);
                    ui.banner_close_button.set_visible(false);
                    ui.banner_progress.set_visible(true);
                    ui.banner_progress.pulse();
                    ui.subscription_updates_pending.set(1);
                    ui.home_update.set_sensitive(false);
                    let spinner = gtk::Spinner::new();
                    spinner.start();
                    ui.home_update.set_child(Some(&spinner));
                    update_subscription_background(
                        Arc::clone(&ui.data),
                        id,
                        ui.events.clone(),
                        false,
                        ui.hwid.clone(),
                    );
                }
                BannerAction::Dismiss => ui.hide_banner(),
            }
        }
    });
    let weak = Rc::downgrade(ui);
    ui.banner_close_button.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.hide_banner();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.home_delete.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_remove_subscription_dialog(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.home_subscription_previous.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let changed = ui.data.lock().is_ok_and(|mut data| {
            let changed = switch_active_subscription(&mut data, -1);
            if changed {
                let _ = save_state(&ui.paths, &data);
            }
            changed
        });
        if changed {
            ui.home_profiles_scroll.vadjustment().set_value(0.0);
            ui.refresh_home();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.home_subscription_next.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let changed = ui.data.lock().is_ok_and(|mut data| {
            let changed = switch_active_subscription(&mut data, 1);
            if changed {
                let _ = save_state(&ui.paths, &data);
            }
            changed
        });
        if changed {
            ui.home_profiles_scroll.vadjustment().set_value(0.0);
            ui.refresh_home();
        }
    });
    let weak = Rc::downgrade(ui);
    ui.power.connect_clicked(move |button| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let status = ui.core.status();
        if matches!(
            status.phase,
            ConnectionPhase::Connecting | ConnectionPhase::Disconnecting
        ) {
            return;
        }
        button.set_sensitive(false);
        let core = Arc::clone(&ui.core);
        let tx = ui.events.clone();
        if status.phase == ConnectionPhase::Connected {
            ui.connection_label.set_text("Отключаемся…");
            std::thread::spawn(move || {
                let result = core
                    .disconnect()
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let _ = tx.send(UiEvent::ConnectionChanged(result));
            });
        } else {
            ui.connection_label.set_text("Подключаемся…");
            let selected_mode = ui
                .data
                .lock()
                .map(|data| data.settings.mode)
                .unwrap_or_default();
            ui.connection_detail.set_text(match selected_mode {
                ConnectionMode::Tun => "Запускаем Xray и sing-box TUN",
                ConnectionMode::MihomoTun => "Запускаем Mihomo TUN",
            });
            let profile = ui.data.lock().ok().and_then(|data| {
                data.selected_profile
                    .and_then(|id| {
                        data.profiles
                            .iter()
                            .find(|profile| profile.id == id)
                            .cloned()
                    })
                    .map(|profile| (profile, data.settings.clone()))
            });
            let shared_installation = Arc::clone(&ui.installation);
            std::thread::spawn(move || {
                let result = match profile {
                    Some((profile, settings)) => {
                        let attempt = privileged::prepare_tun_core().and_then(|core_install| {
                            if let Ok(mut installation) = shared_installation.lock() {
                                *installation = Some(core_install.clone());
                            }
                            let bypass = active_bypass_processes(&settings);
                            match settings.mode {
                                ConnectionMode::Tun => core
                                    .connect_xray_tun(&core_install, &profile, &settings, &bypass)
                                    .map(|_| ()),
                                ConnectionMode::MihomoTun => core
                                    .connect_mihomo_tun(&profile, &settings, &bypass)
                                    .map(|_| ()),
                            }
                        });
                        if let Err(error) = &attempt {
                            core.report_connection_error(
                                &profile,
                                settings.mode,
                                &error.to_string(),
                            );
                        }
                        attempt.map_err(|error| format!("{error:#}"))
                    }
                    None => Err("Сначала выберите сервер и установите ядра NORY".into()),
                };
                let _ = tx.send(UiEvent::ConnectionChanged(result));
            });
        }
    });
    let weak = Rc::downgrade(ui);
    settings_add_subscription.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_subscription_dialog(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    home_update.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.update_active_subscription();
        }
    });
    let weak = Rc::downgrade(ui);
    save_settings.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.save_settings();
            ui.toast_overlay
                .add_toast(adw::Toast::new("Настройки сохранены"));
        }
    });
    let paths = ui.paths.clone();
    let toast_overlay = ui.toast_overlay.clone();
    open_data.connect_clicked(move |_| {
        let _ = open_path(&paths.data_dir);
        toast_overlay.add_toast(adw::Toast::new("Открыта папка данных NORY"));
    });
    let weak = Rc::downgrade(ui);
    check_core.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.check_app_update(true);
        }
    });
    let weak = Rc::downgrade(ui);
    import_profile.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            show_import_dialog(&ui);
        }
    });
}

impl Ui {
    fn save_settings(&self) {
        let mut autostart = None;
        if let Ok(mut data) = self.data.lock() {
            data.settings.mode = if self.mode_dropdown.selected() == 1 {
                ConnectionMode::MihomoTun
            } else {
                ConnectionMode::Tun
            };
            data.settings.socks_port = self.socks_port.value_as_int() as u16;
            data.settings.http_port = self.http_port.value_as_int() as u16;
            data.settings.api_port = self.api_port.value_as_int() as u16;
            data.settings.mtu = self.mtu.value_as_int() as u16;
            data.settings.allow_lan = self.allow_lan.is_active();
            data.settings.enable_ipv6 = self.enable_ipv6.is_active();
            data.settings.sniffing = self.sniffing.is_active();
            data.settings.sniffing_route_only = self.sniffing_route_only.is_active();
            data.settings.socks_udp = self.socks_udp.is_active();
            data.settings.dns_servers = self.dns_servers.text().trim().to_string();
            data.settings.domain_strategy = match self.domain_strategy.selected() {
                0 => DomainStrategy::AsIs,
                2 => DomainStrategy::IpOnDemand,
                _ => DomainStrategy::IpIfNonMatch,
            };
            data.settings.mux_enabled = self.mux_enabled.is_active();
            data.settings.mux_concurrency = self.mux_concurrency.value_as_int() as u16;
            data.settings.tls_allow_insecure = self.tls_allow_insecure.is_active();
            data.settings.tun_interface_name = self.tun_interface_name.text().trim().to_string();
            data.settings.tun_auto_route = true;
            data.settings.auto_connect = self.auto_connect.is_active();
            data.settings.auto_ping = self.auto_ping.is_active();
            data.settings.auto_select_fastest = self.auto_select_fastest.is_active();
            data.settings.ping_timeout_seconds = self.ping_timeout.value_as_int() as u8;
            data.settings.ping_parallelism = self.ping_parallelism.value_as_int() as u8;
            data.settings.traffic_refresh_seconds = self.traffic_refresh.value_as_int() as u8;
            data.settings.auto_reconnect = self.auto_reconnect.is_active();
            data.settings.reconnect_delay_seconds = self.reconnect_delay.value_as_int() as u8;
            data.settings.auto_update_subscriptions = self.auto_update_subscriptions.is_active();
            data.settings.subscription_update_interval_hours =
                self.subscription_update_hours.value_as_int() as u16;
            data.settings.log_level = match self.log_level.selected() {
                0 => LogLevel::None,
                1 => LogLevel::Error,
                3 => LogLevel::Info,
                _ => LogLevel::Warning,
            };
            data.settings.start_minimized = self.start_minimized.is_active();
            data.settings.minimize_on_connect = self.minimize_on_connect.is_active();
            data.settings.start_at_login = self.start_at_login.is_active();
            data.settings.close_to_tray = self.close_to_tray.is_active();
            autostart = Some(data.settings.start_at_login);
            let _ = save_state(&self.paths, &data);
        }
        if let Some(enabled) = autostart
            && let Err(error) = configure_autostart(enabled)
        {
            self.show_banner_error("Не удалось изменить автозапуск", &error.to_string());
        }
        self.home_mode_dropdown
            .set_selected(self.mode_dropdown.selected());
        self.refresh_home();
    }
}

fn server_grid() -> gtk::FlowBox {
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(3)
        .column_spacing(8)
        .row_spacing(8)
        .hexpand(true)
        .valign(Align::Start)
        .build();
    grid.add_css_class("server-grid");
    grid
}

fn server_card(profile: &Profile, selected: bool) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("server-card");
    button.set_hexpand(true);
    if selected {
        button.add_css_class("selected");
    }
    let content = gtk::Box::new(Orientation::Vertical, 6);
    content.set_size_request(216, -1);
    let header = gtk::Box::new(Orientation::Horizontal, 7);
    let (marker, display_name) = profile_marker_and_name(&profile.name);
    let badge = gtk::Box::new(Orientation::Horizontal, 0);
    badge.add_css_class("server-badge");
    badge.set_halign(Align::Center);
    badge.set_valign(Align::Center);
    if let Some(marker) = marker {
        if marker == "🌐" {
            #[cfg(target_os = "linux")]
            badge.add_css_class("flag-badge");
            badge.append(&location_globe(19));
        } else {
            badge.add_css_class("flag-badge");
            badge.append(&country_flag(&marker));
        }
    } else {
        badge.append(&line_icon(
            if profile.favorite {
                LineIcon::Star
            } else {
                LineIcon::Server
            },
            18,
        ));
    }
    let name = label_x(&display_name, 0.0);
    name.add_css_class("server-name");
    name.set_hexpand(true);
    name.set_wrap(true);
    name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    name.set_lines(2);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(22);
    name.set_tooltip_text(Some(&display_name));
    let check = line_icon(LineIcon::Check, 14);
    check.set_opacity(if selected { 1.0 } else { 0.0 });
    header.append(&badge);
    header.append(&name);
    header.append(&check);
    content.append(&header);

    let description = profile.description.as_deref().unwrap_or("").trim();
    let description_label = label_x(description, 0.0);
    description_label.add_css_class("server-description");
    description_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    description_label.set_max_width_chars(24);
    description_label.set_tooltip_text((!description.is_empty()).then_some(description));
    content.append(&description_label);

    let footer = gtk::Box::new(Orientation::Horizontal, 6);
    let mut metadata = format!("{} · {}", profile.protocol(), transport_label(profile));
    if profile.stream.security == TransportSecurity::Reality {
        metadata.push_str(" · REALITY");
    }
    if profile.source_format == nory::models::ProfileFormat::Json {
        metadata.push_str(" · JSON");
    }
    let protocol = label_x(&metadata, 0.0);
    protocol.add_css_class("server-protocol");
    protocol.set_hexpand(true);
    protocol.set_ellipsize(gtk::pango::EllipsizeMode::End);
    protocol.set_max_width_chars(20);
    protocol.set_tooltip_text(Some(&metadata));
    let latency_text = profile
        .latency_ms
        .map_or(String::new(), |value| format!("{value} мс"));
    let latency = label_x(&latency_text, 1.0);
    latency.add_css_class("server-latency");
    footer.append(&protocol);
    footer.append(&latency);
    content.append(&footer);
    button.set_tooltip_text(Some(&format!(
        "{display_name}\n{metadata}\n{}{latency_text}",
        if description.is_empty() {
            String::new()
        } else {
            format!("{description}\n")
        },
    )));
    button.set_child(Some(&content));
    button
}

#[allow(clippy::too_many_arguments)]
fn home_page(
    banner: &gtk::Revealer,
    profiles: &gtk::FlowBox,
    add: &gtk::Button,
    ping: &gtk::Button,
    update: &gtk::Button,
    delete: &gtk::Button,
    subscription_previous: &gtk::Button,
    subscription_next: &gtk::Button,
    subscription_summary: &gtk::Box,
    mode: &gtk::DropDown,
    power: &gtk::Button,
    title: &gtk::Label,
    detail: &gtk::Label,
    session_server: &gtk::Label,
    selected_marker: &gtk::Box,
    selected: &gtk::Label,
    duration: &gtk::Label,
    download: &gtk::Label,
    upload: &gtk::Label,
) -> (gtk::Widget, gtk::ScrolledWindow) {
    let overlay = gtk::Overlay::new();

    let shell = gtk::Box::new(Orientation::Vertical, 10);
    shell.add_css_class("home-shell");
    shell.set_hexpand(true);
    shell.set_valign(Align::Start);
    let page_scroll = content_scroll(&shell);
    page_scroll.add_css_class("home-page-scroll");
    // Keep the scrollbar beside the content, not at the far edge of a maximized window.
    let clamp = adw::Clamp::builder()
        .maximum_size(980)
        .tightening_threshold(700)
        .vexpand(true)
        .child(&page_scroll)
        .build();
    overlay.set_child(Some(&clamp));

    shell.append(banner);
    let topbar = gtk::Box::new(Orientation::Horizontal, 8);
    topbar.add_css_class("home-topbar");
    let brand = gtk::Box::new(Orientation::Horizontal, 9);
    brand.add_css_class("home-brand");
    brand.set_hexpand(true);
    let brand_icon = app_icon(24);
    brand_icon.add_css_class("home-brand-icon");
    brand.append(&brand_icon);
    let app_title = label_x("NORY", 0.0);
    app_title.add_css_class("home-title");
    brand.append(&app_title);
    topbar.append(&brand);
    topbar.append(mode);
    topbar.append(add);
    shell.append(&topbar);

    let overview = gtk::Box::new(Orientation::Horizontal, 12);
    overview.add_css_class("overview");
    let connection = gtk::Box::new(Orientation::Vertical, 12);
    connection.add_css_class("connection-tile");
    connection.set_size_request(238, -1);
    connection.set_valign(Align::Fill);
    title.add_css_class("connection-state");
    title.set_wrap(true);
    connection.append(title);
    power.set_halign(Align::Center);
    power.set_valign(Align::Center);
    power.set_size_request(104, 104);
    connection.append(power);
    let duration_row = gtk::Box::new(Orientation::Horizontal, 8);
    duration_row.set_halign(Align::Center);
    let time_icon = gtk::Image::from_icon_name("document-open-recent-symbolic");
    time_icon.set_pixel_size(14);
    duration_row.append(&time_icon);
    duration_row.append(duration);
    connection.append(&duration_row);
    overview.append(&connection);

    let session_column = gtk::Box::new(Orientation::Vertical, 10);
    session_column.set_hexpand(true);
    let session = gtk::Overlay::new();
    session.add_css_class("session-tile");
    session.set_vexpand(true);
    session.set_overflow(gtk::Overflow::Hidden);
    let session_copy = gtk::Box::new(Orientation::Vertical, 7);
    session_copy.add_css_class("session-copy");
    session_copy.set_valign(Align::Center);
    let caption = label_x("Выбранный сервер", 0.0);
    caption.add_css_class("tile-caption");
    session_copy.append(&caption);
    session_server.add_css_class("session-server");
    session_copy.append(session_server);
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_max_width_chars(42);
    detail.add_css_class("connection-detail");
    session_copy.append(detail);
    session.set_child(Some(&session_copy));
    session_column.append(&session);
    let traffic = gtk::Box::new(Orientation::Horizontal, 10);
    traffic.set_homogeneous(true);
    for (text, icon, value) in [
        ("Получено", "go-down-symbolic", download),
        ("Отправлено", "go-up-symbolic", upload),
    ] {
        let tile = gtk::Box::new(Orientation::Vertical, 7);
        tile.add_css_class("traffic-tile");
        tile.set_hexpand(true);
        let caption_row = gtk::Box::new(Orientation::Horizontal, 7);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(14);
        let caption = label_x(text, 0.0);
        caption.add_css_class("tile-caption");
        caption_row.append(&image);
        caption_row.append(&caption);
        tile.append(&caption_row);
        value.set_xalign(0.0);
        tile.append(value);
        traffic.append(&tile);
    }
    session_column.append(&traffic);
    overview.append(&session_column);
    shell.append(&overview);

    let server_switcher = gtk::Box::new(Orientation::Horizontal, 2);
    server_switcher.add_css_class("server-switcher");
    server_switcher.set_hexpand(true);
    server_switcher.append(subscription_previous);
    let server_toggle = gtk::ToggleButton::new();
    server_toggle.add_css_class("server-drawer-toggle");
    let selected_box = gtk::Box::new(Orientation::Horizontal, 9);
    selected_box.append(selected_marker);
    selected.set_hexpand(true);
    selected_box.append(selected);
    let drawer_chevron = gtk::Image::from_icon_name("pan-down-symbolic");
    drawer_chevron.add_css_class("drawer-chevron");
    drawer_chevron.set_pixel_size(16);
    selected_box.append(&drawer_chevron);
    server_toggle.set_child(Some(&selected_box));
    server_toggle.set_tooltip_text(Some("Показать или скрыть серверы"));
    server_toggle.set_hexpand(true);
    server_switcher.append(&server_toggle);
    server_switcher.append(subscription_next);
    let server_control = gtk::Box::new(Orientation::Horizontal, 7);
    server_control.add_css_class("server-control");
    server_control.append(&server_switcher);
    server_control.append(ping);
    server_control.append(update);
    server_control.append(delete);
    ping.set_valign(Align::Center);
    update.set_valign(Align::Center);
    delete.set_valign(Align::Center);
    let subscription_panel = gtk::Box::new(Orientation::Vertical, 0);
    subscription_panel.add_css_class("subscription-panel");
    subscription_panel.append(&server_control);
    subscription_panel.append(subscription_summary);
    shell.append(&subscription_panel);

    let drawer_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(220)
        .reveal_child(false)
        .build();
    let drawer = gtk::Box::new(Orientation::Vertical, 0);
    drawer.add_css_class("server-drawer");
    drawer.append(profiles);
    drawer_revealer.set_child(Some(&drawer));
    shell.append(&drawer_revealer);
    let developer_link =
        gtk::LinkButton::with_label("https://t.me/linuxset", "TG канал разработчика");
    developer_link.add_css_class("developer-link");
    developer_link.set_halign(Align::End);
    developer_link.set_tooltip_text(Some("Открыть Telegram-канал разработчика"));
    shell.append(&developer_link);
    server_toggle.connect_toggled(move |button| {
        drawer_revealer.set_reveal_child(button.is_active());
        drawer_chevron.set_icon_name(Some(if button.is_active() {
            "pan-up-symbolic"
        } else {
            "pan-down-symbolic"
        }));
    });
    server_toggle.set_active(true);

    (overlay.upcast(), page_scroll)
}

fn applications_page(
    list: &gtk::ListBox,
    search: &gtk::SearchEntry,
    domains: &gtk::TextView,
    geodata: &gtk::ListBox,
    bypass_ru: &gtk::Switch,
) -> (
    gtk::Widget,
    gtk::Button,
    gtk::Button,
    gtk::Button,
    gtk::Button,
    gtk::Button,
) {
    let page = page_box();
    let (heading, actions) = heading(
        "Обход VPN",
        "Домены, GeoData, приложения и процессы для прямого подключения",
    );
    let installed_application = text_button("Из списка");
    let add_application = text_button("Выбрать файл");
    let add_process = primary_button("Добавить процесс");
    actions.append(&installed_application);
    actions.append(&add_application);
    actions.append(&add_process);
    page.append(&heading);

    let content = gtk::Box::new(Orientation::Vertical, 14);
    let presets = gtk::Box::new(Orientation::Vertical, 4);
    presets.add_css_class("card");
    presets.append(&switch_row(
        "Россия — напрямую",
        "Встроенные GeoSite category-ru и GeoIP ru. Совпавший трафик идёт вне VPN.",
        bypass_ru,
    ));
    bypass_ru.set_tooltip_text(Some(
        "Не требует загрузки файлов. При активном VPN изменение перезапустит подключение.",
    ));
    content.append(&presets);
    let domains_card = gtk::Box::new(Orientation::Vertical, 9);
    domains_card.add_css_class("card");
    let domains_header = gtk::Box::new(Orientation::Horizontal, 10);
    let domains_title = section_title("Домены напрямую");
    domains_title.set_hexpand(true);
    let save_domains = primary_button("Сохранить домены");
    domains_header.append(&domains_title);
    domains_header.append(&save_domains);
    domains_card.append(&domains_header);
    let domains_note = label_x(
        "По одному домену на строку. example.com включает все поддомены; доступны full:, domain:, keyword: и regexp:.",
        0.0,
    );
    domains_note.add_css_class("muted");
    domains_note.set_wrap(true);
    domains_card.append(&domains_note);
    domains.set_wrap_mode(gtk::WrapMode::WordChar);
    domains.add_css_class("routing-editor");
    let domains_scroll = content_scroll(domains);
    domains_scroll.set_min_content_height(112);
    domains_scroll.set_vexpand(false);
    domains_scroll.add_css_class("list-card");
    domains_card.append(&domains_scroll);
    content.append(&details_section("Обход по доменам", &domains_card));

    let geodata_card = gtk::Box::new(Orientation::Vertical, 9);
    geodata_card.add_css_class("card");
    let geodata_header = gtk::Box::new(Orientation::Horizontal, 10);
    let geodata_title = section_title("GeoData напрямую");
    geodata_title.set_hexpand(true);
    let add_geodata = primary_button("Добавить .dat");
    geodata_header.append(&geodata_title);
    geodata_header.append(&add_geodata);
    geodata_card.append(&geodata_header);
    let geodata_note = label_x(
        "Импортируйте GeoSite или GeoIP и укажите теги из файла. NORY хранит отдельную локальную копию и подключает её к Xray.",
        0.0,
    );
    geodata_note.add_css_class("muted");
    geodata_note.set_wrap(true);
    geodata_card.append(&geodata_note);
    geodata.add_css_class("list-card");
    geodata_card.append(geodata);
    content.append(&details_section("Правила GeoData", &geodata_card));

    let applications_card = gtk::Box::new(Orientation::Vertical, 9);
    applications_card.add_css_class("card");
    applications_card.append(&section_title("Приложения и процессы"));
    applications_card.append(&actions);
    let note = label_x(
        "Обход процессов работает в режиме TUN и автоматически отслеживает их TCP/UDP-порты.",
        0.0,
    );
    note.add_css_class("muted");
    note.set_wrap(true);
    applications_card.append(&note);
    search.add_css_class("settings-entry");
    applications_card.append(search);
    list.add_css_class("list-card");
    applications_card.append(list);
    content.prepend(&applications_card);
    let scroll = content_scroll(&content);
    page.append(&scroll);
    (
        page.upcast(),
        installed_application,
        add_application,
        add_process,
        save_domains,
        add_geodata,
    )
}

fn logs_page(
    view: &gtk::TextView,
    search: &gtk::SearchEntry,
) -> (gtk::Widget, gtk::Button, gtk::Button) {
    let page = page_box();
    let (heading, actions) = heading("Журнал событий", "Подключения, подписки и сообщения ядер");
    let clear = text_button("Очистить");
    let copy = primary_button("Копировать");
    actions.append(&clear);
    actions.append(&copy);
    heading.append(&actions);
    page.append(&heading);
    search.add_css_class("settings-entry");
    page.append(search);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_left_margin(14);
    view.set_right_margin(14);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    view.add_css_class("log-view");
    let scroll = content_scroll(view);
    scroll.add_css_class("list-card");
    page.append(&scroll);
    (page.upcast(), clear, copy)
}

#[allow(clippy::too_many_arguments)]
fn settings_page(
    mode: &gtk::DropDown,
    socks: &gtk::SpinButton,
    http: &gtk::SpinButton,
    api: &gtk::SpinButton,
    mtu: &gtk::SpinButton,
    lan: &gtk::Switch,
    ipv6: &gtk::Switch,
    sniffing: &gtk::Switch,
    sniffing_route_only: &gtk::Switch,
    socks_udp: &gtk::Switch,
    dns_servers: &gtk::Entry,
    domain_strategy: &gtk::DropDown,
    mux_enabled: &gtk::Switch,
    mux_concurrency: &gtk::SpinButton,
    tls_allow_insecure: &gtk::Switch,
    tun_interface_name: &gtk::Entry,
    tun_auto_route: &gtk::Switch,
    auto: &gtk::Switch,
    auto_ping: &gtk::Switch,
    auto_select_fastest: &gtk::Switch,
    ping_timeout: &gtk::SpinButton,
    ping_parallelism: &gtk::SpinButton,
    traffic_refresh: &gtk::SpinButton,
    auto_reconnect: &gtk::Switch,
    reconnect_delay: &gtk::SpinButton,
    auto_update: &gtk::Switch,
    update_hours: &gtk::SpinButton,
    log_level: &gtk::DropDown,
    minimized: &gtk::Switch,
    minimize_on_connect: &gtk::Switch,
    start_at_login: &gtk::Switch,
    close_to_tray: &gtk::Switch,
    subscriptions: &gtk::ListBox,
    add_subscription: &gtk::Button,
) -> (
    gtk::Widget,
    gtk::Button,
    gtk::Button,
    gtk::Button,
    gtk::Button,
) {
    let page = page_box();
    let (heading, actions) = heading(
        "Настройки",
        "TUN-ядра, сеть, подписки и поведение приложения",
    );
    let save = primary_button("Сохранить");
    actions.append(&save);
    heading.append(&actions);
    page.append(&heading);
    let content = gtk::Box::new(Orientation::Vertical, 14);
    let connection = gtk::Box::new(Orientation::Vertical, 7);
    let behavior_content = gtk::Box::new(Orientation::Vertical, 14);
    let data_content = gtk::Box::new(Orientation::Vertical, 14);
    connection.add_css_class("card");
    connection.append(&section_title("Подключение и TUN"));
    connection.append(&field_row("Режим подключения", mode));
    connection.append(&field_row("Имя TUN-интерфейса", tun_interface_name));
    connection.append(&switch_row(
        "Защита от утечек TUN",
        "Всегда включена: при недоступном сервере прямой трафик блокируется",
        tun_auto_route,
    ));
    connection.append(&field_row("MTU", mtu));
    connection.append(&switch_row("IPv6", "Маршрутизировать IPv6 в TUN", ipv6));
    content.append(&connection);

    let service_ports = gtk::Box::new(Orientation::Vertical, 7);
    service_ports.add_css_class("card");
    service_ports.append(&section_title("Служебные порты ядер"));
    service_ports.append(&field_row("Внутренний SOCKS Xray", socks));
    service_ports.append(&field_row("Локальный API ядра", api));
    let _ = (http, lan, socks_udp);

    let dns = gtk::Box::new(Orientation::Vertical, 7);
    dns.add_css_class("card");
    dns.append(&section_title("DNS и анализ трафика"));
    dns.append(&field_row("DNS-серверы", dns_servers));
    dns.append(&field_row("Стратегия доменов", domain_strategy));
    dns.append(&switch_row(
        "Sniffing",
        "Определять домен по SNI и Host",
        sniffing,
    ));
    dns.append(&switch_row(
        "Sniffing только для правил",
        "Не заменять конечный адрес распознанным доменом",
        sniffing_route_only,
    ));
    content.append(&dns);
    content.append(&details_section("Служебные порты", &service_ports));

    let transport = gtk::Box::new(Orientation::Vertical, 7);
    transport.add_css_class("card");
    transport.append(&section_title("Транспорт и безопасность"));
    transport.append(&switch_row(
        "Mux",
        "Объединять соединения для снижения накладных расходов",
        mux_enabled,
    ));
    transport.append(&field_row("Параллельность Mux", mux_concurrency));
    transport.append(&switch_row(
        "Разрешить недоверенный TLS",
        "Использовать только с серверами, которым вы доверяете",
        tls_allow_insecure,
    ));
    content.append(&details_section("Транспорт и безопасность", &transport));

    let checks = gtk::Box::new(Orientation::Vertical, 7);
    checks.add_css_class("card");
    checks.append(&section_title("Проверка серверов и статистика"));
    checks.append(&switch_row(
        "Проверять серверы при запуске",
        "Запускать массовый пинг после старта",
        auto_ping,
    ));
    checks.append(&switch_row(
        "Выбирать самый быстрый сервер",
        "После массовой проверки выбирать профиль с минимальной задержкой",
        auto_select_fastest,
    ));
    checks.append(&field_row(
        "Тайм-аут одного ICMP-ответа, секунд",
        ping_timeout,
    ));
    checks.append(&field_row("Параллельных проверок", ping_parallelism));
    checks.append(&field_row("Обновление трафика, секунд", traffic_refresh));
    behavior_content.append(&checks);

    let automation = gtk::Box::new(Orientation::Vertical, 7);
    automation.add_css_class("card");
    automation.append(&section_title("Автоматизация"));
    automation.append(&switch_row(
        "Автоподключение",
        "Подключаться к выбранному серверу",
        auto,
    ));
    automation.append(&switch_row(
        "Переподключение после сбоя",
        "Повторно запускать выбранный сервер, если активное ядро завершилось",
        auto_reconnect,
    ));
    automation.append(&field_row(
        "Задержка переподключения, секунд",
        reconnect_delay,
    ));
    automation.append(&switch_row(
        "Автообновление подписок",
        "Обновлять подписки при запуске",
        auto_update,
    ));
    automation.append(&field_row("Интервал подписок, часов", update_hours));
    behavior_content.prepend(&automation);

    let behavior = gtk::Box::new(Orientation::Vertical, 7);
    behavior.add_css_class("card");
    behavior.append(&section_title("Поведение приложения"));
    behavior.append(&switch_row(
        "Запускать вместе с системой",
        "Добавить NORY в автозапуск текущего пользователя",
        start_at_login,
    ));
    behavior.append(&switch_row(
        "Запускать свёрнутым",
        "Не показывать окно после входа",
        minimized,
    ));
    behavior.append(&switch_row(
        "Сворачивать после подключения",
        "Скрывать окно в трей после успешного подключения",
        minimize_on_connect,
    ));
    behavior.append(&switch_row(
        "Закрывать в трей",
        "Кнопка закрытия скрывает окно, соединение продолжает работать",
        close_to_tray,
    ));
    behavior_content.prepend(&behavior);

    let subscription_card = gtk::Box::new(Orientation::Vertical, 9);
    subscription_card.add_css_class("card");
    let subscription_header = gtk::Box::new(Orientation::Horizontal, 10);
    let subscription_title = section_title("Подписки");
    subscription_title.set_hexpand(true);
    subscription_header.append(&subscription_title);
    subscription_header.append(add_subscription);
    subscription_card.append(&subscription_header);
    let subscription_note = label_x(
        "Можно добавить несколько подписок. Здесь для каждой отдельно настраивается HWID, обновление и удаление вместе с её серверами.",
        0.0,
    );
    subscription_note.add_css_class("muted");
    subscription_note.set_wrap(true);
    subscription_card.append(&subscription_note);
    subscriptions.add_css_class("list-card");
    subscription_card.append(subscriptions);
    data_content.append(&subscription_card);

    let service_columns = gtk::Box::new(Orientation::Vertical, 14);
    let xray = gtk::Box::new(Orientation::Vertical, 7);
    xray.add_css_class("card");
    xray.set_hexpand(true);
    xray.append(&section_title("Версия NORY и диагностика"));
    let version = label_x(
        &format!("Установлена NORY {}", env!("CARGO_PKG_VERSION")),
        0.0,
    );
    version.add_css_class("muted");
    xray.append(&version);
    xray.append(&field_row("Уровень журнала", log_level));
    let check_core = text_button("Проверить обновление NORY");
    xray.append(&check_core);
    let data_note = label_x(
        "Xray, sing-box и Mihomo уже включены в пакет и обновляются только вместе с подписанным релизом NORY.",
        0.0,
    );
    data_note.set_wrap(true);
    data_note.add_css_class("muted");
    xray.append(&data_note);

    let data = gtk::Box::new(Orientation::Vertical, 7);
    data.add_css_class("card");
    data.set_hexpand(true);
    data.append(&section_title("Данные и конфигурации"));
    let import_profile = text_button("Импортировать конфигурацию");
    let open = text_button("Открыть папку данных");
    data.append(&import_profile);
    data.append(&open);
    let hwid_note = label_x(
        "HWID хранится локально и отправляется только подпискам, где включён переключатель HWID.",
        0.0,
    );
    hwid_note.add_css_class("muted");
    hwid_note.set_wrap(true);
    data.append(&hwid_note);
    service_columns.append(&xray);
    service_columns.append(&data);
    data_content.append(&service_columns);
    for group in [
        &connection,
        &service_ports,
        &dns,
        &transport,
        &checks,
        &automation,
        &behavior,
    ] {
        group.add_css_class("settings-group");
    }

    let categories = gtk::Stack::builder()
        .vexpand(true)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(180)
        .build();
    for (name, title, body) in [
        ("network", "Подключение", content),
        ("behavior", "Поведение", behavior_content),
        ("data", "Подписки и данные", data_content),
    ] {
        let scroll = content_scroll(&body);
        categories.add_titled(&scroll, Some(name), title);
    }
    let tabs = gtk::StackSwitcher::builder()
        .stack(&categories)
        .halign(Align::Start)
        .build();
    tabs.add_css_class("settings-tabs");
    page.append(&tabs);
    page.append(&categories);
    (page.upcast(), save, open, check_core, import_profile)
}

fn active_bypass_processes(settings: &Settings) -> Vec<String> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for process in settings
        .routing
        .applications
        .iter()
        .filter(|application| application.bypass)
        .flat_map(|application| application.processes.iter())
    {
        let matcher = process.trim().replace('\\', "/");
        if matcher.is_empty() || matcher.contains('\0') {
            continue;
        }
        if seen.insert(matcher.clone()) {
            result.push(matcher.clone());
        }
        if !matcher.ends_with('/')
            && let Some(name) = matcher.rsplit('/').next()
            && name != matcher
            && !name.is_empty()
            && seen.insert(name.to_string())
        {
            result.push(name.to_string());
        }
    }
    result
}

fn active_subscription_id(data: &AppData) -> Option<Uuid> {
    data.selected_subscription
        .filter(|id| {
            data.subscriptions
                .iter()
                .any(|subscription| subscription.id == *id)
        })
        .or_else(|| {
            data.subscriptions
                .first()
                .map(|subscription| subscription.id)
        })
}

fn switch_active_subscription(data: &mut AppData, step: isize) -> bool {
    if data.subscriptions.len() < 2 {
        return false;
    }
    let current = active_subscription_id(data)
        .and_then(|id| {
            data.subscriptions
                .iter()
                .position(|subscription| subscription.id == id)
        })
        .unwrap_or(0);
    let count = data.subscriptions.len() as isize;
    let next = (current as isize + step).rem_euclid(count) as usize;
    let id = data.subscriptions[next].id;
    data.selected_subscription = Some(id);
    data.selected_profile = data
        .profiles
        .iter()
        .find(|profile| profile.subscription_id == Some(id))
        .map(|profile| profile.id);
    true
}

fn add_manual_application(ui: &Rc<Ui>, name: String, matcher: String) {
    let matcher = matcher.trim().to_string();
    if matcher.is_empty() || matcher.contains('\0') {
        ui.show_banner_error(
            "Приложение не добавлено",
            "Путь или имя процесса некорректны",
        );
        return;
    }
    if let Ok(mut data) = ui.data.lock() {
        if let Some(existing) = data
            .settings
            .routing
            .applications
            .iter_mut()
            .find(|application| application.processes.iter().any(|value| value == &matcher))
        {
            existing.bypass = true;
        } else {
            data.settings.routing.applications.push(ApplicationRule {
                id: format!("manual:{}", Uuid::new_v4()),
                name,
                processes: vec![matcher],
                source: ApplicationSource::Manual,
                bypass: true,
            });
        }
        let _ = save_state(&ui.paths, &data);
    }
    ui.refresh_applications();
    let _ = ui
        .events
        .send(UiEvent::RoutingChanged("Правило обхода добавлено".into()));
}

fn parse_bypass_domains(text: &str) -> Result<Vec<String>> {
    let mut domains = Vec::new();
    let mut seen = HashSet::new();
    for raw in text.lines() {
        let value = raw.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        if value.len() > 512 || value.contains(['\0', '\r']) {
            anyhow::bail!("слишком длинное или повреждённое правило");
        }
        let advanced = ["domain:", "full:", "keyword:", "regexp:"]
            .iter()
            .any(|prefix| value.starts_with(prefix));
        if advanced {
            if value
                .split_once(':')
                .is_none_or(|(_, body)| body.is_empty())
            {
                anyhow::bail!("пустое правило: {value}");
            }
        } else if value.contains(['/', '\\', ':']) || value.chars().any(char::is_whitespace) {
            anyhow::bail!("некорректный домен: {value}");
        }
        let normalized = if advanced {
            value.to_string()
        } else {
            value
                .strip_prefix("*.")
                .or_else(|| value.strip_prefix('.'))
                .unwrap_or(value)
                .trim_end_matches('.')
                .to_lowercase()
        };
        if normalized.is_empty() {
            anyhow::bail!("пустое правило домена");
        }
        let key = normalized.to_lowercase();
        if seen.insert(key) {
            domains.push(normalized);
        }
        if domains.len() > 2_048 {
            anyhow::bail!("можно добавить не более 2048 доменных правил");
        }
    }
    Ok(domains)
}

fn parse_geodata_tags(text: &str) -> Result<Vec<String>> {
    let mut tags = Vec::new();
    let mut seen = HashSet::new();
    for value in text.split([',', ';', '\n', '\t', ' ']).map(str::trim) {
        if value.is_empty() {
            continue;
        }
        if value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.@!".contains(&byte))
        {
            anyhow::bail!("некорректный тег GeoData: {value}");
        }
        if seen.insert(value.to_ascii_lowercase()) {
            tags.push(value.to_string());
        }
        if tags.len() > 64 {
            anyhow::bail!("для одного файла можно указать не более 64 тегов");
        }
    }
    if tags.is_empty() {
        anyhow::bail!("укажите хотя бы один тег из GeoData-файла");
    }
    Ok(tags)
}

fn managed_geodata_path(paths: &Paths, file_name: &str) -> Option<PathBuf> {
    let file = Path::new(file_name);
    (file.file_name() == Some(file.as_os_str()) && file_name.to_ascii_lowercase().ends_with(".dat"))
        .then(|| paths.geodata_dir().join(file))
}

fn show_geodata_picker(ui: &Rc<Ui>) {
    let dialog = gtk::FileChooserDialog::new(
        Some("Выберите GeoData-файл"),
        Some(&ui.window),
        gtk::FileChooserAction::Open,
        &[
            ("Отмена", gtk::ResponseType::Cancel),
            ("Продолжить", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("GeoData (*.dat)"));
    filter.add_pattern("*.dat");
    filter.add_pattern("*.DAT");
    dialog.add_filter(&filter);
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
            && let Some(ui) = weak.upgrade()
        {
            show_geodata_rule_dialog(&ui, path);
        }
        dialog.close();
    });
    dialog.present();
}

fn show_geodata_rule_dialog(ui: &Rc<Ui>, source: PathBuf) {
    let dialog = gtk::Dialog::builder()
        .title("Настроить GeoData")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(540)
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Добавить в обход", gtk::ResponseType::Accept);
    let body = gtk::Box::new(Orientation::Vertical, 10);
    body.add_css_class("dialog-box");
    let display_name = source.file_name().map_or_else(
        || source.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let selected = label_x(&display_name, 0.0);
    selected.add_css_class("settings-title");
    selected.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let kind = gtk::DropDown::from_strings(&["GeoSite — домены", "GeoIP — IP-диапазоны"]);
    if display_name.to_ascii_lowercase().contains("geoip") {
        kind.set_selected(1);
    }
    let tags = gtk::Entry::builder()
        .placeholder_text("Теги: ru, private, category-ads-all")
        .build();
    let note = label_x(
        "Укажите существующие теги из выбранного файла через запятую. Весь трафик, совпавший с ними, пойдёт напрямую.",
        0.0,
    );
    note.add_css_class("muted");
    note.set_wrap(true);
    let error = label_x("", 0.0);
    error.add_css_class("status-error");
    error.set_wrap(true);
    error.set_visible(false);
    body.append(&selected);
    body.append(&field_row("Тип данных", &kind));
    body.append(&tags);
    body.append(&note);
    body.append(&error);
    dialog.content_area().append(&body);

    let state = Arc::clone(&ui.data);
    let paths = ui.paths.clone();
    let tx = ui.events.clone();
    let tags_input = tags.clone();
    dialog.connect_response(move |dialog, response| {
        if response != gtk::ResponseType::Accept {
            dialog.close();
            return;
        }
        let result = (|| -> Result<()> {
            let metadata = fs::metadata(&source).context("не удалось прочитать GeoData-файл")?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
                anyhow::bail!("GeoData-файл пуст или превышает 256 МБ");
            }
            if source
                .extension()
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("dat"))
            {
                anyhow::bail!("поддерживаются только файлы с расширением .dat");
            }
            let parsed_tags = parse_geodata_tags(&tags_input.text())?;
            let rule_kind = if kind.selected() == 1 {
                GeoDataKind::GeoIp
            } else {
                GeoDataKind::GeoSite
            };
            let id = Uuid::new_v4().simple().to_string();
            let prefix = match rule_kind {
                GeoDataKind::GeoSite => "geosite",
                GeoDataKind::GeoIp => "geoip",
            };
            let file_name = format!("nory-{prefix}-{id}.dat");
            fs::create_dir_all(paths.geodata_dir())?;
            let destination = paths.geodata_dir().join(&file_name);
            let temporary = paths.geodata_dir().join(format!(".{file_name}.tmp"));
            fs::copy(&source, &temporary).context("не удалось импортировать GeoData-файл")?;
            fs::rename(&temporary, &destination)?;
            let rule = GeoDataRule {
                id,
                display_name: display_name.clone(),
                file_name,
                kind: rule_kind,
                tags: parsed_tags,
            };
            let saved = state
                .lock()
                .map_err(|_| anyhow::anyhow!("внутренняя ошибка настроек"))
                .and_then(|mut data| {
                    data.settings.routing.bypass_geodata.push(rule);
                    save_state(&paths, &data)
                });
            if let Err(save_error) = saved {
                let _ = fs::remove_file(destination);
                return Err(save_error);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let _ = tx.send(UiEvent::DataChanged);
                let _ = tx.send(UiEvent::RoutingChanged("GeoData-правило добавлено".into()));
                dialog.close();
            }
            Err(import_error) => {
                error.set_text(&import_error.to_string());
                error.set_visible(true);
            }
        }
    });
    dialog.present();
    tags.grab_focus();
}

fn show_application_picker(ui: &Rc<Ui>) {
    let dialog = gtk::FileChooserDialog::new(
        Some("Выберите исполняемый файл"),
        Some(&ui.window),
        gtk::FileChooserAction::Open,
        &[
            ("Отмена", gtk::ResponseType::Cancel),
            ("Добавить", gtk::ResponseType::Accept),
        ],
    );
    dialog.set_modal(true);
    let weak = Rc::downgrade(ui);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
            && let Some(ui) = weak.upgrade()
        {
            let name = path.file_name().map_or_else(
                || path.to_string_lossy().into_owned(),
                |value| value.to_string_lossy().into_owned(),
            );
            add_manual_application(&ui, name, path.to_string_lossy().into_owned());
        }
        dialog.close();
    });
    dialog.present();
}

fn show_installed_application_picker(ui: &Rc<Ui>) {
    let applications = applications::installed_applications();
    if applications.is_empty() {
        ui.show_banner_error(
            "Приложения не найдены",
            "Не удалось прочитать список установленных приложений",
        );
        return;
    }
    let dialog = gtk::Dialog::builder()
        .title("Выбрать установленное приложение")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(620)
        .default_height(560)
        .build();
    dialog.add_button("Закрыть", gtk::ResponseType::Close);
    let body = gtk::Box::new(Orientation::Vertical, 10);
    body.add_css_class("dialog-box");
    let note = label_x(
        "Найдите приложение по названию или команде запуска и нажмите на него.",
        0.0,
    );
    note.add_css_class("muted");
    note.set_wrap(true);
    body.append(&note);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Поиск по приложениям")
        .build();
    search.add_css_class("settings-entry");
    body.append(&search);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("list-card");
    let entries = Rc::new(RefCell::new(Vec::<(gtk::ListBoxRow, String)>::new()));
    for application in applications {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        let button = gtk::Button::new();
        button.add_css_class("flat");
        let content = gtk::Box::new(Orientation::Vertical, 2);
        content.add_css_class("process-picker-row");
        let name = label_x(&application.name, 0.0);
        name.add_css_class("settings-title");
        let matcher = label_x(&application.matcher, 0.0);
        matcher.add_css_class("muted");
        matcher.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        content.append(&name);
        content.append(&matcher);
        button.set_child(Some(&content));
        let weak = Rc::downgrade(ui);
        let picker = dialog.clone();
        let application_name = application.name.clone();
        let application_matcher = application.matcher.clone();
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                add_manual_application(&ui, application_name.clone(), application_matcher.clone());
            }
            picker.close();
        });
        row.set_child(Some(&button));
        entries.borrow_mut().push((
            row.clone(),
            format!("{} {}", application.name, application.matcher).to_lowercase(),
        ));
        list.append(&row);
    }
    let visible_entries = Rc::clone(&entries);
    search.connect_search_changed(move |search| {
        let query = search.text().trim().to_lowercase();
        for (row, haystack) in visible_entries.borrow().iter() {
            row.set_visible(query.is_empty() || haystack.contains(&query));
        }
    });
    let scroll = content_scroll(&list);
    body.append(&scroll);
    dialog.content_area().append(&body);
    dialog.connect_response(move |dialog, _| dialog.close());
    dialog.present();
    search.grab_focus();
}

fn show_process_picker(ui: &Rc<Ui>) {
    let processes = applications::running_processes();
    if processes.is_empty() {
        ui.show_banner_error(
            "Процессы не найдены",
            "Не удалось получить список запущенных процессов",
        );
        return;
    }
    let dialog = gtk::Dialog::builder()
        .title("Добавить запущенный процесс")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(620)
        .default_height(540)
        .build();
    dialog.add_button("Закрыть", gtk::ResponseType::Close);
    let body = gtk::Box::new(Orientation::Vertical, 10);
    body.add_css_class("dialog-box");
    let note = label_x(
        "Найдите процесс по имени или пути и нажмите на него. В активном TUN новое правило применится автоматически.",
        0.0,
    );
    note.add_css_class("muted");
    note.set_wrap(true);
    body.append(&note);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Поиск по процессам")
        .build();
    search.add_css_class("settings-entry");
    body.append(&search);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("list-card");
    let entries = Rc::new(RefCell::new(Vec::<(gtk::ListBoxRow, String)>::new()));
    for process in processes {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        let button = gtk::Button::new();
        button.add_css_class("flat");
        let content = gtk::Box::new(Orientation::Vertical, 2);
        content.add_css_class("process-picker-row");
        let name = label_x(&process.name, 0.0);
        name.add_css_class("settings-title");
        let matcher = label_x(&process.matcher, 0.0);
        matcher.add_css_class("muted");
        matcher.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        content.append(&name);
        content.append(&matcher);
        button.set_child(Some(&content));
        let weak = Rc::downgrade(ui);
        let picker = dialog.clone();
        let process_name = process.name.clone();
        let process_matcher = process.matcher.clone();
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                add_manual_application(&ui, process_name.clone(), process_matcher.clone());
            }
            picker.close();
        });
        row.set_child(Some(&button));
        entries.borrow_mut().push((
            row.clone(),
            format!("{} {}", process.name, process.matcher).to_lowercase(),
        ));
        list.append(&row);
    }
    let visible_entries = Rc::clone(&entries);
    search.connect_search_changed(move |search| {
        let query = search.text().trim().to_lowercase();
        for (row, haystack) in visible_entries.borrow().iter() {
            row.set_visible(query.is_empty() || haystack.contains(&query));
        }
    });
    let scroll = content_scroll(&list);
    body.append(&scroll);
    dialog.content_area().append(&body);
    dialog.connect_response(move |dialog, _| {
        dialog.close();
    });
    dialog.present();
    search.grab_focus();
}

fn show_import_dialog(ui: &Rc<Ui>) {
    let dialog = gtk::Dialog::builder()
        .title("Добавить серверы")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(620)
        .default_height(390)
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Импортировать", gtk::ResponseType::Accept);
    let box_ = gtk::Box::new(Orientation::Vertical, 10);
    box_.add_css_class("dialog-box");
    let note = label_x(
        "Вставьте VLESS, VMess, Trojan, Shadowsocks или SOCKS — по одной ссылке на строку.",
        0.0,
    );
    note.set_wrap(true);
    let view = gtk::TextView::new();
    view.set_vexpand(true);
    view.set_wrap_mode(gtk::WrapMode::Char);
    box_.append(&note);
    box_.append(&view);
    dialog.content_area().append(&box_);
    let data = Arc::clone(&ui.data);
    let paths = ui.paths.clone();
    let tx = ui.events.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let buffer = view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            if let Ok(profiles) = parse_many(&text) {
                let mut added = 0;
                if let Ok(mut state) = data.lock() {
                    let keys: HashSet<_> =
                        state.profiles.iter().map(Profile::endpoint_key).collect();
                    let new_profiles = profiles
                        .into_iter()
                        .filter(|profile| !keys.contains(&profile.endpoint_key()))
                        .collect::<Vec<_>>();
                    added = new_profiles.len();
                    state.profiles.extend(new_profiles);
                    if state.selected_profile.is_none() {
                        state.selected_profile = state.profiles.first().map(|profile| profile.id);
                    }
                    let _ = save_state(&paths, &state);
                }
                let _ = tx.send(UiEvent::DataChanged);
                let _ = tx.send(UiEvent::Notice(format!("Импортировано серверов: {added}")));
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn show_subscription_dialog(ui: &Rc<Ui>) {
    let dialog = gtk::Dialog::builder()
        .title("Новая подписка")
        .transient_for(&ui.window)
        .modal(true)
        .default_width(560)
        .build();
    dialog.add_button("Отмена", gtk::ResponseType::Cancel);
    dialog.add_button("Добавить", gtk::ResponseType::Accept);
    let box_ = gtk::Box::new(Orientation::Vertical, 10);
    box_.add_css_class("dialog-box");
    let url = gtk::Entry::builder()
        .placeholder_text("https://example.org/sub/…")
        .build();
    let name_note = label_x(
        "Название загрузится из подписки. Если провайдер его не передаёт, NORY создаст случайное имя.",
        0.0,
    );
    name_note.add_css_class("muted");
    name_note.set_wrap(true);
    let send_hwid = gtk::Switch::new();
    send_hwid.set_active(true);
    let hwid_row = switch_row(
        "Передавать HWID",
        "Добавлять заголовки X-HWID и X-Device-Info только к этой подписке",
        &send_hwid,
    );
    let hwid_value = label_x(&format!("HWID: {}", ui.hwid), 0.0);
    hwid_value.add_css_class("muted");
    hwid_value.set_selectable(true);
    let error = label_x("", 0.0);
    error.add_css_class("status-error");
    error.set_wrap(true);
    error.set_visible(false);
    box_.append(&url);
    box_.append(&name_note);
    box_.append(&hwid_row);
    box_.append(&hwid_value);
    box_.append(&error);
    dialog.content_area().append(&box_);
    let data = Arc::clone(&ui.data);
    let paths = ui.paths.clone();
    let hwid = ui.hwid.clone();
    let tx = ui.events.clone();
    dialog.connect_response(move |dialog, response| {
        if response != gtk::ResponseType::Accept {
            dialog.close();
            return;
        }
        match subscription::new_subscription(url.text().as_str(), send_hwid.is_active()) {
            Ok(subscription) => {
                let id = subscription.id;
                if let Ok(mut state) = data.lock() {
                    state.subscriptions.push(subscription);
                    state.selected_subscription = Some(id);
                    state.selected_profile = None;
                    let _ = save_state(&paths, &state);
                }
                update_subscription_background(
                    Arc::clone(&data),
                    id,
                    tx.clone(),
                    true,
                    hwid.clone(),
                );
                dialog.close();
            }
            Err(fetch_error) => {
                error.set_text(&fetch_error.to_string());
                error.set_visible(true);
                url.grab_focus();
            }
        }
    });
    dialog.present();
}

fn show_remove_subscription_dialog(ui: &Rc<Ui>) {
    let subscription = ui.data.lock().ok().and_then(|data| {
        let id = active_subscription_id(&data)?;
        data.subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .map(|subscription| (subscription.id, subscription.name.clone()))
    });
    let Some((id, name)) = subscription else {
        ui.toast_overlay
            .add_toast(adw::Toast::new("Нет подписок для удаления"));
        return;
    };
    let dialog = gtk::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .buttons(gtk::ButtonsType::YesNo)
        .text(format!("Удалить подписку «{name}»?"))
        .secondary_text("Все серверы этой подписки также будут удалены.")
        .build();
    let state = Arc::clone(&ui.data);
    let paths = ui.paths.clone();
    let tx = ui.events.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Yes
            && let Ok(mut data) = state.lock()
        {
            let old_position = data
                .subscriptions
                .iter()
                .position(|subscription| subscription.id == id)
                .unwrap_or(0);
            data.subscriptions
                .retain(|subscription| subscription.id != id);
            data.profiles
                .retain(|profile| profile.subscription_id != Some(id));
            data.selected_subscription = if data.subscriptions.is_empty() {
                None
            } else {
                Some(data.subscriptions[old_position.min(data.subscriptions.len() - 1)].id)
            };
            data.selected_profile = data.selected_subscription.and_then(|subscription| {
                data.profiles
                    .iter()
                    .find(|profile| profile.subscription_id == Some(subscription))
                    .map(|profile| profile.id)
            });
            let _ = save_state(&paths, &data);
            let _ = tx.send(UiEvent::DataChanged);
            let _ = tx.send(UiEvent::Notice("Подписка удалена".into()));
        }
        dialog.close();
    });
    dialog.present();
}

fn update_subscription_background(
    data: Arc<Mutex<AppData>>,
    id: Uuid,
    tx: mpsc::Sender<UiEvent>,
    added: bool,
    hwid: String,
) {
    std::thread::spawn(move || {
        let subscription = data.lock().ok().and_then(|state| {
            state
                .subscriptions
                .iter()
                .find(|item| item.id == id)
                .cloned()
        });
        let result = subscription
            .context("подписка не найдена")
            .and_then(|subscription| {
                updater::default_client()
                    .and_then(|client| subscription::fetch(&client, &subscription, Some(&hwid)))
            })
            .map_err(|error| error.to_string());
        let event = if added {
            UiEvent::SubscriptionAdded(id, result)
        } else {
            UiEvent::SubscriptionUpdated(id, result)
        };
        let _ = tx.send(event);
    });
}

fn content_scroll<W: IsA<gtk::Widget>>(content: &W) -> gtk::ScrolledWindow {
    // A fixed, narrow gutter keeps the thumb off controls, including when the
    // desktop theme or GTK_OVERLAY_SCROLLING requests traditional scrollbars.
    // Leave a little room for keyboard focus outlines inside the viewport.
    content.set_margin_start(3);
    content.set_margin_end(3);
    content.set_margin_top(3);
    content.set_margin_bottom(4);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .overlay_scrolling(false)
        .kinetic_scrolling(true)
        .vexpand(true)
        .child(content)
        .build();
    scroll.add_css_class("nory-scroll");
    let hide_timer = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let weak_scroll = scroll.downgrade();
    scroll.vadjustment().connect_value_changed(move |_| {
        let Some(scroll) = weak_scroll.upgrade() else {
            return;
        };
        scroll.add_css_class("scroll-active");
        if let Some(timer) = hide_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak_scroll = scroll.downgrade();
        let finished_timer = Rc::clone(&hide_timer);
        let timer = gtk::glib::timeout_add_local_once(Duration::from_millis(1000), move || {
            finished_timer.borrow_mut().take();
            if let Some(scroll) = weak_scroll.upgrade() {
                scroll.remove_css_class("scroll-active");
            }
        });
        hide_timer.replace(Some(timer));
    });
    scroll
}

fn page_box() -> gtk::Box {
    let page = gtk::Box::new(Orientation::Vertical, 12);
    page.add_css_class("page");
    page.set_hexpand(true);
    page.set_vexpand(true);
    page
}
fn label_x(text: &str, xalign: f32) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(xalign);
    label
}
fn app_icon(size: i32) -> gtk::Image {
    // Use the same asset as the launcher and tray, not a second drawing.
    let bytes =
        gtk::glib::Bytes::from_static(include_bytes!("../assets/icons/io.nory.NORY-128.png"));
    let image = match gtk::gdk::Texture::from_bytes(&bytes) {
        Ok(texture) => gtk::Image::from_paintable(Some(&texture)),
        Err(_) => gtk::Image::from_icon_name("io.nory.NORY"),
    };
    image.set_pixel_size(size);
    image
}
fn primary_button(label: &str) -> gtk::Button {
    let button = text_button(label);
    button.add_css_class("primary");
    button
}
fn action_button(label: &str, icon: LineIcon) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("toolbar-button");
    button.set_child(Some(&action_button_content(label, icon)));
    button
}
fn compact_icon_button(icon: LineIcon) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.set_child(Some(&line_icon(icon, 16)));
    button
}
fn action_button_content(label: &str, icon: LineIcon) -> gtk::Box {
    let content = gtk::Box::new(Orientation::Horizontal, 7);
    content.set_halign(Align::Center);
    content.append(&line_icon(icon, 17));
    let label = label_x(label, 0.0);
    label.add_css_class("settings-title");
    content.append(&label);
    content
}
fn text_button(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("toolbar-button");
    button
}
fn spin(min: f64, max: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(min, max, 1.0);
    spin.set_numeric(true);
    spin
}

fn heading(title: &str, subtitle: &str) -> (gtk::Box, gtk::Box) {
    let heading = gtk::Box::new(Orientation::Horizontal, 12);
    heading.add_css_class("page-heading");
    let copy = gtk::Box::new(Orientation::Vertical, 2);
    copy.set_hexpand(true);
    let title = label_x(title, 0.0);
    title.add_css_class("page-title");
    let subtitle = label_x(subtitle, 0.0);
    subtitle.add_css_class("muted");
    subtitle.set_wrap(true);
    subtitle.set_max_width_chars(55);
    copy.append(&title);
    copy.append(&subtitle);
    let actions = gtk::Box::new(Orientation::Horizontal, 8);
    actions.set_valign(Align::Center);
    heading.append(&copy);
    (heading, actions)
}
fn section_title(text: &str) -> gtk::Label {
    let label = label_x(text, 0.0);
    label.add_css_class("section-title");
    label
}

fn switch_row(title: &str, subtitle: &str, switch: &gtk::Switch) -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    row.add_css_class("settings-row");
    let copy = gtk::Box::new(Orientation::Vertical, 2);
    copy.set_hexpand(true);
    let title = label_x(title, 0.0);
    title.add_css_class("settings-title");
    let subtitle = label_x(subtitle, 0.0);
    subtitle.add_css_class("muted");
    subtitle.set_wrap(true);
    copy.append(&title);
    copy.append(&subtitle);
    switch.set_valign(Align::Center);
    row.append(&copy);
    row.append(switch);
    row
}
fn field_row<W: IsA<gtk::Widget>>(title: &str, widget: &W) -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    row.add_css_class("settings-row");
    let label = label_x(title, 0.0);
    label.set_hexpand(true);
    label.set_wrap(true);
    label.set_max_width_chars(36);
    label.add_css_class("settings-title");
    row.append(&label);
    row.append(widget);
    widget.set_valign(Align::Center);
    row
}

fn navigation_bar(stack: &gtk::Stack) -> gtk::Box {
    let nav = [
        ("home", LineIcon::Globe, "Подключение"),
        ("applications", LineIcon::Server, "Обход"),
        ("logs", LineIcon::Logs, "Логи"),
        ("settings", LineIcon::Settings, "Настройки"),
    ];
    let bottom_nav = gtk::Box::new(Orientation::Horizontal, 4);
    bottom_nav.add_css_class("bottom-nav");
    bottom_nav.set_halign(Align::Center);
    bottom_nav.set_valign(Align::End);
    let mut previous: Option<gtk::ToggleButton> = None;
    for (index, (name, icon, title)) in nav.iter().enumerate() {
        let button = gtk::ToggleButton::new();
        let content = gtk::Box::new(Orientation::Horizontal, 7);
        content.set_halign(Align::Center);
        content.append(&line_icon(*icon, 18));
        let label = label_x(title, 0.0);
        label.add_css_class("nav-label");
        content.append(&label);
        button.set_child(Some(&content));
        button.set_tooltip_text(Some(title));
        button.add_css_class("nav-button");
        if let Some(first) = previous.as_ref() {
            button.set_group(Some(first));
        } else {
            previous = Some(button.clone());
        }
        if index == 0 {
            button.set_active(true);
        }
        let stack_clone = stack.clone();
        let page = name.to_string();
        button.connect_toggled(move |button| {
            if button.is_active() {
                stack_clone.set_visible_child_name(&page);
            }
        });
        bottom_nav.append(&button);
    }
    bottom_nav
}

fn page_clamp(page: &gtk::Widget) -> adw::Clamp {
    adw::Clamp::builder()
        .maximum_size(960)
        .tightening_threshold(720)
        .child(page)
        .build()
}

fn details_section(title: &str, content: &gtk::Box) -> gtk::Expander {
    let expander = gtk::Expander::builder().label(title).child(content).build();
    expander.add_css_class("details-section");
    content.set_margin_top(12);
    expander
}
fn empty_row(title: &str, subtitle: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    let box_ = gtk::Box::new(Orientation::Vertical, 5);
    box_.set_margin_top(32);
    box_.set_margin_bottom(32);
    let title = label_x(title, 0.5);
    title.add_css_class("settings-title");
    let subtitle = label_x(subtitle, 0.5);
    subtitle.add_css_class("muted");
    box_.append(&title);
    box_.append(&subtitle);
    row.set_child(Some(&box_));
    row
}
fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
fn format_time(timestamp: i64) -> String {
    let age = unix_now().saturating_sub(timestamp);
    if age < 60 {
        "только что".into()
    } else if age < 3600 {
        format!("{} мин назад", age / 60)
    } else if age < 86400 {
        format!("{} ч назад", age / 3600)
    } else {
        format!("{} дн назад", age / 86400)
    }
}
fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn format_bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value, UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn transport_label(profile: &Profile) -> String {
    if matches!(
        &profile.connection,
        ConnectionSpec::XrayJson { name } if name.eq_ignore_ascii_case("wireguard")
    ) {
        return "UDP".into();
    }
    match profile.stream.network.as_xray() {
        "raw" => "TCP".into(),
        value => value.to_uppercase(),
    }
}

fn profile_marker_and_name(name: &str) -> (Option<String>, String) {
    let trimmed = name.trim();
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return (None, String::new());
    };
    if first == '🌐' {
        return (
            Some(first.to_string()),
            chars.as_str().trim_start().to_string(),
        );
    }
    if is_regional_indicator(first)
        && let Some(second) = chars.next()
        && is_regional_indicator(second)
    {
        return (
            Some(format!(
                "{}{}",
                regional_indicator_letter(first),
                regional_indicator_letter(second)
            )),
            chars.as_str().trim_start().to_string(),
        );
    }
    (None, trimmed.to_string())
}

fn is_regional_indicator(value: char) -> bool {
    ('🇦'..='🇿').contains(&value)
}

fn regional_indicator_letter(value: char) -> char {
    char::from_u32(u32::from(value) - u32::from('🇦') + u32::from('A')).unwrap_or('?')
}

fn configure_text_rendering() {
    if let Some(settings) = gtk::Settings::default() {
        #[cfg(target_os = "windows")]
        settings.set_gtk_font_name(Some("Segoe UI 10"));
        // GTK >= 4.16 otherwise ignores the low-level font settings in its
        // automatic mode. Discover the enum at runtime to keep older Linux
        // GTK installations supported without adding a new ABI requirement.
        if let Some(property) = settings.find_property("gtk-font-rendering")
            && let Some(class) = gtk::glib::EnumClass::with_type(property.value_type())
            && let Some(manual) = class.to_value_by_nick("manual")
        {
            settings.set_property_from_value("gtk-font-rendering", &manual);
        }
        settings.set_gtk_xft_antialias(1);
        settings.set_gtk_xft_hinting(1);
        settings.set_gtk_xft_hintstyle(Some("hintslight"));
        // Grayscale coverage stays clean on fractional-DPI, rotated, OLED and
        // BGR displays; do not assume that every monitor has RGB subpixels.
        settings.set_gtk_xft_rgba(Some("none"));
        if settings.find_property("gtk-hint-font-metrics").is_some() {
            settings.set_property("gtk-hint-font-metrics", false);
        }
    }
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(APP_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            // Apply only inside NORY; leave desktop settings and fonts intact.
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
    }
}
#[cfg(target_os = "linux")]
fn open_path(path: &std::path::Path) -> std::io::Result<std::process::Child> {
    Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(target_os = "windows")]
fn open_path(path: &std::path::Path) -> Result<()> {
    nory::windows::open_path(path)
}

#[cfg(target_os = "windows")]
fn configure_autostart(enabled: bool) -> Result<()> {
    nory::windows::configure_autostart(enabled)
}

#[cfg(target_os = "linux")]
fn configure_autostart(enabled: bool) -> Result<()> {
    let base = directories::BaseDirs::new().context("не найден домашний каталог")?;
    let directory = base.config_dir().join("autostart");
    let path = directory.join("io.nory.NORY.desktop");
    if !enabled {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(&directory)?;
    let executable = std::env::current_exe()?;
    let executable = executable.to_string_lossy().replace('"', "\\\"");
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=NORY\nExec=\"{executable}\"\nIcon=io.nory.NORY\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ),
    )?;
    Ok(())
}
