//! Apps for the picker on Linux: the ones playing audio, with the name and icon from their desktop
//! entry (the .desktop files app menus use).
use crate::audio::Audio;
use base64::Engine;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Serialize, Clone)]
pub struct App {
    pub exe: String,
    pub name: String,
    pub icon: String, // data: URI, "" if none
    pub audio: bool,
    pub path: String,
}

#[derive(Default)]
pub struct Cache;

impl Cache {
    pub fn list(&mut self, audio: Option<&Audio>) -> Vec<App> {
        let mut apps: Vec<App> = vec![];
        for s in audio.map(Audio::sessions).unwrap_or_default() {
            if s.exe.is_empty() || s.exe == "pcpanel-revive.exe" || apps.iter().any(|a| a.exe == s.exe) {
                continue;
            }
            let path = crate::audio::process_path(s.pid);
            let icon = icon_file(&s.exe).and_then(|f| data_uri(&f)).unwrap_or_default();
            apps.push(App { name: friendly_name(&s.exe), exe: s.exe, icon, audio: true, path });
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps
    }
}

/// What a desktop entry says about an app.
#[derive(Clone, Default)]
struct Entry {
    name: String,
    icon: String,
}

fn data_dirs() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.push(home.join(".local/share/flatpak/exports/share"));
    dirs.push("/var/lib/flatpak/exports/share".into());
    dirs.push("/var/lib/snapd/desktop".into());
    dirs
}

/// "firefox.exe" -> its desktop entry, matched on the program it runs, its window class or its file name.
fn entry(exe: &str) -> Option<Entry> {
    static ENTRIES: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    let prog = exe.trim_end_matches(".exe").to_lowercase();
    let map = ENTRIES.get_or_init(|| Mutex::new(read_entries()));
    let map = map.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&prog).cloned()
}

/// Every desktop entry, keyed by each lowercase name it can be found under.
fn read_entries() -> HashMap<String, Entry> {
    let mut out = HashMap::new();
    for dir in data_dirs().iter().rev() {
        let Ok(files) = std::fs::read_dir(dir.join("applications")) else { continue };
        for f in files.flatten().map(|f| f.path()).filter(|p| p.extension().is_some_and(|e| e == "desktop")) {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let (mut e, mut exec, mut class, mut main) = (Entry::default(), String::new(), String::new(), false);
            for line in text.lines() {
                if line.starts_with('[') {
                    main = line == "[Desktop Entry]";
                } else if main {
                    match line.split_once('=') {
                        Some(("Name", v)) => e.name = v.trim().into(),
                        Some(("Icon", v)) => e.icon = v.trim().into(),
                        Some(("Exec", v)) => exec = v.trim().into(),
                        Some(("StartupWMClass", v)) => class = v.trim().into(),
                        _ => {}
                    }
                }
            }
            if e.name.is_empty() {
                continue;
            }
            let stem = f.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            // "env FOO=1 /usr/bin/firefox %u" -> "firefox"
            let prog = exec.split_whitespace().find(|w| *w != "env" && !w.contains('=')).map(|p| p.rsplit('/').next().unwrap_or(p).to_lowercase());
            let keys = [prog, Some(class.to_lowercase()), Some(stem.rsplit('.').next().unwrap_or(&stem).to_string()), Some(stem.clone())];
            for k in keys.into_iter().flatten().filter(|k| !k.is_empty()) {
                out.insert(k, e.clone());
            }
        }
    }
    out
}

/// An icon file for an app, from its desktop entry and the icon theme folders.
pub fn icon_file(exe: &str) -> Option<PathBuf> {
    let icon = entry(exe)?.icon;
    if icon.starts_with('/') {
        return Path::new(&icon).exists().then(|| icon.into());
    }
    let sizes = ["48x48", "64x64", "128x128", "256x256", "32x32", "scalable"];
    for dir in data_dirs() {
        for theme in ["hicolor", "Adwaita", "breeze"] {
            for size in sizes {
                for ext in ["png", "svg"] {
                    let p = dir.join(format!("icons/{theme}/{size}/apps/{icon}.{ext}"));
                    if p.exists() {
                        return Some(p);
                    }
                }
            }
        }
        for ext in ["png", "svg", "xpm"] {
            let p = dir.join(format!("pixmaps/{icon}.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn data_uri(file: &Path) -> Option<String> {
    let mime = match file.extension()?.to_str()? {
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => return None,
    };
    let bytes = std::fs::read(file).ok().filter(|b| b.len() < 512 * 1024)?;
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// "firefox.exe" or /usr/lib/firefox/firefox -> "Firefox" (its desktop entry's name when there is one).
pub fn friendly_name(path: &str) -> String {
    let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".exe");
    if let Some(e) = entry(stem) {
        return e.name;
    }
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}
