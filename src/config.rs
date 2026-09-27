use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const KNOBS: usize = 5;
pub const CONTROLS: usize = 9; // 0-4 knobs, 5-8 sliders (HID analog index)

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub active: String,
    /// Ignore slider/knob changes of this many steps (0-255 scale) to stop twitching.
    pub deadband: u8,
    /// Apply physical positions to volumes when the device connects.
    pub apply_on_connect: bool,
    /// Max ms between two presses to count as a double press.
    pub double_press_ms: u64,
    /// How long a knob press must be held to count as a hold.
    pub hold_ms: u64,
    /// Ignore button contact chatter for this long after a press or release.
    pub button_debounce_ms: u64,
    /// Show the on-screen volume popup.
    pub osd: bool,
    /// "bottom" | "top"
    pub osd_position: String,
    /// Don't jump the volume when it was changed elsewhere; wait until the control passes it.
    pub pickup: bool,
    pub obs: Obs,
    pub profiles: BTreeMap<String, Profile>,
    /// Notification lights; shared by every profile.
    pub alerts: Vec<Alert>,
    /// A slider that picks the profile, in every profile.
    pub profile_slider: ProfileSlider,
}

/// Slide to switch profiles: the travel is split evenly between `profiles`, bottom first.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ProfileSlider {
    /// 0 = off, 1-4 = S1-S4.
    pub slider: u8,
    pub profiles: Vec<String>,
    /// Its light fills up to the slider's position in this color.
    pub color: String,
}

impl Default for ProfileSlider {
    fn default() -> Self {
        ProfileSlider { slider: 0, profiles: vec![], color: "#ffffff".into() }
    }
}

impl ProfileSlider {
    /// The slider's control index, when it's in use.
    pub fn control(&self) -> Option<usize> {
        ((1..=CONTROLS - KNOBS).contains(&(self.slider as usize)) && !self.profiles.is_empty()).then(|| KNOBS + self.slider as usize - 1)
    }
}

/// Light up / pulse some LEDs while an app wants attention.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Alert {
    pub enabled: bool,
    pub app: String,
    /// "flash" (taskbar button flashes) | "title" (window title matches `pattern`) |
    /// "notification" (a new Windows notification; `pattern` optionally names its source) |
    /// "mic" (the app, or any app when `app` is empty, is using a microphone)
    pub trigger: String,
    /// For "title": text to look for; empty = an unread count like "(3)".
    pub pattern: String,
    /// "k1".."k5", "s1".."s4", "logo"
    pub lights: Vec<String>,
    /// "pulse" | "blink" | "solid"
    pub effect: String,
    pub color: String,
    pub period_ms: u32,
    /// Stop showing after this many minutes (0 = until you switch to the app / the title clears).
    pub timeout_min: u32,
}

impl Default for Alert {
    fn default() -> Self {
        Alert {
            enabled: true, app: String::new(), trigger: "flash".into(), pattern: String::new(), lights: vec!["logo".into()],
            effect: "pulse".into(), color: "#5865f2".into(), period_ms: 1200, timeout_min: 0,
        }
    }
}

impl Alert {
    /// Does a window title satisfy this alert's title trigger?
    pub fn title_matches(&self, title: &str) -> bool {
        let p = self.pattern.trim();
        if !p.is_empty() {
            return title.to_lowercase().contains(&p.to_lowercase());
        }
        // An unread count: "(3)" anywhere in the title.
        title.split('(').skip(1).any(|rest| rest.split_once(')').is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
    }

    /// LED index for a light name: 0-8 = controls, 9 = logo.
    pub fn light_index(name: &str) -> Option<usize> {
        let n = name.trim().to_lowercase();
        if n == "logo" {
            return Some(CONTROLS);
        }
        let (kind, num) = n.split_at(1.min(n.len()));
        let num: usize = num.parse().ok()?;
        match kind {
            "k" if (1..=KNOBS).contains(&num) => Some(num - 1),
            "s" if (1..=CONTROLS - KNOBS).contains(&num) => Some(KNOBS + num - 1),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Obs {
    pub url: String,
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Profile {
    /// Switch to this profile while one of these exes has focus.
    pub auto_apps: Vec<String>,
    pub controls: Vec<Control>,
    pub lighting: Lighting,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Control {
    pub label: String,
    pub turn: Turn,
    pub min: f32,
    pub max: f32,
    pub invert: bool,
    /// "linear" | "log" (finer control at low volume)
    pub curve: String,
    pub press: Vec<Action>,
    pub double: Vec<Action>,
    pub hold: Vec<Action>,
    pub light: Light,
    /// Slider label LED (sliders only).
    pub label_color: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Turn {
    #[default]
    None,
    /// exe names, or "focused", "system", "unmapped".
    App { #[serde(default)] apps: Vec<String> },
    /// "default", "default_capture", or part of a device name.
    Device { #[serde(default)] device: String },
    Obs { #[serde(default)] source: String },
    Voicemeeter {
        #[serde(default)] param: String,
        #[serde(default = "db_min")] min_db: f32,
        #[serde(default)] max_db: f32,
    },
    /// `{value}` (0-100) is substituted.
    Command { #[serde(default)] cmd: String },
    Brightness,
    /// One keystroke per step: `up` when turned right / slid up, `down` the other way.
    /// `steps` is how many steps the full travel has.
    Keys {
        #[serde(default)] up: String,
        #[serde(default)] down: String,
        #[serde(default = "steps")] steps: u8,
    },
    /// Dimmer: send the level to a web address as it changes (Home Assistant, Hue, webhooks).
    /// In the URL and body, {value} is 0-100, {value255} 0-255, {bri} 1-254 (Hue) and {on} true/false.
    Http {
        #[serde(default = "post")] method: String,
        #[serde(default)] url: String,
        #[serde(default)] headers: String,
        #[serde(default)] body: String,
    },
}
fn db_min() -> f32 { -60.0 }
fn steps() -> u8 { 24 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    MuteTurn,
    MuteApp { #[serde(default)] apps: Vec<String> },
    MuteDevice { #[serde(default)] device: String },
    SetDefault { #[serde(default)] device: String },
    CycleDefault { #[serde(default)] devices: Vec<String> },
    /// "ctrl+shift+m", "play_pause", "next", "prev", "vol_up", ...
    Keys { #[serde(default)] keys: String },
    Run { #[serde(default)] cmd: String },
    Kill { #[serde(default)] process: String },
    Profile { #[serde(default)] name: String },
    NextProfile,
    ObsScene { #[serde(default)] scene: String },
    ObsMute { #[serde(default)] source: String },
    ObsRecord,
    ObsStream,
    Voicemeeter { #[serde(default)] script: String },
    /// Bring the app's window to the front; start it (via `launch`, or the exe name) if it isn't running.
    FocusApp {
        #[serde(default)] app: String,
        #[serde(default)] launch: String,
        /// Minimize it instead when it's already in front.
        #[serde(default)] toggle: bool,
    },
    /// A URL, file or folder, opened with its default app.
    Open { #[serde(default)] target: String },
    TypeText { #[serde(default)] text: String },
    MuteMic,
    LockPc,
    /// Empty = every display (sleep); otherwise toggle these displays (\\.\DISPLAYn) over DDC/CI.
    MonitorOff { #[serde(default)] displays: Vec<String> },
    LightsToggle,
    /// Set app volume to a fixed level (0-100).
    SetAppVolume { #[serde(default)] apps: Vec<String>, #[serde(default = "half")] level: u8 },
    SetDeviceVolume { #[serde(default)] device: String, #[serde(default = "half")] level: u8 },
    /// Focus mode: mute every app except these; press again to unmute what it muted.
    MuteOthers { #[serde(default)] apps: Vec<String> },
    /// HTTP request (Home Assistant, smart lights, webhooks). `headers` is one "Name: value" per line.
    Http {
        #[serde(default = "post")] method: String,
        #[serde(default)] url: String,
        #[serde(default)] headers: String,
        #[serde(default)] body: String,
    },
    /// Route these apps to an output device; "default" hands them back to the Windows default.
    AppOutput { #[serde(default)] apps: Vec<String>, #[serde(default)] device: String },
    /// Use another profile while this knob is held (from Hold), or until it's pressed again (from a press).
    Shift { #[serde(default)] profile: String },
    /// Show what every control does: while held (from Hold), or for a few seconds (from a press).
    CheatSheet,
}
fn half() -> u8 { 50 }
fn post() -> String { "POST".into() }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Light {
    /// none | static | gradient (knob: dark->color by position) | volume (slider fill)
    pub mode: String,
    pub color: String,
    pub color2: String,
    /// When set and the turn target is muted, show this color instead.
    pub mute_color: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Lighting {
    /// custom | color | rainbow | wave | breath | visualizer
    pub mode: String,
    pub brightness: u8, // 0-100, global
    pub color: String,
    pub hue: u8,
    pub speed: u8,
    pub anim_brightness: u8,
    pub reverse: bool,
    pub bounce: bool,
    pub vertical: bool,
    pub logo: Logo,
    /// Visualizer colors: "rainbow", or "colors" (`color` when quiet to `color2` when loud).
    pub viz: String,
    pub color2: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Logo {
    /// none | static | rainbow | breath
    pub mode: String,
    pub color: String,
    pub hue: u8,
    pub speed: u8,
    pub brightness: u8,
}

impl Default for Obs {
    fn default() -> Self { Obs { url: "ws://127.0.0.1:4455".into(), password: String::new() } }
}

impl Default for Control {
    fn default() -> Self {
        Control {
            label: String::new(), turn: Turn::None, min: 0.0, max: 100.0, invert: false,
            curve: "linear".into(), press: vec![], double: vec![], hold: vec![], light: Light::default(), label_color: "#ffffff".into(),
        }
    }
}

impl Default for Light {
    fn default() -> Self {
        Light { mode: "static".into(), color: "#ffffff".into(), color2: "#000000".into(), mute_color: String::new() }
    }
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            mode: "custom".into(), brightness: 80, color: "#00aaff".into(), hue: 0, speed: 100,
            anim_brightness: 255, reverse: false, bounce: false, vertical: false, logo: Logo::default(),
            viz: "rainbow".into(), color2: "#ff2d95".into(),
        }
    }
}

impl Default for Logo {
    fn default() -> Self {
        Logo { mode: "static".into(), color: "#ffffff".into(), hue: 0, speed: 100, brightness: 255 }
    }
}

impl Default for Config {
    fn default() -> Self {
        let colors = ["#ff3b30", "#ff9500", "#ffcc00", "#34c759", "#00c7be", "#007aff", "#5856d6", "#af52de", "#ff2d55"];
        let mut controls: Vec<Control> = (0..CONTROLS)
            .map(|i| Control {
                light: Light { color: colors[i].into(), mute_color: "#ff0000".into(), ..Light::default() },
                ..Control::default()
            })
            .collect();
        let apps = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        controls[0].turn = Turn::Device { device: "default".into() };
        controls[1].turn = Turn::Device { device: "default_capture".into() };
        controls[2].turn = Turn::App { apps: apps(&["focused"]) };
        controls[3].press = vec![Action::Keys { keys: "play_pause".into() }];
        controls[3].double = vec![Action::Keys { keys: "next".into() }];
        controls[4].press = vec![Action::NextProfile];
        controls[5].turn = Turn::App { apps: apps(&["chrome.exe", "firefox.exe", "msedge.exe"]) };
        controls[6].turn = Turn::App { apps: apps(&["discord.exe"]) };
        controls[7].turn = Turn::App { apps: apps(&["spotify.exe"]) };
        controls[8].turn = Turn::App { apps: apps(&["unmapped"]) };
        for c in &mut controls[..3] {
            c.press = vec![Action::MuteTurn];
        }
        let mut profiles = BTreeMap::new();
        profiles.insert("Default".into(), Profile { controls, ..Profile::default() });
        Config {
            active: "Default".into(), deadband: 1, apply_on_connect: false, double_press_ms: 300, hold_ms: 500, button_debounce_ms: 50,
            osd: true, osd_position: "bottom".into(), pickup: true, obs: Obs::default(), profiles, alerts: vec![],
            profile_slider: ProfileSlider::default(),
        }
    }
}

impl Config {
    /// Make the config safe to use: an active profile exists and every profile has 9 controls.
    pub fn normalize(&mut self) {
        if self.profiles.is_empty() {
            *self = Config::default();
        }
        if !self.profiles.contains_key(&self.active) {
            self.active = self.profiles.keys().next().unwrap().clone();
        }
        for p in self.profiles.values_mut() {
            p.controls.resize_with(CONTROLS, Control::default);
            p.controls.truncate(CONTROLS);
            p.lighting.brightness = p.lighting.brightness.min(100);
        }
        self.double_press_ms = self.double_press_ms.clamp(100, 1000);
        self.hold_ms = self.hold_ms.clamp(250, 3000);
        // Older configs have no value (0); 50 ms matches the official software.
        self.button_debounce_ms = if self.button_debounce_ms == 0 { 50 } else { self.button_debounce_ms.clamp(5, 200) };
        // The panel's slider strips have five segments: up to five profiles, and only ones that exist.
        let profiles = &self.profiles;
        self.profile_slider.profiles.retain(|p| profiles.contains_key(p));
        self.profile_slider.profiles.truncate(5);
    }

    pub fn profile(&self) -> &Profile {
        &self.profiles[&self.active]
    }
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("pcpanel-revive").join("config.json")
}

pub fn load() -> Result<Config, String> {
    let p = path();
    let mut cfg = match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?,
        Err(_) => {
            let c = Config::default();
            save(&c)?;
            c
        }
    };
    cfg.normalize();
    Ok(cfg)
}

pub fn save(cfg: &Config) -> Result<(), String> {
    let p = path();
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

pub fn backup_dir() -> PathBuf {
    path().with_file_name("backups")
}

/// Copy the current config into backups/ (skipped when identical to the newest backup); keeps the last 10.
pub fn backup() {
    let Ok(current) = std::fs::read(path()) else { return };
    let dir = backup_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut list = backups();
    if list.first().is_some_and(|(name, _)| std::fs::read(dir.join(name)).ok().as_ref() == Some(&current)) {
        return;
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let _ = std::fs::write(dir.join(format!("config-{secs}.json")), &current);
    list = backups();
    for (name, _) in list.iter().skip(10) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Back up unless the newest backup is younger than `secs` (the settings window saves on every change).
pub fn backup_every(secs: u64) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if backups().first().is_none_or(|(_, t)| now.saturating_sub(*t) >= secs) {
        backup();
    }
}

/// (file name, unix seconds), newest first.
pub fn backups() -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = std::fs::read_dir(backup_dir()).into_iter().flatten().flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let secs = name.strip_prefix("config-")?.strip_suffix(".json")?.parse().ok()?;
            Some((name, secs))
        })
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// Level (0..1) a control sets for a raw position, after invert, curve and range.
pub fn level(c: &Control, raw: u8) -> f32 {
    let mut f = raw as f32 / 255.0;
    if c.invert {
        f = 1.0 - f;
    }
    if c.curve == "log" {
        f = f * f; // audio taper: half-way is 25%, so the quiet end gets more of the travel
    }
    (c.min + f * (c.max - c.min)) / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_title_and_lights() {
        let a = Alert { trigger: "title".into(), ..Alert::default() };
        assert!(a.title_matches("(3) Slack | general"));
        assert!(a.title_matches("Inbox (12) - Gmail"));
        assert!(!a.title_matches("#welcome | Server - Discord"));
        assert!(!a.title_matches("Notes (draft)"));
        let b = Alert { pattern: "unread".into(), ..a };
        assert!(b.title_matches("2 UNREAD messages"));
        assert_eq!(Alert::light_index("K2"), Some(1));
        assert_eq!(Alert::light_index("s4"), Some(8));
        assert_eq!(Alert::light_index("logo"), Some(9));
        assert_eq!(Alert::light_index("k6"), None);
    }

    #[test]
    fn level_applies_invert_curve_and_range() {
        let mut c = Control { min: 20.0, max: 80.0, ..Control::default() };
        assert!((level(&c, 0) - 0.2).abs() < 1e-6);
        assert!((level(&c, 255) - 0.8).abs() < 1e-6);
        c.invert = true;
        assert!((level(&c, 0) - 0.8).abs() < 1e-6);
        c = Control { curve: "log".into(), ..Control::default() };
        assert!((level(&c, 128) - 0.252).abs() < 0.01);
    }
}
