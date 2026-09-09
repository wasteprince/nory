use crate::models::{ApplicationRule, ApplicationSource};
use anyhow::{Context, Result};
use directories::BaseDirs;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RunningProcess {
    pub name: String,
    pub matcher: String,
}

#[derive(Debug, Clone)]
pub struct InstalledApplication {
    pub name: String,
    pub matcher: String,
}

#[cfg(target_os = "linux")]
pub fn installed_applications() -> Vec<InstalledApplication> {
    let mut directories = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Some(base) = BaseDirs::new() {
        directories.push(base.data_dir().join("applications"));
        directories.push(
            base.home_dir()
                .join(".local/share/flatpak/exports/share/applications"),
        );
    }
    let mut applications = Vec::new();
    let mut seen = HashSet::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            if desktop_value(&text, "Hidden").is_some_and(|value| value == "true")
                || desktop_value(&text, "NoDisplay").is_some_and(|value| value == "true")
            {
                continue;
            }
            let Some(name) = desktop_value(&text, "Name") else {
                continue;
            };
            let matcher = desktop_value(&text, "TryExec")
                .or_else(|| desktop_value(&text, "Exec").and_then(|exec| executable_from(&exec)));
            let Some(matcher) = matcher else {
                continue;
            };
            if seen.insert(matcher.clone()) {
                applications.push(InstalledApplication { name, matcher });
            }
        }
    }
    applications.sort_by_key(|application| application.name.to_lowercase());
    applications
}

#[cfg(target_os = "windows")]
pub fn installed_applications() -> Vec<InstalledApplication> {
    crate::windows::installed_applications()
}

fn desktop_value(text: &str, key: &str) -> Option<String> {
    let mut in_desktop_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        let Some((field, value)) = line.split_once('=') else {
            continue;
        };
        if field == key {
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

fn executable_from(exec: &str) -> Option<String> {
    if let Some(command) = exec
        .split_whitespace()
        .find_map(|part| part.strip_prefix("--command="))
    {
        return Some(command.trim_matches(['\'', '"']).to_string());
    }
    let mut parts = exec
        .split_whitespace()
        .map(|part| part.trim_matches(['\'', '"']));
    let mut executable = parts.next()?;
    if executable == "env" {
        executable = parts.find(|part| !part.contains('=') && !part.starts_with('-'))?;
    }
    if executable.ends_with("flatpak") {
        return parts
            .rfind(|part| !part.starts_with('-') && !part.starts_with('%'))
            .map(ToOwned::to_owned);
    }
    (!executable.starts_with('%') && !executable.is_empty()).then(|| executable.to_string())
}

#[cfg(target_os = "linux")]
pub fn running_processes() -> Vec<RunningProcess> {
    let mut processes = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return processes;
    };
    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            continue;
        }
        let directory = entry.path();
        let name = fs::read_to_string(directory.join("comm"))
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let executable = fs::read_link(directory.join("exe"))
            .ok()
            .map(|value| value.to_string_lossy().into_owned());
        let Some(matcher) = executable.or_else(|| name.clone()) else {
            continue;
        };
        if !seen.insert(matcher.clone()) {
            continue;
        }
        let name = name.unwrap_or_else(|| {
            PathBuf::from(&matcher).file_name().map_or_else(
                || matcher.clone(),
                |value| value.to_string_lossy().into_owned(),
            )
        });
        processes.push(RunningProcess { name, matcher });
    }
    processes.sort_by_key(|process| process.name.to_lowercase());
    processes
}

#[cfg(target_os = "windows")]
pub fn running_processes() -> Vec<RunningProcess> {
    crate::windows::running_processes()
}

pub fn discover_steam_games() -> Result<Vec<ApplicationRule>> {
    let Some(base) = BaseDirs::new() else {
        return Ok(Vec::new());
    };
    let home = base.home_dir();
    #[cfg(target_os = "linux")]
    let steam_roots = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/debian-installation"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ];
    #[cfg(target_os = "windows")]
    let steam_roots = [
        std::env::var_os("PROGRAMFILES(X86)")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
            .join("Steam"),
        std::env::var_os("PROGRAMFILES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
            .join("Steam"),
        home.join("AppData/Local/Steam"),
        home.join("AppData/Roaming/Steam"),
    ];
    discover_from_roots(&steam_roots)
}

pub fn merge_steam_games(rules: &mut Vec<ApplicationRule>) -> Result<usize> {
    let discovered = discover_steam_games()?;
    let mut changed = 0;
    for game in discovered {
        if let Some(existing) = rules.iter_mut().find(|rule| rule.id == game.id) {
            if existing.name != game.name || existing.processes != game.processes {
                existing.name = game.name;
                existing.processes = game.processes;
                changed += 1;
            }
        } else {
            rules.push(game);
            changed += 1;
        }
    }
    rules.sort_by_key(|rule| rule.name.to_lowercase());
    Ok(changed)
}

fn discover_from_roots(roots: &[PathBuf]) -> Result<Vec<ApplicationRule>> {
    let mut steamapps = Vec::new();
    for root in roots {
        let directory = root.join("steamapps");
        if directory.is_dir() {
            steamapps.push(directory);
        }
    }
    let initial = steamapps.clone();
    for directory in initial {
        let libraries = directory.join("libraryfolders.vdf");
        let Ok(text) = fs::read_to_string(libraries) else {
            continue;
        };
        for path in vdf_values(&text, "path") {
            let directory = PathBuf::from(path).join("steamapps");
            if directory.is_dir() {
                steamapps.push(directory);
            }
        }
    }
    let mut seen_libraries = HashSet::new();
    steamapps.retain(|path| {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.clone());
        seen_libraries.insert(normalized)
    });

    let mut games = Vec::new();
    let mut seen_apps = HashSet::new();
    for directory in steamapps {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("не удалось прочитать {}", directory.display()))?
            .flatten()
        {
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if !file_name.starts_with("appmanifest_") || !file_name.ends_with(".acf") {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            let Some(app_id) = vdf_value(&text, "appid") else {
                continue;
            };
            if !seen_apps.insert(app_id.clone()) {
                continue;
            }
            let Some(name) = vdf_value(&text, "name") else {
                continue;
            };
            if is_steam_tool(&name) {
                continue;
            }
            let Some(install_dir) = vdf_value(&text, "installdir") else {
                continue;
            };
            let game_dir = directory.join("common").join(install_dir);
            if !game_dir.is_dir() {
                continue;
            }
            let game_dir = game_dir.canonicalize().unwrap_or(game_dir);
            let mut process_folder = game_dir.to_string_lossy().into_owned();
            if !process_folder.ends_with('/') {
                process_folder.push('/');
            }
            games.push(ApplicationRule {
                id: format!("steam:{app_id}"),
                name,
                processes: vec![process_folder],
                source: ApplicationSource::Steam,
                bypass: false,
            });
        }
    }
    games.sort_by_key(|game| game.name.to_lowercase());
    Ok(games)
}

fn vdf_value(text: &str, key: &str) -> Option<String> {
    vdf_values(text, key).into_iter().next()
}

fn vdf_values(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let quoted = quoted_values(line);
            (quoted.len() >= 2 && quoted[0].eq_ignore_ascii_case(key))
                .then(|| quoted[1].replace("\\\\", "\\"))
        })
        .collect()
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quoted {
            current.push(character);
            escaped = true;
        } else if character == '"' {
            if quoted {
                values.push(std::mem::take(&mut current));
            }
            quoted = !quoted;
        } else if quoted {
            current.push(character);
        }
    }
    values
}

fn is_steam_tool(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("proton ")
        || name.starts_with("steam linux runtime")
        || name.contains("steamworks common redistributables")
}
