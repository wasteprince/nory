#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;

use adw::prelude::*;

fn main() {
    #[cfg(target_os = "windows")]
    if let Err(error) =
        nory::windows::require_windows_11().and_then(|_| nory::windows::configure_process())
    {
        nory::windows::show_error(&error.to_string());
        return;
    }
    wait_for_previous_version();
    #[cfg(target_os = "windows")]
    if std::env::args().any(|argument| argument == "--check-runtime") {
        match app::check_windows_runtime() {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("NORY runtime: {error:#}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(target_os = "windows")]
    if std::env::args().any(|argument| argument == "--check-service") {
        match nory::privileged::prepare_tun_core() {
            Ok(_) => std::process::exit(0),
            Err(error) => {
                eprintln!("NORY service: {error:#}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(target_os = "windows")]
    let instance = match nory::windows::SingleInstance::acquire() {
        Ok(Some(instance)) => std::rc::Rc::new(instance),
        Ok(None) => return,
        Err(error) => {
            nory::windows::show_error(&error.to_string());
            return;
        }
    };
    let application = adw::Application::builder()
        .application_id("io.nory.NORY")
        .build();
    #[cfg(target_os = "windows")]
    {
        application.set_flags(gtk4::gio::ApplicationFlags::NON_UNIQUE);
        let app = application.downgrade();
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            if let Some(app) = app.upgrade() {
                if instance.take_show_request() {
                    if let Some(window) = app.active_window() {
                        window.present();
                    } else {
                        app.activate();
                    }
                }
                gtk4::glib::ControlFlow::Continue
            } else {
                gtk4::glib::ControlFlow::Break
            }
        });
    }
    application.connect_activate(move |application| match app::present(application) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("NORY: {error:#}");
            #[cfg(target_os = "windows")]
            nory::windows::show_error(&format!("Не удалось запустить NORY: {error:#}"));
            application.quit();
        }
    });
    #[cfg(target_os = "linux")]
    application.run();
    #[cfg(target_os = "windows")]
    application.run_with_args(&["nory"]);
}

fn wait_for_previous_version() {
    let mut arguments = std::env::args_os();
    while let Some(argument) = arguments.next() {
        if argument != "--restart-after-pid" {
            continue;
        }
        let Some(pid) = arguments
            .next()
            .and_then(|pid| pid.to_string_lossy().parse::<u32>().ok())
        else {
            return;
        };
        for _ in 0..300 {
            if !process_exists(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        return;
    }
}

#[cfg(target_os = "linux")]
fn process_exists(pid: u32) -> bool {
    std::path::PathBuf::from(format!("/proc/{pid}")).exists()
}

#[cfg(target_os = "windows")]
fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: OpenProcess returns an owned handle or null; the owned handle is
    // closed immediately and no pointer arguments are involved.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}
