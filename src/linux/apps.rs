//! Apps for the picker on Linux: the ones playing audio. No icons yet.
use crate::audio::Audio;
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct App {
    pub exe: String,
    pub name: String,
    pub icon: String,
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
            apps.push(App { name: friendly_name(&s.exe), exe: s.exe, icon: String::new(), audio: true, path });
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        apps
    }
}

/// "firefox.exe" or /usr/lib/firefox/firefox -> "Firefox".
pub fn friendly_name(path: &str) -> String {
    let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".exe");
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}
