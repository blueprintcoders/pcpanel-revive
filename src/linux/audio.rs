//! Linux audio through `pactl` (PulseAudio, or PipeWire's PulseAudio server): device volume/mute,
//! per-app stream volume/mute, the default device and moving apps to another device. The same
//! approach nvdweem/PCPanel uses on Linux. Needs pactl 16 or newer (for its JSON output).
//!
//! Apps are named after their program with ".exe" added ("firefox.exe"), so the app names in a profile
//! mean the same thing on Windows and Linux.
use serde_json::Value;
use std::process::Command;

#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub fn message(&self) -> String {
        self.0.clone()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type R<T> = Result<T, Error>;

pub fn com_init() {}

fn pactl(args: &[&str]) -> R<String> {
    let out = Command::new("pactl").args(args).output().map_err(|e| Error(format!("pactl: {e} (install pulseaudio-utils)")))?;
    if !out.status.success() {
        return Err(Error(format!("pactl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn list(what: &str) -> Vec<Value> {
    pactl(&["-f", "json", "list", what]).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// Average of the channel volumes, 0-1 (pactl reports each channel out of 65536).
fn level(v: &Value) -> f32 {
    let ch: Vec<f64> = v["volume"].as_object().into_iter().flatten().filter_map(|(_, c)| c["value"].as_f64()).collect();
    if ch.is_empty() { 0.0 } else { (ch.iter().sum::<f64>() / ch.len() as f64 / 65536.0) as f32 }
}

fn raw(v: f32) -> String {
    ((v.clamp(0.0, 1.0) * 65536.0).round() as u32).to_string()
}

/// A live peak meter. Not on Linux yet: nothing reads one.
#[derive(Clone)]
pub struct Meter;

pub fn peak(_: &Meter) -> f32 {
    0.0
}

pub struct Device {
    /// The PulseAudio name, like alsa_output.pci-0000_00_1f.3.analog-stereo.
    pub id: String,
    pub name: String,
    pub capture: bool,
    volume: f32,
    muted: bool,
}

/// One app's audio stream.
pub struct Session {
    pub exe: String,
    pub pid: u32,
    pub meter: Option<Meter>,
    index: u64,
    volume: f32,
    muted: bool,
}

impl Session {
    pub fn set_volume(&self, v: f32) {
        let _ = pactl(&["set-sink-input-volume", &self.index.to_string(), &raw(v)]);
    }
    pub fn volume(&self) -> f32 {
        self.volume
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
    pub fn set_mute(&self, m: bool) {
        let _ = pactl(&["set-sink-input-mute", &self.index.to_string(), if m { "1" } else { "0" }]);
    }
}

pub struct Audio;

impl Audio {
    pub fn new() -> R<Self> {
        pactl(&["info"]).map(|_| Audio)
    }

    /// Outputs, then inputs. The "monitor" inputs that mirror each output are left out.
    pub fn devices(&self) -> Vec<Device> {
        let mut out = vec![];
        for (what, capture) in [("sinks", false), ("sources", true)] {
            for d in list(what) {
                if d["properties"]["device.class"] == "monitor" {
                    continue;
                }
                out.push(Device {
                    id: d["name"].as_str().unwrap_or_default().to_string(),
                    name: d["description"].as_str().unwrap_or_default().to_string(),
                    capture,
                    volume: level(&d),
                    muted: d["mute"].as_bool().unwrap_or(false),
                });
            }
        }
        out
    }

    fn default_name(&self, capture: bool) -> Option<String> {
        let info: Value = serde_json::from_str(&pactl(&["-f", "json", "info"]).ok()?).ok()?;
        info[if capture { "default_source_name" } else { "default_sink_name" }].as_str().map(String::from)
    }

    /// A device by "default" / "default_capture" (Linux has no separate communications device), id or part of its name.
    pub fn find(&self, spec: &str) -> Option<Device> {
        let spec = spec.trim().to_lowercase();
        let all = self.devices();
        let default = match spec.as_str() {
            "" | "default" | "default_comm" => Some(false),
            "default_capture" | "default_comm_capture" => Some(true),
            _ => None,
        };
        let pos = match default {
            Some(capture) => {
                let name = self.default_name(capture)?;
                all.iter().position(|d| d.id == name)
            }
            None => all.iter().position(|d| d.id.to_lowercase() == spec)
                .or_else(|| all.iter().position(|d| d.name.to_lowercase().contains(&spec))),
        }?;
        all.into_iter().nth(pos)
    }

    fn kind(d: &Device) -> &'static str {
        if d.capture { "source" } else { "sink" }
    }

    pub fn set_device_volume(&self, spec: &str, v: f32) -> R<()> {
        let d = self.find(spec).ok_or_else(not_found)?;
        pactl(&[&format!("set-{}-volume", Self::kind(&d)), &d.id, &raw(v)]).map(drop)
    }

    pub fn device_meter(&self, _spec: &str) -> Option<Meter> {
        None
    }

    pub fn device_volume(&self, spec: &str) -> Option<f32> {
        self.find(spec).map(|d| d.volume)
    }

    pub fn device_muted(&self, spec: &str) -> Option<bool> {
        self.find(spec).map(|d| d.muted)
    }

    pub fn toggle_device_mute(&self, spec: &str) -> R<()> {
        let d = self.find(spec).ok_or_else(not_found)?;
        pactl(&[&format!("set-{}-mute", Self::kind(&d)), &d.id, "toggle"]).map(drop)
    }

    pub fn set_default(&self, spec: &str) -> R<()> {
        let d = self.find(spec).ok_or_else(not_found)?;
        pactl(&[&format!("set-default-{}", Self::kind(&d)), &d.id]).map(drop)
    }

    pub fn default_id(&self, capture: bool) -> Option<String> {
        self.default_name(capture)
    }

    /// Every app stream that's playing (or paused) on any output.
    pub fn sessions(&self) -> Vec<Session> {
        let streams = list("sink-inputs");
        // Apps that use PipeWire directly only give their process id on their client.
        let clients = if streams.iter().all(|s| s["properties"]["application.process.id"].is_string()) { vec![] } else { list("clients") };
        streams.iter().map(|s| {
            let p = &s["properties"];
            let pid = p["application.process.id"].as_str()
                .or_else(|| clients.iter().find(|c| c["index"].to_string() == s["client"].to_string().trim_matches('"'))?["properties"]["application.process.id"].as_str())
                .and_then(|v| v.parse().ok()).unwrap_or(0);
            // Flatpak apps may not say which program they are; their app id or name has to do.
            let exe = match p["application.process.binary"].as_str() {
                Some(prog) => exe_name(prog),
                None if pid > 0 => process_name(pid),
                None => exe_name(p["pipewire.access.portal.app_id"].as_str().or(p["application.name"].as_str()).unwrap_or_default()),
            };
            Session {
                exe,
                pid,
                meter: None,
                index: s["index"].as_u64().unwrap_or(0),
                volume: level(s),
                muted: s["mute"].as_bool().unwrap_or(false),
            }
        }).collect()
    }

    pub fn loopback(&self) -> R<Loopback> {
        Err(Error("the music visualizer isn't on Linux yet".into()))
    }
}

/// Apps recording from an input right now (not the level meters of mixer apps).
pub fn recording_apps() -> Vec<String> {
    list("source-outputs").iter()
        .filter(|s| s["properties"]["stream.monitor"] != "true" && s["corked"] != true)
        .filter_map(|s| s["properties"]["application.process.binary"].as_str().map(exe_name))
        .collect()
}

fn not_found() -> Error {
    Error("audio device not found".into())
}

/// Not on Linux yet: `Audio::loopback` never makes one.
pub struct Loopback {
    pub rate: f32,
    pub device: String,
}

impl Loopback {
    pub fn read(&self, _out: &mut Vec<f32>) -> R<()> {
        Err(not_found())
    }
}

/// Move these apps' streams to a device (None: the default output).
pub fn set_app_output(pids: &[u32], device_id: Option<&str>) -> R<()> {
    let target = match device_id {
        Some(id) => id.to_string(),
        None => Audio.default_name(false).ok_or_else(not_found)?,
    };
    for s in list("sink-inputs") {
        let pid: u32 = s["properties"]["application.process.id"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
        if pids.contains(&pid) {
            pactl(&["move-sink-input", &s["index"].as_u64().unwrap_or(0).to_string(), &target])?;
        }
    }
    Ok(())
}

/// "Firefox" or "/usr/lib/firefox/firefox" -> "firefox.exe".
fn exe_name(prog: &str) -> String {
    let base = prog.rsplit('/').next().unwrap_or("").trim().to_lowercase();
    if base.is_empty() || base.ends_with(".exe") { base } else { base + ".exe" }
}

/// Lowercase program name of a process with ".exe" added, "" if unknown.
pub fn process_name(pid: u32) -> String {
    let path = process_path(pid);
    if !path.is_empty() {
        return exe_name(&path);
    }
    std::fs::read_to_string(format!("/proc/{pid}/comm")).map(|c| exe_name(&c)).unwrap_or_default()
}

/// Full path of a process's program, "" if unknown.
pub fn process_path(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    std::fs::read_link(format!("/proc/{pid}/exe")).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The program of the window in front: Hyprland, KDE (kdotool) or X11 (xdotool). GNOME on Wayland
/// doesn't tell other apps, so there it's "".
pub fn foreground_exe() -> String {
    crate::sys::focused_pid().map(process_name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_levels() {
        assert_eq!(exe_name("/usr/lib/firefox/firefox"), "firefox.exe");
        assert_eq!(exe_name("Discord"), "discord.exe");
        assert_eq!(exe_name(""), "");
        let v: Value = serde_json::from_str(r#"{"volume":{"front-left":{"value":65536},"front-right":{"value":32768}}}"#).unwrap();
        assert!((level(&v) - 0.75).abs() < 1e-6);
        assert_eq!(raw(0.5), "32768");
    }

    /// Against the real audio server: play a tone with `pw-play` or `paplay` while it runs.
    #[test]
    #[ignore]
    fn pactl_live() {
        let a = Audio::new().unwrap_or_else(|e| panic!("{}", e.message()));
        for d in a.devices() {
            println!("device {} '{}' capture={} volume={:.2} muted={}", d.id, d.name, d.capture, d.volume, d.muted);
        }
        println!("default output: {:?}", a.find("default").map(|d| d.id));
        let s = a.sessions();
        for s in &s {
            println!("app {} pid={} volume={:.2} muted={}", s.exe, s.pid, s.volume, s.muted);
        }
        let first = s.first().expect("play something first");
        first.set_volume(0.3);
        first.set_mute(true);
        let again = a.sessions().into_iter().find(|x| x.index == first.index).unwrap();
        assert!((again.volume() - 0.3).abs() < 0.01 && again.muted());
        again.set_mute(false);
        again.set_volume(1.0);
        let out = a.find("default").unwrap();
        a.set_device_volume("default", 0.4).unwrap_or_else(|e| panic!("{}", e.message()));
        assert!((a.device_volume("default").unwrap() - 0.4).abs() < 0.01);
        a.set_device_volume("default", out.volume).ok();
    }
}
