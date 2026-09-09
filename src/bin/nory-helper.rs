#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    let command = std::env::args().nth(1).unwrap_or_default();
    if let Err(error) = nory::privileged::run_helper(&command) {
        eprintln!("nory-helper: {error:#}");
        std::process::exit(1);
    }
}
