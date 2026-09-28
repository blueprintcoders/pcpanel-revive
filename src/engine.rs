//! Turns device events into actions. Owns the config; runs on its own COM thread.
use crate::audio::{self, Audio, Session};
use crate::config::{self, Action, Alert, Config, Light, Logo, Profile, Turn, CONTROLS, KNOBS};
use crate::hid::{self, Event};
use crate::osd::{self, Info};
use crate::{log, obs::Obs, sonar::{self, Sonar}, sys, viz, wavelink::WaveLink, Msg, Shared, UiMsg};
use serde_json::json;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime};

const POLL: Duration = Duration::from_millis(500);
const CMD_INTERVAL: Duration = Duration::from_millis(150);
/// Frame time while an alert animates the LEDs.
const FRAME: Duration = Duration::from_millis(50);
const PREVIEW: Duration = Duration::from_secs(5);
/// Frame time while the music visualizer runs.
const VIZ_FRAME: Duration = Duration::from_millis(33);
/// A control moved while the visualizer drives its light shows its position for this long.
const SHOW_POSITION: Duration = Duration::from_millis(1500);
/// Pickup: how close the control must get to the current volume to take over.
const PICKUP_WINDOW: f32 = 0.03;
/// Pickup: a volume that moved this far from what we last set was changed elsewhere.
const EXTERNAL_CHANGE: f32 = 0.02;

#[derive(Clone, Copy, PartialEq)]
enum Press { Single, Double, Hold }

struct Engine {
    cfg: Config,
    shared: Arc<Mutex<Shared>>,
    post: Box<dyn Fn(UiMsg)>,
    audio: Audio,
    obs: Obs,
    wavelink: WaveLink,
    sonar: Sonar,
    sent: [Option<u8>; CONTROLS],
    pending: [Option<u8>; CONTROLS],
    last_cmd: [Option<Instant>; CONTROLS],
    muted: [bool; CONTROLS],
    press_deadline: [Option<Instant>; KNOBS],
    hold_deadline: [Option<Instant>; KNOBS],
    hold_fired: [bool; KNOBS],
    /// Button contacts chatter for a few ms; this turns raw edges into clean presses.
    debounce: [Debounce; KNOBS],
    /// Pickup state per control: taken over, level we last set, which side of the volume the control was on.
    engaged: [bool; CONTROLS],
    last_set: [Option<f32>; CONTROLS],
    side: [Option<f32>; CONTROLS],
    /// Startup sync ("apply positions on connect"): take over even though the volume differs.
    force_sync: [bool; CONTROLS],
    /// Keystroke turns: position of the last step, so each full step sends one key.
    step_anchor: [Option<f32>; CONTROLS],
    /// Profile to return to when the auto-switched app loses focus.
    auto_prev: Option<String>,
    cfg_mtime: Option<SystemTime>,
    /// Brightness to restore when the lights are toggled back on.
    lights_before: u8,
    /// Apps muted by focus mode, to unmute on the next press.
    focus_muted: Vec<String>,
    /// Apps whose taskbar button flashed and that haven't been switched to since.
    flashing: HashSet<String>,
    /// Apps with a new Windows notification since you last switched to them.
    notified: HashSet<String>,
    /// Newest notification seen per source; None until the first read (old ones don't count).
    notif_seen: Option<HashMap<String, i64>>,
    alerts_on: Vec<bool>,
    preview: Option<(usize, Instant)>,
    /// When each alert started showing, and whether it ran past its time limit.
    alert_since: Vec<Option<Instant>>,
    alert_expired: Vec<bool>,
    /// Latest known volume per control (for "follow the real volume" lights).
    levels: [Option<f32>; CONTROLS],
    /// Audio meters per control, refreshed every poll; smoothed peaks per frame.
    meters: Vec<Vec<audio::Meter>>,
    peaks: [f32; CONTROLS],
    connected: bool,
    /// Panel self-test: inputs don't run actions until this time.
    test_until: Option<Instant>,
    polls: u64,
    light_test: Option<usize>,
    anim_t0: Instant,
    /// Dimmer requests, sent in order on their own thread.
    http: Sender<(usize, [String; 4])>,
    /// Shift layer: (knob, profile to return to, ends on release rather than the next press).
    shift: Option<(usize, String, bool)>,
    /// The kind of press whose actions are running.
    pressing: Press,
    /// Profile slider: the range it's in.
    slider_seg: Option<usize>,
    /// Cheat sheet: the knob holding it open, or until when a press shows it.
    sheet_knob: Option<usize>,
    sheet_until: Option<Instant>,
    /// Music visualizer: the speaker capture and its analysis, the latest frame, and when to retry after a failure.
    viz: Option<(audio::Loopback, viz::Analyzer)>,
    viz_frame: viz::Frame,
    viz_buf: Vec<f32>,
    viz_retry: Option<Instant>,
    /// "While audio is playing": audio was heard recently enough to keep the visualizer on.
    playing: bool,
    playing_until: Option<Instant>,
    /// Locked, screens off, asleep: the lights go dark while any is true.
    idle: [bool; 3],
    /// Apps with sound as of the last poll; None before the first.
    known_pids: Option<HashSet<u32>>,
    /// When each control was last moved by hand, until its light goes back to the visualizer.
    moved_until: [Option<Instant>; CONTROLS],
}

pub fn run(rx: &Receiver<Msg>, cfg: Config, shared: Arc<Mutex<Shared>>, post: Box<dyn Fn(UiMsg)>) {
    audio::com_init();
    let audio = match Audio::new() {
        Ok(a) => a,
        Err(e) => return log(&shared, format!("audio init failed: {e}")),
    };
    let http = http_worker(shared.clone());
    let mut e = Engine {
        cfg, shared, post, audio, obs: Obs::default(), wavelink: WaveLink::default(), sonar: Sonar::default(), http, shift: None, pressing: Press::Single, slider_seg: None,
        sheet_knob: None, sheet_until: None, viz: None, viz_frame: viz::Frame::default(), viz_buf: vec![], viz_retry: None, playing: false, playing_until: None, moved_until: [None; CONTROLS], idle: [false; 3], known_pids: None,
        sent: [None; CONTROLS], pending: [None; CONTROLS], last_cmd: [None; CONTROLS], muted: [false; CONTROLS],
        press_deadline: [None; KNOBS], hold_deadline: [None; KNOBS], hold_fired: [false; KNOBS], debounce: [Debounce::default(); KNOBS],
        engaged: [false; CONTROLS], last_set: [None; CONTROLS], side: [None; CONTROLS], force_sync: [false; CONTROLS], step_anchor: [None; CONTROLS],
        auto_prev: None, cfg_mtime: mtime(), lights_before: 0, focus_muted: vec![],
        flashing: HashSet::new(), notified: HashSet::new(), notif_seen: None, alerts_on: vec![], preview: None, anim_t0: Instant::now(),
        alert_since: vec![], alert_expired: vec![], levels: [None; CONTROLS], meters: vec![], peaks: [0.0; CONTROLS],
        connected: false, test_until: None, light_test: None, polls: 0,
    };
    e.publish();
    let mut next_poll = Instant::now();
    let mut next_frame = Instant::now();
    loop {
        let now = Instant::now();
        let mut wake = next_poll;
        if e.animating() {
            wake = wake.min(next_frame);
        }
        for d in e.press_deadline.iter().chain(&e.hold_deadline).flatten() {
            wake = wake.min(*d);
        }
        let settle = Duration::from_millis(e.cfg.button_debounce_ms);
        for d in e.debounce.iter().filter_map(|d| d.pending(settle)) {
            wake = wake.min(d);
        }
        if e.pending.iter().any(Option::is_some) {
            wake = wake.min(now + Duration::from_millis(30));
        }
        match rx.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(m) => e.handle(m),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Coalesce: drain everything queued so only the latest position per control is applied.
        while let Ok(m) = rx.try_recv() {
            e.handle(m);
        }
        e.apply_turns();
        e.fire_due_presses();
        if Instant::now() >= next_poll {
            e.poll();
            next_poll = Instant::now() + POLL;
        }
        if e.animating() && Instant::now() >= next_frame {
            e.sample_meters();
            e.sample_viz();
            if e.preview.is_some_and(|(_, until)| Instant::now() >= until) && e.update_alerts() && !e.animating() {
                e.relight(); // preview ended: restore the normal lighting once
            } else {
                e.relight();
            }
            next_frame = Instant::now() + if e.viz_on() { VIZ_FRAME } else { FRAME };
        }
    }
}

/// Sends dimmer requests one at a time; while one is in flight, only the newest per control is kept.
fn http_worker(shared: Arc<Mutex<Shared>>) -> Sender<(usize, [String; 4])> {
    let (tx, rx) = channel::<(usize, [String; 4])>();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            let mut batch = vec![first];
            while let Ok(m) = rx.try_recv() {
                match batch.iter_mut().find(|b| b.0 == m.0) {
                    Some(b) => *b = m,
                    None => batch.push(m),
                }
            }
            for (_, [method, url, headers, body]) in batch {
                if let Err(e) = sys::http(&method, &url, &headers, &body) {
                    log(&shared, format!("dimmer: {e}"));
                }
            }
        }
    });
    tx
}

/// What GG calls a Sonar channel.
fn sonar_name(channel: &str) -> &str {
    sonar::CHANNELS.iter().find(|(id, _)| *id == channel).map_or(channel, |(_, name)| name)
}

/// Dimmer placeholders: {value} 0-100, {value255} 0-255, {bri} 1-254 (Philips Hue), {on} true/false.
fn fill_level(template: &str, v: f32) -> String {
    let v = v.clamp(0.0, 1.0);
    template
        .replace("{value255}", &((v * 255.0).round() as u32).to_string())
        .replace("{value}", &((v * 100.0).round() as u32).to_string())
        .replace("{bri}", &((v * 253.0).round() as u32 + 1).to_string())
        .replace("{on}", if v > 0.0 { "true" } else { "false" })
}

/// The profile as it should look at time `t` with the active alerts drawn on top.
fn alert_profile(p: &Profile, alerts: &[Alert], on: &[bool], t: f32) -> Profile {
    let mut f = p.clone();
    let l = &p.lighting;
    let solid = |c: String| Light { mode: "static".into(), color: c.clone(), color2: c, mute_color: String::new() };
    if l.mode != "custom" {
        // Knob rings can't mix a firmware animation with a single alert LED, so imitate it in software.
        let dim = l.anim_brightness as f32 / 255.0;
        let period = 2.0 + (255 - l.speed) as f32 / 255.0 * 8.0;
        for i in 0..CONTROLS {
            let c = match l.mode.as_str() {
                "color" => l.color.clone(),
                "breath" => hsv_hex(l.hue as f32 * 1.41, 1.0, dim * (0.55 - 0.45 * (t / period * std::f32::consts::TAU).cos())),
                _ => hsv_hex(i as f32 * 40.0 + l.hue as f32 * 1.41 + t / period * 360.0 * if l.reverse { -1.0 } else { 1.0 }, 1.0, dim),
            };
            f.controls[i].light = solid(c.clone());
            f.controls[i].label_color = c;
        }
        f.lighting.logo = match l.mode.as_str() {
            "color" => Logo { mode: "static".into(), color: l.color.clone(), ..Logo::default() },
            "breath" => Logo { mode: "breath".into(), hue: l.hue, speed: l.speed, brightness: l.anim_brightness, ..Logo::default() },
            _ => Logo { mode: "rainbow".into(), speed: l.speed, brightness: l.anim_brightness, ..Logo::default() },
        };
        f.lighting.mode = "custom".into();
    }
    for (a, _) in alerts.iter().zip(on).filter(|(_, on)| **on) {
        let c = scale_hex(&a.color, intensity(a, t));
        for idx in a.lights.iter().filter_map(|n| Alert::light_index(n)) {
            if idx == CONTROLS {
                f.lighting.logo = Logo { mode: "static".into(), color: c.clone(), ..Logo::default() };
            } else {
                f.controls[idx].light = solid(c.clone());
            }
        }
    }
    f
}

/// A light driven by a live level (audio peak or real volume), as plain colors the panel understands.
/// Slider strips only take two colors, so the level fills bottom-first: the top half lights past 50%.
pub(crate) fn live_light(l: &Light, v: f32, slider: bool) -> Light {
    let v = v.clamp(0.0, 1.0);
    let (color, color2) = if slider {
        (scale_hex(&l.color, (2.0 * v).min(1.0)), scale_hex(&l.color2, (2.0 * v - 1.0).max(0.0)))
    } else if l.mode == "level" {
        let c = mix_hex(&l.color, &l.color2, v);
        (c.clone(), c)
    } else {
        let c = scale_hex(&l.color, 0.06 + 0.94 * v);
        (c.clone(), c)
    };
    Light { mode: if slider { "gradient" } else { "static" }.into(), color, color2, mute_color: l.mute_color.clone() }
}

pub(crate) fn mix_hex(a: &str, b: &str, t: f32) -> String {
    let p = |h: &str, i: usize| h.trim_start_matches('#').get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0) as f32;
    let c = |i| (p(a, i) + (p(b, i) - p(a, i)) * t.clamp(0.0, 1.0)) as u8;
    format!("#{:02x}{:02x}{:02x}", c(0), c(2), c(4))
}

/// Self-test frame: everything off except one LED, lit white at full brightness.
fn test_profile(p: &Profile, led: usize) -> Profile {
    let mut f = p.clone();
    let off = Light { mode: "none".into(), color: String::new(), color2: String::new(), mute_color: String::new() };
    let white = Light { mode: "static".into(), color: "#ffffff".into(), color2: "#ffffff".into(), mute_color: String::new() };
    f.lighting.mode = "custom".into();
    f.lighting.brightness = 100;
    f.lighting.logo = Logo { mode: "none".into(), ..Logo::default() };
    for (i, c) in f.controls.iter_mut().enumerate() {
        c.light = if i == led { white.clone() } else { off.clone() };
        c.label_color = if i == led && i >= KNOBS { "#ffffff".into() } else { String::new() };
    }
    if led == CONTROLS {
        f.lighting.logo = Logo { mode: "static".into(), color: "#ffffff".into(), ..Logo::default() };
    }
    f
}

/// Button debouncer: the first edge counts at once; edges within `settle` of it are chatter,
/// and whatever state the contact settled in is applied when the window ends.
#[derive(Clone, Copy, Default)]
struct Debounce {
    raw: bool,
    state: bool,
    changed: Option<Instant>,
}

impl Debounce {
    /// A raw edge from the device. Returns a clean edge to act on now, if any.
    fn raw(&mut self, down: bool, now: Instant, settle: Duration) -> Option<bool> {
        self.raw = down;
        self.settle(now, settle)
    }
    /// Apply the settled state once the chatter window has passed.
    fn settle(&mut self, now: Instant, settle: Duration) -> Option<bool> {
        let quiet = self.changed.is_none_or(|t| now.duration_since(t) >= settle);
        if self.raw != self.state && quiet {
            self.state = self.raw;
            self.changed = Some(now);
            return Some(self.state);
        }
        None
    }
    /// When to check again for a state change that arrived during the window.
    fn pending(&self, settle: Duration) -> Option<Instant> {
        (self.raw != self.state).then(|| self.changed.map_or_else(Instant::now, |t| t + settle))
    }
}

pub(crate) fn hsv_hex(h: f32, s: f32, v: f32) -> String {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x), 3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x),
    };
    let m = v - c;
    format!("#{:02x}{:02x}{:02x}", ((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8)
}

pub(crate) fn scale_hex(hex: &str, k: f32) -> String {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0) as f32 * k.clamp(0.0, 1.0);
    format!("#{:02x}{:02x}{:02x}", p(0) as u8, p(2) as u8, p(4) as u8)
}

/// Alert brightness (0..1) at time `t` seconds.
fn intensity(a: &Alert, t: f32) -> f32 {
    let period = a.period_ms.clamp(200, 10_000) as f32 / 1000.0;
    match a.effect.as_str() {
        "solid" => 1.0,
        "blink" => if t % period < period / 2.0 { 1.0 } else { 0.0 },
        _ => 0.12 + 0.88 * (0.5 - 0.5 * (t / period * std::f32::consts::TAU).cos()),
    }
}

fn mtime() -> Option<SystemTime> {
    std::fs::metadata(config::path()).and_then(|m| m.modified()).ok()
}

/// Lowercase, add ".exe" to bare names; keeps the special targets.
fn norm(app: &str) -> String {
    let a = app.trim().to_lowercase();
    if a.contains('.') || matches!(a.as_str(), "focused" | "system" | "unmapped" | "") { a } else { a + ".exe" }
}

/// "spotify.exe" -> "Spotify"; the special targets get their names.
fn app_name(exe: &str) -> String {
    match norm(exe).as_str() {
        "focused" => "App in front".into(),
        "system" => "System sounds".into(),
        "unmapped" => "Everything else".into(),
        "msedge.exe" => "Edge".into(),
        e => {
            let stem = e.trim_end_matches(".exe");
            stem.chars().next().map_or(String::new(), |c| c.to_uppercase().collect::<String>() + &stem[c.len_utf8()..])
        }
    }
}

fn apps_name(apps: &[String]) -> String {
    apps.iter().map(|a| app_name(a)).collect::<Vec<_>>().join(", ")
}

fn device_name(spec: &str) -> String {
    match spec.trim().to_lowercase().as_str() {
        "" | "default" => "Speakers".into(),
        "default_capture" => "Mic".into(),
        "default_comm" => "Call speakers".into(),
        "default_comm_capture" => "Call mic".into(),
        _ => spec.trim().into(),
    }
}

/// What turning a control does, in a few words.
fn turn_text(t: &Turn) -> String {
    match t {
        Turn::None => String::new(),
        Turn::App { apps } => format!("{} volume", apps_name(apps)),
        Turn::Device { device } => format!("{} volume", device_name(device)),
        Turn::Obs { source } => format!("OBS: {source}"),
        Turn::Voicemeeter { param, .. } => format!("Voicemeeter {param}"),
        Turn::Command { .. } => "Runs a command".into(),
        Turn::Brightness => "Panel brightness".into(),
        Turn::Keys { up, .. } if up.contains("scroll") => if up.contains("ctrl") { "Zoom" } else { "Scroll" }.into(),
        Turn::Keys { up, down, .. } => format!("Keys {down} / {up}"),
        Turn::Http { .. } => "Smart light".into(),
        Turn::WaveLink { name, .. } => format!("Wave Link: {name}"),
        Turn::Sonar { channel, .. } => format!("Sonar: {}", sonar_name(channel)),
    }
}

fn key_name(keys: &str) -> String {
    match keys.trim() {
        "play_pause" => "Play/pause".into(),
        "next" => "Next track".into(),
        "prev" => "Previous track".into(),
        "stop" => "Stop".into(),
        "mute" => "Mute".into(),
        "vol_up" => "Volume up".into(),
        "vol_down" => "Volume down".into(),
        k => k.into(),
    }
}

/// What an action does, in a few words.
fn action_text(a: &Action) -> String {
    match a {
        Action::MuteTurn => "Mute".into(),
        Action::MuteApp { apps } => format!("Mute {}", apps_name(apps)),
        Action::MuteDevice { device } => format!("Mute {}", device_name(device)),
        Action::SetDefault { device } => format!("Use {}", device_name(device)),
        Action::CycleDefault { .. } => "Next audio device".into(),
        Action::Keys { keys } => key_name(keys),
        Action::Run { cmd } => format!("Run {}", cmd.trim_matches('"').rsplit(['\\', '/']).next().unwrap_or("")),
        Action::Kill { process } => format!("Close {}", app_name(process)),
        Action::Profile { name } => format!("Profile {name}"),
        Action::NextProfile => "Next profile".into(),
        Action::ObsScene { scene } => format!("OBS scene {scene}"),
        Action::ObsMute { source } => format!("OBS mute {source}"),
        Action::ObsRecord => "OBS record".into(),
        Action::ObsStream => "OBS stream".into(),
        Action::Voicemeeter { .. } => "Voicemeeter".into(),
        Action::FocusApp { app, .. } => format!("Open {}", app_name(app)),
        Action::Open { target } => format!("Open {}", target.trim_start_matches("https://").trim_start_matches("http://")),
        Action::TypeText { .. } => "Type text".into(),
        Action::MuteMic => "Mute mic".into(),
        Action::LockPc => "Lock PC".into(),
        Action::MonitorOff { .. } => "Displays off".into(),
        Action::LightsToggle => "Lights on/off".into(),
        Action::SetAppVolume { apps, level } => format!("{} to {level}%", apps_name(apps)),
        Action::SetDeviceVolume { device, level } => format!("{} to {level}%", device_name(device)),
        Action::MuteOthers { .. } => "Focus mode".into(),
        Action::Http { .. } => "Web request".into(),
        Action::AppOutput { apps, device } => format!("{} to {}", apps_name(apps), device_name(device)),
        Action::Shift { profile } => format!("Shift to {profile}"),
        Action::CheatSheet => "This cheat sheet".into(),
        Action::WaveLinkMute { name, .. } => format!("Mute {name}"),
        Action::SonarMute { channel, .. } => format!("Mute Sonar {}", sonar_name(channel)),
    }
}

/// Cheat sheet cards for the first `n` controls of the active profile.
fn sheet_rows(cfg: &Config, n: usize) -> Vec<osd::SheetItem> {
    let list = |v: &[Action]| v.iter().map(action_text).collect::<Vec<_>>().join(", ");
    // The light's color, or the app's blue when it's off or too dark to read.
    let rgb = |hex: &str| {
        let h = hex.trim_start_matches('#');
        let c = [0, 2, 4].map(|i| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0));
        if c.iter().all(|&v| v < 70) { [79, 157, 255] } else { c }
    };
    cfg.profile().controls.iter().take(n).enumerate().map(|(i, c)| {
        let tag = if i < KNOBS { format!("K{}", i + 1) } else { format!("S{}", i - KNOBS + 1) };
        let knob = i < KNOBS;
        if cfg.profile_slider.control() == Some(i) {
            let color = rgb(cfg.profile_slider.colors_for(&cfg.active).1);
            let lines = vec![cfg.profile_slider.profiles.join(", "), "Slide to switch".into()];
            return osd::SheetItem { tag, title: "Profiles".into(), lines, color, knob };
        }
        let what = turn_text(&c.turn);
        let label = c.label.trim();
        let (title, mut lines) = match (label, what.is_empty()) {
            ("", _) => (what, vec![]),
            (l, true) => (l.to_string(), vec![]),
            // The label already says it ("Scroll" / "Scroll"): no need to repeat it.
            (l, false) if l.eq_ignore_ascii_case(&what) => (l.to_string(), vec![]),
            (l, false) => (l.to_string(), vec![what]),
        };
        lines.extend([("Press", &c.press), ("Double", &c.double), ("Hold", &c.hold)].into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| format!("{k}: {}", list(v))));
        let color = if c.light.mode == "none" { rgb("") } else { rgb(&c.light.color) };
        osd::SheetItem { tag, title, lines, color, knob }
    }).collect()
}

/// How many of a slider's five segments the panel lights in fill mode: position / 51, rounded
/// (measured on a Pro).
fn lit(v: i32) -> usize {
    ((v.clamp(0, 255) as f32 / 51.0).round() as usize).min(5)
}

/// A light that shows the control's position, in its usual colors: a slider fills up to where it is,
/// a knob ring goes from dim to bright as it turns.
fn position_light(l: &Light, slider: bool) -> Light {
    let c1 = if l.color.is_empty() || l.mode == "none" { "#ffffff".to_string() } else { l.color.clone() };
    let two = matches!(l.mode.as_str(), "gradient" | "volume" | "level" | "meter") && !l.color2.is_empty();
    let c2 = if two { l.color2.clone() } else { c1.clone() };
    let (mode, color) = match slider {
        true => ("volume", c1),
        false if two => ("gradient", c1),
        false => ("gradient", scale_hex(&c1, 0.08)),
    };
    Light { mode: mode.into(), color, color2: c2, mute_color: String::new() }
}

/// Which of `n` profiles (up to five) a slider position picks: the first up to one segment lit (the
/// very bottom counts as one), the next with two, and so on; the last profile keeps the rest of the way
/// up. Going up, the profile changes exactly as the next segment lights. Going down, it holds on a few
/// steps past where the light goes off, so a slider resting on the line doesn't flip between two.
fn segment(value: u8, n: usize, current: Option<usize>) -> usize {
    let at = |v: i32| (lit(v).max(1) - 1).min(n.clamp(1, 5) - 1);
    let v = value as i32;
    match current {
        Some(c) if at(v) < c && at(v + 3) == c => c,
        _ => at(v),
    }
}

/// Keystroke turn: whole steps moved since `anchor` (positive = up) and the new anchor.
/// The anchor jumps a whole step at a time, so jitter around a step never repeats a key.
fn key_steps(anchor: f32, raw: u8, steps: u8) -> (i32, f32) {
    let size = 255.0 / steps.clamp(2, 128) as f32;
    let n = ((raw as f32 - anchor) / size).trunc() as i32;
    (n, anchor + n as f32 * size)
}

/// Is a microphone in use by this app (or by any app, for an empty `exe`)?
fn mic_matches(users: &[String], exe: &str) -> bool {
    let stem = exe.trim_end_matches(".exe");
    users.iter().any(|u| exe.is_empty() || u == exe || (!stem.is_empty() && u.contains(stem)))
}

/// Pickup decision. Returns true when the control should drive the volume now.
/// `side` remembers which side of the current volume the control was on, so a fast sweep across it still engages.
fn picks_up(target: f32, current: f32, side: &mut Option<f32>) -> bool {
    let d = target - current;
    let crossed = side.is_some_and(|s| s != d.signum());
    if d.abs() <= PICKUP_WINDOW || crossed {
        *side = None;
        true
    } else {
        *side = Some(d.signum());
        false
    }
}

impl Engine {
    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Hid(Event::Connected(c)) => {
                crate::lock(&self.shared).connected = c;
                self.connected = c;
                if c {
                    self.relight();
                }
                (self.post)(UiMsg::Refresh);
            }
            Msg::Hid(Event::Turn { index, value, initial }) => {
                crate::lock(&self.shared).values[index] = value;
                // Keystroke turns count steps from the last one; everything else just tracks the position.
                let keys = matches!(self.cfg.profile().controls[index].turn, Turn::Keys { .. });
                if !keys || initial || self.testing() || self.step_anchor[index].is_none() {
                    self.step_anchor[index] = Some(value as f32);
                }
                if self.cfg.profile_slider.control() == Some(index) {
                    if !self.testing() {
                        self.slider_pick(value);
                    }
                    return;
                }
                if !initial {
                    self.moved_until[index] = Some(Instant::now() + SHOW_POSITION);
                }
                if self.testing() {
                    self.sent[index] = Some(value);
                    return;
                }
                if initial && !self.cfg.apply_on_connect {
                    self.sent[index] = Some(value);
                    return;
                }
                if initial {
                    self.force_sync[index] = true; // the user asked for positions to win at connect
                }
                if let Some(s) = self.sent[index] {
                    let db = self.cfg.deadband;
                    if value.abs_diff(s) <= db && value != 0 && value != 255 {
                        return;
                    }
                }
                self.pending[index] = Some(value);
            }
            Msg::Hid(Event::Button { index, down }) => {
                let settle = Duration::from_millis(self.cfg.button_debounce_ms);
                if let Some(down) = self.debounce[index].raw(down, Instant::now(), settle) {
                    self.button(index, down);
                }
            }
            Msg::Profile(name) => {
                self.auto_prev = None;
                self.activate(&name, true);
            }
            Msg::Reload => match config::load() {
                Ok(cfg) => {
                    self.cfg_mtime = mtime();
                    self.set_config(cfg);
                    log(&self.shared, "config reloaded".into());
                }
                Err(e) => log(&self.shared, format!("config error: {e}")),
            },
            Msg::Flash(exe) => {
                if self.cfg.alerts.iter().any(|a| a.enabled && norm(&a.app) == exe) {
                    self.flashing.insert(exe);
                    if self.update_alerts() {
                        self.relight();
                    }
                }
            }
            Msg::PreviewAlert(n) => {
                if n < self.cfg.alerts.len() {
                    self.preview = Some((n, Instant::now() + PREVIEW));
                    self.update_alerts();
                    self.relight();
                }
            }
            Msg::TestMode(on) => {
                self.test_until = on.then(|| Instant::now() + Duration::from_secs(600));
                if !on {
                    self.light_test = None;
                    self.relight();
                }
                log(&self.shared, format!("panel self-test {}", if on { "started" } else { "finished" }));
            }
            Msg::Idle(what, on) => {
                let was = self.dark();
                self.idle[what as usize] = on;
                if self.dark() != was {
                    log(&self.shared, if was { "lights back on".into() } else { "lights off while the PC is idle".into() });
                    self.relight();
                }
            }
            Msg::TestLight(i) => {
                self.light_test = i;
                self.relight();
            }
            Msg::Test(i, action) => {
                self.pressing = Press::Single;
                if let Err(e) = self.action(i.min(CONTROLS - 1), action) {
                    log(&self.shared, format!("test: {e}"));
                }
                self.refresh_mutes();
                self.relight();
            }
        }
    }

    /// A clean (debounced) button edge.
    fn button(&mut self, index: usize, down: bool) {
        crate::lock(&self.shared).buttons[index] = down;
        if self.testing() {
            return;
        }
        if !down && self.sheet_knob == Some(index) {
            self.sheet_knob = None;
            self.popup(Info { hide: true, ..Default::default() });
        }
        // Leave a shift layer: let go of the held knob, or press a latched one again.
        if let Some((k, prev, momentary)) = self.shift.clone() {
            if k == index && down != momentary {
                self.shift = None;
                self.hold_deadline[index] = None;
                self.activate(&prev, false);
                return;
            }
        }
        let has_hold = !self.cfg.profile().controls[index].hold.is_empty();
        match (down, has_hold) {
            (true, true) => {
                self.hold_fired[index] = false;
                self.hold_deadline[index] = Some(Instant::now() + Duration::from_millis(self.cfg.hold_ms));
            }
            (true, false) => self.click(index),
            // Released before the hold time: it was a normal click.
            (false, true) if !self.hold_fired[index] && self.hold_deadline[index].take().is_some() => self.click(index),
            _ => {}
        }
    }

    /// A completed click: single, or the first/second half of a double.
    fn click(&mut self, i: usize) {
        if self.cfg.profile().controls[i].double.is_empty() {
            self.press(i, Press::Single);
        } else if self.press_deadline[i].take().is_some() {
            self.press(i, Press::Double);
        } else {
            self.press_deadline[i] = Some(Instant::now() + Duration::from_millis(self.cfg.double_press_ms));
        }
    }

    /// Profile slider moved: switch to the profile for its range.
    fn slider_pick(&mut self, value: u8) {
        let names = self.cfg.profile_slider.profiles.clone();
        let seg = segment(value, names.len(), self.slider_seg);
        self.slider_seg = Some(seg);
        if self.shift.is_none() && names[seg] != self.cfg.active {
            self.auto_prev = None;
            log(&self.shared, format!("profile slider at {value}: {}", names[seg]));
            self.activate(&names[seg], true);
        }
    }

    fn set_config(&mut self, cfg: Config) {
        if cfg.obs != self.cfg.obs {
            self.obs.disconnect();
        }
        self.cfg = cfg;
        self.update_alerts();
        self.publish();
    }

    /// Lights off: the PC is locked, its screens are off or it's asleep (unless that's turned off).
    fn dark(&self) -> bool {
        self.cfg.lights_off_idle && self.idle.iter().any(|&i| i)
    }

    fn animating(&self) -> bool {
        if self.dark() {
            return false; // nothing to animate while the lights are off
        }
        self.alerts_on.iter().any(|&on| on) || (self.connected && !self.meters.is_empty() && self.light_test.is_none()) || self.viz_on()
    }

    fn viz_on(&self) -> bool {
        self.connected && self.light_test.is_none() && self.viz_active()
    }

    /// Should the visualizer replace this profile's lighting right now?
    fn viz_active(&self) -> bool {
        match self.cfg.profile().lighting.viz_when.as_str() {
            "always" => true,
            "playing" => self.playing,
            _ => false,
        }
    }

    /// "While audio is playing": is one of the chosen apps (or any app) making sound? Stays on through
    /// short gaps between songs. Returns true when that changed.
    fn check_playing(&mut self) -> bool {
        let l = &self.cfg.profile().lighting;
        let (when, apps) = (l.viz_when.clone(), l.viz_apps.clone());
        if when == "playing" {
            let m = self.matcher(&apps);
            let any = apps.is_empty();
            let heard = self.audio.sessions().iter().any(|s| {
                (if any { !s.exe.is_empty() && s.exe != "system" } else { m(s) })
                    && !s.muted()
                    && s.meter.as_ref().is_some_and(|mt| audio::peak(mt) > 0.003)
            });
            if heard {
                self.playing_until = Some(Instant::now() + Duration::from_secs(3));
            }
        }
        let playing = when == "playing" && self.playing_until.is_some_and(|t| Instant::now() < t);
        std::mem::replace(&mut self.playing, playing) != playing
    }

    /// Music visualizer: read what's playing since the last frame and analyze it.
    fn sample_viz(&mut self) {
        if !self.viz_on() {
            self.viz = None;
            return;
        }
        if self.viz.is_none() && self.viz_retry.is_none_or(|t| Instant::now() >= t) {
            match self.audio.loopback() {
                Ok(lb) => {
                    let analyzer = viz::Analyzer::new(lb.rate);
                    self.viz = Some((lb, analyzer));
                }
                Err(e) => {
                    self.viz_retry = Some(Instant::now() + Duration::from_secs(5));
                    log(&self.shared, format!("visualizer: can't listen to the speakers: {}", e.message()));
                }
            }
        }
        let Some((lb, analyzer)) = &mut self.viz else { return };
        self.viz_buf.clear();
        let ok = lb.read(&mut self.viz_buf).is_ok();
        if ok {
            self.viz_frame = analyzer.feed(&self.viz_buf);
        } else {
            self.viz = None; // the device went away; reopen on the next frame
        }
    }

    /// New Windows notifications from apps that have a "notification" alert.
    fn check_notifications(&mut self) {
        let wanted: Vec<(String, String)> = self.cfg.alerts.iter()
            .filter(|a| a.enabled && a.trigger == "notification")
            .map(|a| (norm(&a.app), a.pattern.clone()))
            .collect();
        if wanted.is_empty() {
            self.notif_seen = None;
            return;
        }
        let sources = match crate::notif::sources() {
            Ok(s) => s,
            Err(e) => return log(&self.shared, format!("notifications: {e}")),
        };
        let first = self.notif_seen.is_none();
        let seen = self.notif_seen.get_or_insert_with(HashMap::new);
        for s in &sources {
            let prev = seen.insert(s.handler.clone(), s.latest).unwrap_or(0);
            if !first && s.latest > prev {
                for (exe, pattern) in &wanted {
                    if crate::notif::matches(&s.handler, exe, pattern) {
                        self.notified.insert(exe.clone());
                    }
                }
            }
        }
        seen.retain(|h, _| sources.iter().any(|s| &s.handler == h));
        // Dismissing / clearing an app's notifications clears its alert.
        self.notified.retain(|exe| {
            wanted.iter().filter(|(e, _)| e == exe)
                .any(|(e, p)| sources.iter().any(|s| s.count > 0 && crate::notif::matches(&s.handler, e, p)))
        });
    }

    fn testing(&self) -> bool {
        self.test_until.is_some_and(|t| Instant::now() < t)
    }

    /// Read every "pulse with audio" meter and smooth it (instant rise, gentle fall).
    fn sample_meters(&mut self) {
        let p = self.cfg.profile();
        for i in 0..CONTROLS {
            if p.controls[i].light.mode != "meter" {
                continue;
            }
            let peak = self.meters.get(i).map_or(0.0, |ms| ms.iter().map(audio::peak).fold(0.0f32, f32::max));
            let target = if peak <= 0.0001 { 0.0 } else { ((20.0 * peak.log10() + 48.0) / 48.0).clamp(0.0, 1.0) };
            let cur = self.peaks[i];
            self.peaks[i] = if target > cur { target } else { cur * 0.8 + target * 0.2 };
        }
        crate::lock(&self.shared).peaks = self.peaks;
    }

    /// Profile with "pulse with audio" and "follow the volume" lights resolved to plain colors.
    fn live_frame(&self, p: &Profile) -> Profile {
        let viz = self.viz_active();
        let mut f = if viz {
            let knobs = crate::lock(&self.shared).model.buttons();
            viz::paint(p, &self.viz_frame, self.anim_t0.elapsed().as_secs_f32(), knobs)
        } else {
            p.clone()
        };
        if f.lighting.mode != "custom" {
            return f;
        }
        // A control being moved shows where it is, not the music, for a moment.
        let now = Instant::now();
        for i in (0..CONTROLS).filter(|&i| viz && p.lighting.viz_on(i) && self.moved_until[i].is_some_and(|t| now < t)) {
            f.controls[i].light = position_light(&p.controls[i].light, i >= KNOBS);
        }
        // Meter and real-volume lights still run on the lights the visualizer leaves alone.
        for i in (0..CONTROLS).filter(|&i| !viz || !p.lighting.viz_on(i)) {
            let v = match p.controls[i].light.mode.as_str() {
                "meter" => self.peaks[i],
                "level" => self.levels[i].unwrap_or(0.0),
                _ => continue,
            };
            f.controls[i].light = live_light(&p.controls[i].light, v, i >= KNOBS);
        }
        // The profile slider: filled up to its position, in the active profile's color or a bottom-to-top blend.
        if let Some(i) = self.cfg.profile_slider.control() {
            let (low, high) = self.cfg.profile_slider.colors_for(&self.cfg.active);
            f.controls[i].light = Light { mode: "volume".into(), color: low.into(), color2: high.into(), mute_color: String::new() };
            f.controls[i].label_color = high.into();
        }
        f
    }

    /// Recompute which alerts are lit. Returns true if that changed.
    fn update_alerts(&mut self) -> bool {
        if self.preview.is_some_and(|(_, until)| Instant::now() >= until) {
            self.preview = None;
        }
        let n_alerts = self.cfg.alerts.len();
        self.alert_since.resize(n_alerts, None);
        self.alert_expired.resize(n_alerts, false);
        let mut on = vec![false; n_alerts];
        let mic = self.cfg.alerts.iter().any(|a| a.enabled && a.trigger == "mic").then(sys::mic_users).unwrap_or_default();
        for (n, a) in self.cfg.alerts.iter().enumerate() {
            let exe = norm(&a.app);
            let raw = a.enabled && match a.trigger.as_str() {
                "mic" => mic_matches(&mic, &exe),
                _ if exe.is_empty() => false,
                "title" => sys::window_titles(&exe).iter().any(|t| a.title_matches(t)),
                "notification" => self.notified.contains(&exe),
                _ => self.flashing.contains(&exe),
            };
            if !raw {
                self.alert_since[n] = None;
                self.alert_expired[n] = false;
            } else {
                let since = *self.alert_since[n].get_or_insert_with(Instant::now);
                if a.timeout_min > 0 && !self.alert_expired[n] && since.elapsed() >= Duration::from_secs(a.timeout_min as u64 * 60) {
                    self.alert_expired[n] = true;
                    log(&self.shared, format!("alert off after {} min: {}", a.timeout_min, a.app));
                    // A taskbar flash / notification is forgotten, so the next one lights it again.
                    self.flashing.remove(&exe);
                    self.notified.remove(&exe);
                }
            }
            on[n] = self.preview.is_some_and(|(p, _)| p == n) || (raw && !self.alert_expired[n]);
        }
        let changed = on != self.alerts_on;
        for (n, a) in self.cfg.alerts.iter().enumerate() {
            let (was, now) = (self.alerts_on.get(n).copied().unwrap_or(false), on[n]);
            if was != now && self.preview.is_none_or(|(p, _)| p != n) && !(was && self.alert_expired[n]) {
                let who = if a.app.is_empty() { "microphone in use" } else { &a.app };
                log(&self.shared, format!("alert {}: {who}", if now { "on" } else { "off" }));
            }
        }
        if changed && !on.iter().any(|&x| x) {
            self.anim_t0 = Instant::now();
        }
        self.alerts_on = on.clone();
        crate::lock(&self.shared).alerts_on = on;
        changed
    }

    fn alert_frame(&self, p: &Profile) -> Profile {
        alert_profile(p, &self.cfg.alerts, &self.alerts_on, self.anim_t0.elapsed().as_secs_f32())
    }

    /// Push config + lighting to shared state and refresh the tray.
    fn publish(&mut self) {
        crate::lock(&self.shared).config = self.cfg.clone();
        self.refresh_mutes();
        self.relight();
        (self.post)(UiMsg::Refresh);
    }

    fn relight(&self) {
        let model = crate::lock(&self.shared).model;
        let frame = if let Some(led) = self.light_test {
            test_profile(self.cfg.profile(), led)
        } else {
            let live = self.live_frame(self.cfg.profile());
            if self.alerts_on.iter().any(|&on| on) { self.alert_frame(&live) } else { live }
        };
        let mut frame = frame;
        if self.dark() {
            frame.lighting.brightness = 0;
        }
        let reports = hid::lighting(model, &frame, &self.muted);
        let mut s = crate::lock(&self.shared);
        s.lights = reports;
        s.lights_gen += 1;
        s.muted = self.muted;
    }

    fn osd(&self, info: Info) {
        if self.cfg.osd {
            self.popup(Info { top: self.cfg.osd_position == "top", ..info });
        }
    }

    fn popup(&self, info: Info) {
        crate::lock(&self.shared).osd = Some(info);
        (self.post)(UiMsg::Osd);
    }

    fn activate(&mut self, name: &str, persist: bool) {
        if !self.cfg.profiles.contains_key(name) || self.cfg.active == name {
            return;
        }
        self.cfg.active = name.to_string();
        if persist {
            let _ = config::save(&self.cfg);
            self.cfg_mtime = mtime();
        }
        self.engaged = [false; CONTROLS]; // controls may point somewhere new
        self.publish();
        self.osd(Info { title: name.to_string(), icon: osd::Icon::Profile, hint: "Profile".into(), ..Default::default() });
    }

    fn apply_turns(&mut self) {
        for i in 0..CONTROLS {
            let Some(v) = self.pending[i] else { continue };
            if matches!(self.cfg.profile().controls[i].turn, Turn::Command { .. } | Turn::Http { .. }) {
                if self.last_cmd[i].is_some_and(|t| t.elapsed() < CMD_INTERVAL) {
                    continue; // retried on the next wake
                }
                self.last_cmd[i] = Some(Instant::now());
            }
            self.pending[i] = None;
            self.sent[i] = Some(v);
            if let Err(e) = self.turn(i, v) {
                log(&self.shared, format!("{}: {e}", self.name(i)));
            }
        }
    }

    fn name(&self, i: usize) -> String {
        let l = &self.cfg.profile().controls[i].label;
        if !l.is_empty() { l.clone() } else if i < KNOBS { format!("Knob {}", i + 1) } else { format!("Slider {}", i - KNOBS + 1) }
    }

    fn turn(&mut self, i: usize, raw: u8) -> Result<(), String> {
        let c = self.cfg.profile().controls[i].clone();
        let v = config::level(&c, raw);
        let label = (!c.label.is_empty()).then(|| c.label.clone());
        match c.turn {
            Turn::None => {}
            Turn::App { apps } => {
                let m = self.matcher(&apps);
                let hits: Vec<Session> = self.audio.sessions().into_iter().filter(|s| m(s)).collect();
                let Some(first) = hits.first() else {
                    self.osd(Info { title: label.unwrap_or_else(|| apps.join(", ")), hint: "Not running".into(), ..Default::default() });
                    return Ok(());
                };
                let icon = osd::Icon::Exe(audio::process_path(first.pid));
                let (title, exe_name) = (label.clone().unwrap_or_else(|| first.exe.clone()), label.is_none());
                if !self.pickup(i, v, first.volume()) {
                    let cur = first.volume();
                    self.osd(Info { title, exe_name, icon, level: Some(cur), marker: Some(v), hint: format!("Move to {}% to take over", (cur * 100.0).round()), ..Default::default() });
                    return Ok(());
                }
                hits.iter().for_each(|s| s.set_volume(v));
                self.set_level(i, Some(v));
                self.osd(Info { title, exe_name, icon, level: Some(v), muted: first.muted(), ..Default::default() });
            }
            Turn::Device { device } => {
                let Some(dev) = self.audio.find(&device) else { return Err(format!("audio device '{device}' not found")) };
                let cur = self.audio.device_volume(&dev.id).unwrap_or(v);
                let icon = if dev.capture { osd::Icon::Mic } else { osd::Icon::Speaker };
                let title = label.unwrap_or_else(|| dev.name.clone());
                if !self.pickup(i, v, cur) {
                    self.osd(Info { title, icon, level: Some(cur), marker: Some(v), hint: format!("Move to {}% to take over", (cur * 100.0).round()), ..Default::default() });
                    return Ok(());
                }
                self.audio.set_device_volume(&dev.id, v).map_err(|e| e.message())?;
                self.set_level(i, Some(v));
                let muted = self.audio.device_muted(&dev.id).unwrap_or(false);
                self.osd(Info { title, icon, level: Some(v), muted, ..Default::default() });
            }
            Turn::Obs { source } => {
                let db = if v <= 0.0 { -100.0 } else { -60.0 + 60.0 * v.min(1.0) };
                self.obs.request(&self.cfg.obs, "SetInputVolume", json!({"inputName": source, "inputVolumeDb": db}))?;
                self.osd(Info { title: label.unwrap_or(format!("OBS: {source}")), icon: osd::Icon::Speaker, level: Some(v), ..Default::default() });
            }
            Turn::Voicemeeter { param, min_db, max_db } => {
                sys::voicemeeter(&format!("{param}={:.1};", min_db + v.clamp(0.0, 1.0) * (max_db - min_db)))?;
                self.osd(Info { title: label.unwrap_or(param), icon: osd::Icon::Speaker, level: Some(v), ..Default::default() });
            }
            Turn::Command { cmd } => sys::run(&cmd.replace("{value}", &format!("{}", (v * 100.0).round() as i32)))?,
            Turn::WaveLink { channel, mix, name } => {
                self.wavelink.set_level(&channel, &mix, v)?;
                self.osd(Info { title: label.unwrap_or(name), icon: osd::Icon::Speaker, level: Some(v), ..Default::default() });
            }
            Turn::Sonar { channel, mix } => {
                self.sonar.set_level(&channel, &mix, v)?;
                self.osd(Info { title: label.unwrap_or_else(|| format!("Sonar {}", sonar_name(&channel))), icon: osd::Icon::Speaker, level: Some(v), ..Default::default() });
            }
            Turn::Http { method, url, headers, body } => {
                let _ = self.http.send((i, [method, fill_level(&url, v), headers, fill_level(&body, v)]));
                self.osd(Info { title: label.unwrap_or("Lights".into()), icon: osd::Icon::Light, level: Some(v), ..Default::default() });
            }
            Turn::Keys { up, down, steps } => {
                let (mut n, anchor) = key_steps(self.step_anchor[i].unwrap_or(raw as f32), raw, steps);
                self.step_anchor[i] = Some(anchor);
                if c.invert {
                    n = -n;
                }
                let key = if n > 0 { up } else { down };
                if !key.trim().is_empty() {
                    for _ in 0..n.unsigned_abs().min(32) {
                        sys::send_keys(&key)?;
                    }
                }
            }
            Turn::Brightness => {
                let active = self.cfg.active.clone();
                self.cfg.profiles.get_mut(&active).unwrap().lighting.brightness = (v * 100.0).clamp(0.0, 100.0) as u8;
                self.relight(); // not saved to disk; saving on every tick would be wasteful
                self.osd(Info { title: label.unwrap_or("LED brightness".into()), icon: osd::Icon::Light, level: Some(v), ..Default::default() });
            }
        }
        Ok(())
    }

    /// Soft takeover: after the volume changed elsewhere, ignore the control until it reaches that volume.
    fn pickup(&mut self, i: usize, target: f32, current: f32) -> bool {
        if std::mem::take(&mut self.force_sync[i]) {
            self.engaged[i] = true;
            self.side[i] = None;
            return true;
        }
        if !self.cfg.pickup {
            return true;
        }
        let external = self.last_set[i].is_none_or(|l| (current - l).abs() > EXTERNAL_CHANGE);
        if external {
            self.engaged[i] = false;
        }
        if !self.engaged[i] {
            self.engaged[i] = picks_up(target, current, &mut self.side[i]);
        }
        self.engaged[i]
    }

    fn set_level(&mut self, i: usize, v: Option<f32>) {
        self.last_set[i] = v;
        self.levels[i] = v;
        if self.cfg.profile().controls[i].light.mode == "level" {
            self.relight();
        }
        let mut s = crate::lock(&self.shared);
        s.levels[i] = v;
        s.present[i] = true;
    }

    /// Session filter for a list of app targets.
    fn matcher(&self, apps: &[String]) -> impl Fn(&Session) -> bool {
        let mut names: Vec<String> = apps.iter().map(|a| norm(a)).collect();
        if names.iter().any(|a| a == "focused") {
            names.push(audio::foreground_exe());
        }
        let unmapped = names.iter().any(|a| a == "unmapped");
        let mapped: Vec<String> = self.cfg.profile().controls.iter()
            .filter_map(|c| match &c.turn { Turn::App { apps } => Some(apps), _ => None })
            .flatten()
            .map(|a| norm(a))
            .collect();
        move |s: &Session| {
            !s.exe.is_empty()
                && (names.contains(&s.exe) || (unmapped && s.exe != "system" && !mapped.contains(&s.exe)))
        }
    }

    fn fire_due_presses(&mut self) {
        let now = Instant::now();
        let settle = Duration::from_millis(self.cfg.button_debounce_ms);
        for i in 0..KNOBS {
            if let Some(down) = self.debounce[i].settle(now, settle) {
                self.button(i, down);
            }
        }
        for i in 0..KNOBS {
            if self.press_deadline[i].is_some_and(|d| d <= now) {
                self.press_deadline[i] = None;
                self.press(i, Press::Single);
            }
            if self.hold_deadline[i].is_some_and(|d| d <= now) {
                self.hold_deadline[i] = None;
                self.hold_fired[i] = true;
                self.press(i, Press::Hold);
            }
        }
    }

    fn press(&mut self, i: usize, kind: Press) {
        let c = &self.cfg.profile().controls[i];
        let actions = match kind { Press::Single => c.press.clone(), Press::Double => c.double.clone(), Press::Hold => c.hold.clone() };
        self.pressing = kind;
        for a in actions {
            if let Err(e) = self.action(i, a) {
                log(&self.shared, format!("{}: {e}", self.name(i)));
            }
        }
        self.refresh_mutes();
        self.relight();
    }

    fn action(&mut self, i: usize, a: Action) -> Result<(), String> {
        let obs = |s: &mut Self, kind: &str, data| s.obs.request(&s.cfg.obs.clone(), kind, data);
        match a {
            Action::MuteTurn => match self.cfg.profile().controls[i].turn.clone() {
                Turn::App { apps } => self.toggle_app_mute(i, &apps),
                Turn::Device { device } => self.toggle_device_mute(i, &device)?,
                Turn::Obs { source } => obs(self, "ToggleInputMute", json!({"inputName": source}))?,
                _ => return Err("mute_turn needs an app, device or OBS turn action".into()),
            },
            Action::MuteApp { apps } => self.toggle_app_mute(i, &apps),
            Action::MuteDevice { device } => self.toggle_device_mute(i, &device)?,
            Action::SetDefault { device } => {
                let d = self.audio.find(&device).ok_or("audio device not found")?;
                self.audio.set_default(&d.id).map_err(|e| e.message())?;
                self.osd(Info { title: d.name, icon: if d.capture { osd::Icon::Mic } else { osd::Icon::Speaker }, hint: "Default device".into(), ..Default::default() });
            }
            Action::CycleDefault { devices } => {
                let found: Vec<_> = devices.iter().filter_map(|d| self.audio.find(d)).collect();
                let first = found.first().ok_or("none of the devices were found")?;
                let cur = self.audio.default_id(first.capture);
                let at = found.iter().position(|d| Some(&d.id) == cur.as_ref()).map_or(0, |p| (p + 1) % found.len());
                self.audio.set_default(&found[at].id).map_err(|e| e.message())?;
                log(&self.shared, format!("default device: {}", found[at].name));
                let icon = if found[at].capture { osd::Icon::Mic } else { osd::Icon::Speaker };
                self.osd(Info { title: found[at].name.clone(), icon, hint: "Default device".into(), ..Default::default() });
            }
            Action::Keys { keys } => sys::send_keys(&keys)?,
            Action::Run { cmd } => sys::run(&cmd)?,
            Action::Kill { process } => {
                let p = if process.trim().eq_ignore_ascii_case("focused") { audio::foreground_exe() } else { process };
                if p.is_empty() || p == "explorer.exe" {
                    return Err("won't kill that".into());
                }
                sys::kill(&p)?
            }
            Action::Profile { name } => {
                self.auto_prev = None;
                self.activate(&name, true);
            }
            Action::NextProfile => {
                let names: Vec<String> = self.cfg.profiles.keys().cloned().collect();
                let at = names.iter().position(|n| *n == self.cfg.active).map_or(0, |p| (p + 1) % names.len());
                self.auto_prev = None;
                self.activate(&names[at], true);
            }
            Action::ObsScene { scene } => obs(self, "SetCurrentProgramScene", json!({"sceneName": scene}))?,
            Action::ObsMute { source } => obs(self, "ToggleInputMute", json!({"inputName": source}))?,
            Action::ObsRecord => obs(self, "ToggleRecord", json!({}))?,
            Action::ObsStream => obs(self, "ToggleStream", json!({}))?,
            Action::Voicemeeter { script } => sys::voicemeeter(&script)?,
            Action::FocusApp { app, launch, toggle } => sys::focus_app(&norm(&app), &launch, toggle)?,
            Action::Open { target } => sys::open(&target)?,
            Action::TypeText { text } => sys::type_text(&text),
            Action::MuteMic => self.toggle_device_mute(i, "default_capture")?,
            Action::LockPc => sys::lock(),
            Action::MonitorOff { displays } if displays.is_empty() => sys::monitor_off()?,
            Action::MonitorOff { displays } => sys::toggle_displays(&displays)?,
            Action::LightsToggle => {
                let active = self.cfg.active.clone();
                let l = &mut self.cfg.profiles.get_mut(&active).unwrap().lighting;
                if l.brightness > 0 {
                    self.lights_before = l.brightness;
                    l.brightness = 0;
                } else {
                    l.brightness = if self.lights_before > 0 { self.lights_before } else { 80 };
                }
                self.relight(); // runtime only; a reload or profile switch restores the saved brightness
            }
            Action::SetAppVolume { apps, level } => {
                let m = self.matcher(&apps);
                let hits: Vec<Session> = self.audio.sessions().into_iter().filter(|s| m(s)).collect();
                let first = hits.first().ok_or("app isn't running")?;
                let v = level.min(100) as f32 / 100.0;
                hits.iter().for_each(|s| s.set_volume(v));
                self.osd(Info { title: first.exe.clone(), exe_name: true, icon: osd::Icon::Exe(audio::process_path(first.pid)), level: Some(v), ..Default::default() });
            }
            Action::SetDeviceVolume { device, level } => {
                let d = self.audio.find(&device).ok_or("audio device not found")?;
                let v = level.min(100) as f32 / 100.0;
                self.audio.set_device_volume(&d.id, v).map_err(|e| e.message())?;
                self.osd(Info { title: d.name, icon: if d.capture { osd::Icon::Mic } else { osd::Icon::Speaker }, level: Some(v), ..Default::default() });
            }
            Action::MuteOthers { apps } => {
                let m = self.matcher(&apps);
                let sessions = self.audio.sessions();
                let on = self.focus_muted.is_empty();
                if on {
                    for s in sessions.iter().filter(|s| !m(s) && !s.muted()) {
                        s.set_mute(true);
                        self.focus_muted.push(s.exe.clone());
                    }
                } else {
                    // Only unmute what focus mode muted, not apps the user muted themselves.
                    sessions.iter().filter(|s| self.focus_muted.contains(&s.exe)).for_each(|s| s.set_mute(false));
                    log(&self.shared, format!("focus mode off: unmuted {}", self.focus_muted.join(", ")));
                    self.focus_muted.clear();
                }
                if on {
                    let names = if self.focus_muted.is_empty() { "nothing".into() } else { self.focus_muted.join(", ") };
                    log(&self.shared, format!("focus mode on: muted {names}"));
                }
                self.osd(Info { title: "Focus mode".into(), icon: osd::Icon::Speaker, hint: if on { "On" } else { "Off" }.into(), ..Default::default() });
            }
            Action::Http { method, url, headers, body } => {
                let shared = self.shared.clone();
                std::thread::spawn(move || {
                    if let Err(e) = sys::http(&method, &url, &headers, &body) {
                        log(&shared, format!("web request: {e}"));
                    }
                });
            }
            Action::Shift { profile } => {
                if !self.cfg.profiles.contains_key(&profile) {
                    return Err(format!("shift: there's no profile called '{profile}'"));
                }
                if self.shift.is_none() && profile != self.cfg.active {
                    self.shift = Some((i, self.cfg.active.clone(), self.pressing == Press::Hold));
                    self.activate(&profile, false);
                }
            }
            Action::WaveLinkMute { channel, mix, name } => {
                let muted = self.wavelink.toggle_mute(&channel, &mix)?;
                self.osd(Info { title: name, icon: osd::Icon::Speaker, muted, hint: if muted { "Muted" } else { "Unmuted" }.into(), ..Default::default() });
            }
            Action::SonarMute { channel, mix } => {
                let muted = self.sonar.toggle_mute(&channel, &mix)?;
                self.osd(Info { title: format!("Sonar {}", sonar_name(&channel)), icon: osd::Icon::Speaker, muted, hint: if muted { "Muted" } else { "Unmuted" }.into(), ..Default::default() });
            }
            Action::CheatSheet => {
                let held = self.pressing == Press::Hold;
                if !held && self.sheet_until.is_some_and(|t| Instant::now() < t) {
                    self.sheet_until = None; // pressed again: close it
                    self.popup(Info { hide: true, ..Default::default() });
                } else {
                    self.sheet_knob = held.then_some(i);
                    let stay = if held { 120_000 } else { 8_000 };
                    self.sheet_until = (!held).then(|| Instant::now() + Duration::from_millis(stay));
                    let n = crate::lock(&self.shared).model.analogs();
                    let hint = if held { "Let go to close" } else { "Press again to close" };
                    let sheet = sheet_rows(&self.cfg, n);
                    self.popup(Info { title: self.cfg.active.clone(), hint: hint.into(), sheet, stay_ms: stay, ..Default::default() });
                }
            }
            Action::AppOutput { apps, device } => {
                let m = self.matcher(&apps);
                let hits: Vec<Session> = self.audio.sessions().into_iter().filter(|s| m(s)).collect();
                let first = hits.first().ok_or("app isn't running - start it first")?;
                let target = match device.trim() {
                    "" | "default" => None,
                    spec => Some(self.audio.find(spec).ok_or("audio device not found")?),
                };
                if target.as_ref().is_some_and(|d| d.capture) {
                    return Err("pick an output device, not a microphone".into());
                }
                let mut pids: Vec<u32> = hits.iter().map(|s| s.pid).filter(|&p| p != 0).collect();
                pids.dedup();
                audio::set_app_output(&pids, target.as_ref().map(|d| d.id.as_str())).map_err(|e| e.message())?;
                let to = target.map_or("Windows default".to_string(), |d| d.name);
                log(&self.shared, format!("{} now plays on {to}", first.exe));
                self.osd(Info { title: first.exe.clone(), exe_name: true, icon: osd::Icon::Exe(audio::process_path(first.pid)), hint: format!("Now on {to}"), ..Default::default() });
            }
        }
        Ok(())
    }

    fn toggle_app_mute(&self, i: usize, apps: &[String]) {
        let m = self.matcher(apps);
        let hits: Vec<Session> = self.audio.sessions().into_iter().filter(|s| m(s)).collect();
        let mute = hits.iter().any(|s| !s.muted());
        hits.iter().for_each(|s| s.set_mute(mute));
        if let Some(first) = hits.first() {
            let label = &self.cfg.profile().controls[i].label;
            let (title, exe_name) = if label.is_empty() { (first.exe.clone(), true) } else { (label.clone(), false) };
            self.osd(Info { title, exe_name, icon: osd::Icon::Exe(audio::process_path(first.pid)), level: Some(first.volume()), muted: mute, ..Default::default() });
        }
    }

    fn toggle_device_mute(&self, i: usize, device: &str) -> Result<(), String> {
        let d = self.audio.find(device).ok_or("audio device not found")?;
        self.audio.toggle_device_mute(&d.id).map_err(|e| e.message())?;
        let label = &self.cfg.profile().controls[i].label;
        self.osd(Info {
            title: if label.is_empty() { d.name.clone() } else { label.clone() },
            icon: if d.capture { osd::Icon::Mic } else { osd::Icon::Speaker },
            level: self.audio.device_volume(&d.id),
            muted: self.audio.device_muted(&d.id).unwrap_or(false),
            ..Default::default()
        });
        Ok(())
    }

    /// Update `muted`, plus the live level and "is it running" state for the settings window.
    /// Returns true if the mute state changed.
    fn refresh_mutes(&mut self) -> bool {
        let sessions = self.audio.sessions();
        self.new_apps_at_dial(&sessions);
        let (mut muted, mut levels, mut present) = ([false; CONTROLS], [None; CONTROLS], [true; CONTROLS]);
        for i in 0..CONTROLS {
            match &self.cfg.profile().controls[i].turn {
                Turn::App { apps } => {
                    let m = self.matcher(apps);
                    let hits: Vec<&Session> = sessions.iter().filter(|s| m(s)).collect();
                    present[i] = !hits.is_empty();
                    levels[i] = hits.first().map(|s| s.volume());
                    muted[i] = !hits.is_empty() && hits.iter().all(|s| s.muted());
                }
                Turn::Device { device } => {
                    let d = self.audio.find(device);
                    present[i] = d.is_some();
                    if let Some(d) = d {
                        levels[i] = self.audio.device_volume(&d.id);
                        muted[i] = self.audio.device_muted(&d.id).unwrap_or(false);
                    }
                }
                _ => {}
            }
        }
        {
            let mut s = crate::lock(&self.shared);
            s.levels = levels;
            s.present = present;
        }
        let p = self.cfg.profile();
        let level_lights = (0..CONTROLS).any(|i| p.controls[i].light.mode == "level") && p.lighting.mode == "custom";
        let levels_moved = levels.iter().zip(&self.levels).any(|(a, b)| (a.unwrap_or(0.0) - b.unwrap_or(0.0)).abs() > 0.005);
        self.levels = levels;
        // Meters for "pulse with audio" lights.
        let wants_meter = |i: usize| p.lighting.mode == "custom" && p.controls[i].light.mode == "meter";
        let meters: Vec<Vec<audio::Meter>> = (0..CONTROLS).map(|i| {
            if !wants_meter(i) {
                return vec![];
            }
            match &p.controls[i].turn {
                Turn::App { apps } => {
                    let m = self.matcher(apps);
                    sessions.iter().filter(|s| m(s)).filter_map(|s| s.meter.clone()).collect()
                }
                Turn::Device { device } => self.audio.device_meter(device).into_iter().collect(),
                _ => vec![],
            }
        }).collect();
        self.meters = if (0..CONTROLS).any(wants_meter) { meters } else { vec![] };
        let relight_levels = level_lights && levels_moved;
        // Only controls with a mute color change their LEDs.
        let p = self.cfg.profile();
        for (i, m) in muted.iter_mut().enumerate() {
            *m &= p.lighting.mode == "custom" && !p.controls[i].light.mute_color.is_empty();
        }
        let changed = muted != self.muted;
        self.muted = muted;
        changed || relight_levels
    }

    /// Apps that started playing sound since the last poll take their dial's level
    /// (not on the first poll, or everything already running would jump).
    fn new_apps_at_dial(&mut self, sessions: &[Session]) {
        let known = self.known_pids.replace(sessions.iter().map(|s| s.pid).collect());
        let Some(known) = known.filter(|_| self.cfg.new_apps_at_dial) else { return };
        let fresh: Vec<&Session> = sessions.iter().filter(|s| !known.contains(&s.pid)).collect();
        if fresh.is_empty() {
            return;
        }
        for (i, c) in self.cfg.profile().controls.iter().enumerate() {
            let (Turn::App { apps }, Some(raw)) = (&c.turn, self.sent[i]) else { continue };
            // "focused" follows whichever app is in front, so it has no level of its own to hand out.
            let apps: Vec<String> = apps.iter().filter(|a| norm(a) != "focused").cloned().collect();
            let m = self.matcher(&apps);
            for s in fresh.iter().filter(|s| m(s)) {
                s.set_volume(config::level(c, raw));
                log(&self.shared, format!("{} started at {}'s level", s.exe, self.name(i)));
            }
        }
    }

    /// Periodic work: auto profile switching, mute LEDs, live levels, external config edits.
    fn poll(&mut self) {
        // Every ~5 s: is the official PCPanel software fighting us for the panel?
        self.polls += 1;
        if self.polls % 10 == 1 {
            let running = !sys::official_app_pids().is_empty();
            let was = std::mem::replace(&mut crate::lock(&self.shared).official_running, running);
            if running && !was {
                log(&self.shared, "the official PCPanel software is also running - both apps react to the panel".into());
            }
        }
        let fg = audio::foreground_exe();
        if self.test_until.is_some_and(|t| Instant::now() >= t) {
            self.handle(Msg::TestMode(false));
        }
        // Switching to an app is what clears its taskbar flash / notification.
        self.flashing.remove(&fg);
        self.notified.remove(&fg);
        if self.polls % 4 == 0 {
            self.check_notifications();
        }
        if self.update_alerts() && !self.animating() {
            self.relight();
        }
        if !fg.is_empty() && fg != "pcpanel-revive.exe" {
            let hit = self.cfg.profiles.iter()
                .find(|(_, p)| p.auto_apps.iter().any(|a| norm(a) == fg))
                .map(|(n, _)| n.clone());
            match hit {
                Some(name) if name != self.cfg.active => {
                    if self.auto_prev.is_none() {
                        self.auto_prev = Some(self.cfg.active.clone());
                    }
                    self.activate(&name, false);
                }
                None => {
                    if let Some(prev) = self.auto_prev.take() {
                        self.activate(&prev, false);
                    }
                }
                _ => {}
            }
        }
        if self.refresh_mutes() | self.check_playing() {
            self.relight();
        }
        // The visualizer follows the default output device.
        if let Some((lb, _)) = &self.viz {
            if !self.viz_on() || self.audio.default_id(false).is_some_and(|id| id != lb.device) {
                self.viz = None;
            }
        }
        let m = mtime();
        if m.is_some() && m != self.cfg_mtime {
            self.handle(Msg::Reload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_lights() {
        let s = position_light(&Light { mode: "static".into(), color: "#00ff00".into(), color2: String::new(), mute_color: String::new() }, true);
        assert_eq!((s.mode.as_str(), s.color.as_str(), s.color2.as_str()), ("volume", "#00ff00", "#00ff00"), "slider fills in its color");
        let k = position_light(&Light { mode: "static".into(), color: "#ff0000".into(), color2: String::new(), mute_color: String::new() }, false);
        assert_eq!((k.mode.as_str(), k.color.as_str(), k.color2.as_str()), ("gradient", "#140000", "#ff0000"), "knob dim to bright");
        let g = position_light(&Light { mode: "gradient".into(), color: "#0000ff".into(), color2: "#ff00ff".into(), mute_color: String::new() }, false);
        assert_eq!((g.color.as_str(), g.color2.as_str()), ("#0000ff", "#ff00ff"), "a two-color knob keeps its colors");
    }

    #[test]
    fn cheat_sheet_rows() {
        let mut cfg = Config::default();
        cfg.normalize();
        let rows = sheet_rows(&cfg, 9);
        assert_eq!(rows.len(), 9);
        assert_eq!((rows[0].tag.as_str(), rows[0].title.as_str()), ("K1", "Speakers volume"));
        assert_eq!(rows[0].lines, ["Press: Mute"]);
        assert!(rows[0].knob && !rows[5].knob);
        assert_eq!(rows[0].color, [0xff, 0x3b, 0x30], "the knob's light color");
        assert_eq!(rows[3].lines, ["Press: Play/pause", "Double: Next track"]);
        assert_eq!(rows[7].title, "Spotify volume");
        let active = cfg.active.clone();
        cfg.profiles.get_mut(&active).unwrap().controls[5].label = "Browsers".into();
        let s1 = &sheet_rows(&cfg, 9)[5];
        assert_eq!((s1.title.as_str(), s1.lines[0].as_str()), ("Browsers", "Chrome, Firefox, Edge volume"));
        assert_eq!(sheet_rows(&cfg, 4).len(), 4, "Mini: four knobs");
        assert_eq!(action_text(&Action::Run { cmd: r#""C:\Tools\obs64.exe""#.into() }), "Run obs64.exe");
    }

    #[test]
    fn slider_segments_have_sticky_edges() {
        // Three profiles: the bottom up to one segment lit, then two lit, then the last keeps three to five.
        let picks: Vec<usize> = [0u8, 20, 30, 70, 80, 125, 130, 180, 255].iter().map(|&v| segment(v, 3, None)).collect();
        assert_eq!(picks, [0, 0, 0, 0, 1, 1, 2, 2, 2]);
        // Five profiles: one per segment, switching where the panel lights the next one.
        let picks: Vec<usize> = [0u8, 70, 80, 125, 130, 175, 182, 225, 235, 255].iter().map(|&v| segment(v, 5, None)).collect();
        assert_eq!(picks, [0, 0, 1, 1, 2, 2, 3, 3, 4, 4]);
        // The second segment lights at 77: going up, it switches right there.
        assert_eq!(segment(76, 2, Some(0)), 0);
        assert_eq!(segment(77, 2, Some(0)), 1, "switches as the segment lights");
        // Going down, it holds on until 3 below the edge.
        assert_eq!(segment(75, 2, Some(1)), 1, "jitter under the edge doesn't switch back");
        assert_eq!(segment(74, 2, Some(1)), 1);
        assert_eq!(segment(73, 2, Some(1)), 0);
        assert_eq!(segment(250, 3, Some(0)), 2, "a jump far away switches at once");
    }

    #[test]
    fn dimmer_placeholders() {
        assert_eq!(fill_level(r#"{"brightness_pct": {value}, "b": {value255}}"#, 0.5), r#"{"brightness_pct": 50, "b": 128}"#);
        assert_eq!(fill_level(r#"{"on": {on}, "bri": {bri}}"#, 1.0), r#"{"on": true, "bri": 254}"#);
        assert_eq!(fill_level(r#"{"on": {on}, "bri": {bri}}"#, 0.0), r#"{"on": false, "bri": 1}"#);
    }

    #[test]
    fn key_steps_count_whole_steps_only() {
        // 25 steps over 0-255: one step every 10.2.
        assert_eq!(key_steps(100.0, 105, 25).0, 0);
        let (n, a) = key_steps(100.0, 111, 25);
        assert_eq!(n, 1);
        assert!((a - 110.2).abs() < 0.01);
        // Jitter back across the step doesn't send the opposite key.
        assert_eq!(key_steps(a, 109, 25).0, 0);
        assert_eq!(key_steps(a, 99, 25).0, -1);
        assert_eq!(key_steps(0.0, 255, 25).0, 25);
    }

    #[test]
    fn mic_users_match() {
        let users = vec!["discord.exe".to_string(), "microsoft.windowssoundrecorder_8wekyb3d8bbwe".to_string()];
        assert!(mic_matches(&users, ""), "no app = any app");
        assert!(mic_matches(&users, "discord.exe"));
        assert!(mic_matches(&users, "windowssoundrecorder.exe"), "Store apps match by name");
        assert!(!mic_matches(&users, "zoom.exe"));
        assert!(!mic_matches(&[], ""));
    }

    #[test]
    fn norm_names() {
        assert_eq!(norm(" Spotify "), "spotify.exe");
        assert_eq!(norm("Discord.exe"), "discord.exe");
        assert_eq!(norm("focused"), "focused");
    }

    #[test]
    fn alert_lights_one_knob_over_rainbow() {
        let mut cfg = Config::default();
        cfg.normalize();
        let mut p = cfg.profile().clone();
        p.lighting.mode = "rainbow".into();
        p.lighting.brightness = 100;
        let a = Alert { app: "discord.exe".into(), lights: vec!["k2".into()], effect: "solid".into(), color: "#a64dff".into(), ..Alert::default() };
        let f = alert_profile(&p, &[a], &[true], 0.0);
        let r = hid::lighting(hid::Model::Pro, &f, &[false; CONTROLS]);
        assert_eq!(r.len(), 4, "custom lighting = knobs, labels, sliders, logo");
        assert_eq!(&r[0][..2], &[5, 2]);
        assert_eq!(&r[0][9..13], &[1, 0xa6, 0x4d, 0xff], "K2 static purple");
        assert_eq!(r[0][2], 1, "K1 static (imitated rainbow)");
        assert_eq!(&r[3][..3], &[5, 3, 2], "logo keeps firmware rainbow");
    }

    #[test]
    fn live_lights_and_self_test() {
        let l = Light { mode: "meter".into(), color: "#ff0000".into(), color2: "#00ff00".into(), mute_color: "#123456".into() };
        let quiet = live_light(&l, 0.0, true);
        assert_eq!((quiet.mode.as_str(), quiet.color.as_str(), quiet.color2.as_str()), ("gradient", "#000000", "#000000"));
        let half = live_light(&l, 0.5, true);
        assert_eq!((half.color.as_str(), half.color2.as_str()), ("#ff0000", "#000000"), "bottom full, top off at 50%");
        assert_eq!(live_light(&l, 1.0, true).color2, "#00ff00");
        assert_eq!(live_light(&l, 0.3, true).mute_color, "#123456", "mute color still wins");
        let lvl = Light { mode: "level".into(), ..l.clone() };
        assert_eq!(live_light(&lvl, 1.0, false).color, "#00ff00");
        let mut cfg = Config::default();
        cfg.normalize();
        let t = test_profile(cfg.profile(), 6);
        let r = hid::lighting(hid::Model::Pro, &t, &[false; CONTROLS]);
        assert!(r[0][2..].iter().all(|&b| b == 0), "knobs off");
        assert_eq!(&r[2][2 + 7..2 + 7 + 4], &[1, 0xff, 0xff, 0xff], "slider 2 white");
        assert_eq!(r[3][2], 0, "logo off");
    }

    #[test]
    fn debounce_turns_chatter_into_one_press() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let w = Duration::from_millis(50);
        let mut d = Debounce::default();
        // Press with chatter: down, up, down within 5 ms -> exactly one "down".
        assert_eq!(d.raw(true, ms(0), w), Some(true));
        assert_eq!(d.raw(false, ms(2), w), None);
        assert_eq!(d.raw(true, ms(4), w), None);
        assert_eq!(d.settle(ms(60), w), None, "settled down: nothing new");
        // Release with chatter -> exactly one "up".
        assert_eq!(d.raw(false, ms(150), w), Some(false));
        assert_eq!(d.raw(true, ms(152), w), None);
        assert_eq!(d.raw(false, ms(154), w), None);
        assert_eq!(d.settle(ms(210), w), None);
        // A very quick tap (released inside the window) still gets its release, late.
        assert_eq!(d.raw(true, ms(300), w), Some(true));
        assert_eq!(d.raw(false, ms(320), w), None);
        assert!(d.pending(w).is_some());
        assert_eq!(d.settle(ms(351), w), Some(false));
        // A real double press (two presses 120 ms apart) is still two presses.
        assert_eq!(d.raw(true, ms(500), w), Some(true));
        assert_eq!(d.raw(false, ms(560), w), Some(false));
        assert_eq!(d.raw(true, ms(620), w), Some(true));
    }

    #[test]
    fn pickup_waits_until_the_control_reaches_the_volume() {
        let mut side = None;
        assert!(!picks_up(0.20, 0.60, &mut side)); // below the volume: ignored
        assert!(!picks_up(0.40, 0.60, &mut side));
        assert!(picks_up(0.58, 0.60, &mut side)); // within the window
        let mut side = None;
        assert!(!picks_up(0.20, 0.60, &mut side));
        assert!(picks_up(0.90, 0.60, &mut side)); // swept past it in one jump
        let mut side = None;
        assert!(!picks_up(0.90, 0.60, &mut side)); // above, then still above
        assert!(!picks_up(0.80, 0.60, &mut side));
    }
}
