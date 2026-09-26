//! Import profiles from the stock PCPanel software (%LOCALAPPDATA%\PCPanel Software\save.json).
//! Command formats per the stock app's hid/InputInterpreter and nvdweem/PCPanel CommandConverter.
use crate::config::{Action, Control, Light, Lighting, Logo, Profile, Turn, CONTROLS, KNOBS};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

pub struct Imported {
    pub profiles: BTreeMap<String, Profile>,
    pub active: Option<String>,
    pub notes: Vec<String>,
}

pub fn stock_path() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    base.join("PCPanel Software").join("save.json")
}

/// `device_name(id)` turns a stock device id into a readable name when that device exists.
pub fn convert(json: &str, device_name: &dyn Fn(&str) -> Option<String>) -> Result<Imported, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| format!("not a PCPanel save file: {e}"))?;
    let devices = root["devices"].as_object().ok_or("no devices in save file")?;
    // Prefer the Pro (9 analog inputs).
    let dev = devices.values().find(|d| d["dialData"].as_array().is_some_and(|a| a.len() == CONTROLS))
        .or_else(|| devices.values().next())
        .ok_or("no devices in save file")?;
    let mut out = Imported { profiles: BTreeMap::new(), active: dev["currentProfile"].as_str().map(String::from), notes: vec![] };
    let list = dev["profiles"].as_array().cloned().unwrap_or_default();
    let list = if list.is_empty() { vec![dev.clone()] } else { list };
    for p in &list {
        let name = p["name"].as_str().unwrap_or("Imported").to_string();
        let mut notes = vec![];
        let profile = profile(p, device_name, &mut notes);
        out.notes.extend(notes.into_iter().map(|n| format!("{name}: {n}")));
        out.profiles.insert(name, profile);
    }
    Ok(out)
}

fn s(v: &Value, i: usize) -> String {
    v.get(i).and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// Java stores bytes signed (-1 = 255).
fn byte(v: &Value) -> u8 {
    (v.as_i64().unwrap_or(0) & 0xff) as u8
}

fn exe(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_lowercase()
}

fn profile(p: &Value, device_name: &dyn Fn(&str) -> Option<String>, notes: &mut Vec<String>) -> Profile {
    let missing = RefCell::new(Vec::<String>::new());
    let dev = |id: &str| {
        if id.is_empty() { return "default".to_string() }
        device_name(id).unwrap_or_else(|| { missing.borrow_mut().push(id.to_string()); "default".to_string() })
    };
    let mut controls: Vec<Control> = (0..CONTROLS).map(|_| Control::default()).collect();
    let label = |i: usize| if i < KNOBS { format!("knob {}", i + 1) } else { format!("slider {}", i - KNOBS + 1) };

    for (i, d) in p["dialData"].as_array().into_iter().flatten().take(CONTROLS).enumerate() {
        let c = &mut controls[i];
        c.turn = match s(d, 0).as_str() {
            "app_volume" => {
                let apps: Vec<String> = [s(d, 1), s(d, 2)].iter().filter(|a| !a.is_empty()).map(|a| exe(a)).collect();
                if apps.is_empty() { Turn::None } else { Turn::App { apps } }
            }
            "focus_volume" => Turn::App { apps: vec!["focused".into()] },
            "device_volume" => Turn::Device { device: dev(&s(d, 1)) },
            "obs_dial" => Turn::Obs { source: s(d, 2) },
            "voicemeeter_dial" if s(d, 1) == "advanced" => Turn::Voicemeeter { param: s(d, 2), min_db: -60.0, max_db: 12.0 },
            "" => Turn::None,
            other => { notes.push(format!("{}: '{other}' not imported", label(i))); Turn::None }
        };
        if let Some(k) = p["knobSettings"].get(i) {
            c.min = k["minTrim"].as_f64().unwrap_or(0.0) as f32;
            c.max = k["maxTrim"].as_f64().unwrap_or(100.0) as f32;
            if k["logarithmic"] == true {
                c.curve = "log".into();
            }
        }
    }
    for (key, double) in [("buttonData", false), ("dblButtonData", true)] {
        for (i, d) in p[key].as_array().into_iter().flatten().take(KNOBS).enumerate() {
            let Some(a) = action(d, &dev, notes) else {
                if !s(d, 0).is_empty() && s(d, 0) != "shortcut" { notes.push(format!("{} button: '{}' not imported", label(i), s(d, 0))); }
                continue;
            };
            if double { controls[i].double.push(a) } else { controls[i].press.push(a) }
        }
    }
    for id in missing.into_inner() {
        notes.push(format!("audio device {id} isn't connected to this PC; using the default device instead"));
    }
    Profile { auto_apps: vec![], lighting: lighting(&p["lightingConfig"], &mut controls, notes), controls }
}

/// Paths saved on another PC often only differ by the user folder; retarget them to this user.
fn fix_path(path: &str) -> Option<String> {
    if Path::new(path).exists() { return Some(path.to_string()) }
    let lower = path.to_lowercase();
    let rest = lower.strip_prefix("c:\\users\\")?.split_once('\\')?.1.len();
    let home = std::env::var("USERPROFILE").ok()?;
    let moved = format!("{home}{}", path.get(path.len().checked_sub(rest + 1)?..)?);
    Path::new(&moved).exists().then_some(moved)
}

fn action(d: &Value, dev: &dyn Fn(&str) -> String, notes: &mut Vec<String>) -> Option<Action> {
    if s(d, 0) == "shortcut" && !s(d, 1).is_empty() {
        let path = s(d, 1);
        if exe(&path) == "pcpanel.exe" {
            notes.push("dropped a button that launched the stock PCPanel app".into());
            return None;
        }
        let Some(found) = fix_path(&path) else {
            notes.push(format!("kept a shortcut to {path}, which doesn't exist on this PC"));
            return Some(Action::Run { cmd: format!("start \"\" \"{path}\"") });
        };
        return Some(Action::Run { cmd: format!("start \"\" \"{found}\"") });
    }
    Some(match s(d, 0).as_str() {
        "keystroke" if !s(d, 1).is_empty() => Action::Keys { keys: s(d, 1).to_lowercase().replace(' ', "") },
        "media" => Action::Keys { keys: match s(d, 1).as_str() {
            "media1" => "play_pause", "media2" => "stop", "media3" => "prev", "media4" => "next", "media5" => "mute", _ => return None,
        }.into() },
        "end_program" => Action::Kill { process: if s(d, 1) == "specific" { exe(&s(d, 2)) } else { "focused".into() } },
        "sound_device" => Action::SetDefault { device: dev(&s(d, 1)) },
        "toggle_device" => Action::CycleDefault { devices: s(d, 1).split('|').filter(|x| !x.is_empty()).map(dev).collect() },
        "mute_app" if !s(d, 1).is_empty() => Action::MuteApp { apps: vec![exe(&s(d, 1))] },
        "mute_device" => Action::MuteDevice { device: dev(&s(d, 1)) },
        "obs_button" if s(d, 1) == "set_scene" => Action::ObsScene { scene: s(d, 2) },
        "obs_button" if s(d, 1) == "mute_source" => Action::ObsMute { source: s(d, 2) },
        "profile" if !s(d, 1).is_empty() => Action::Profile { name: s(d, 1) },
        _ => return None,
    })
}

fn lighting(l: &Value, controls: &mut [Control], notes: &mut Vec<String>) -> Lighting {
    let mut out = Lighting { brightness: 100, ..Lighting::default() };
    let color = |v: &Value| v.as_str().filter(|c| c.starts_with('#') && c.len() == 7).unwrap_or("#000000").to_lowercase();
    match l["lightingMode"].as_str().unwrap_or("") {
        "ALL_COLOR" => { out.mode = "color".into(); out.color = color(&l["allColor"]); }
        "ALL_RAINBOW" => {
            out.mode = "rainbow".into();
            (out.hue, out.anim_brightness, out.speed) = (byte(&l["rainbowPhaseShift"]), byte(&l["rainbowBrightness"]), byte(&l["rainbowSpeed"]));
            (out.reverse, out.vertical) = (byte(&l["rainbowReverse"]) != 0, byte(&l["rainbowVertical"]) != 0);
        }
        "ALL_WAVE" => {
            out.mode = "wave".into();
            (out.hue, out.anim_brightness, out.speed) = (byte(&l["waveHue"]), byte(&l["waveBrightness"]), byte(&l["waveSpeed"]));
            (out.reverse, out.bounce) = (byte(&l["waveReverse"]) != 0, byte(&l["waveBounce"]) != 0);
        }
        "ALL_BREATH" => {
            out.mode = "breath".into();
            (out.hue, out.anim_brightness, out.speed) = (byte(&l["breathHue"]), byte(&l["breathBrightness"]), byte(&l["breathSpeed"]));
        }
        "CUSTOM" => {
            out.mode = "custom".into();
            let light = |c: &Value, slider: bool| Light {
                mode: match c["mode"].as_str().unwrap_or("NONE") {
                    "STATIC" => "static", "STATIC_GRADIENT" => "gradient", "VOLUME_GRADIENT" if slider => "volume", "VOLUME_GRADIENT" => "gradient", _ => "none",
                }.into(),
                color: color(&c["color1"]),
                color2: color(&c["color2"]),
                mute_color: String::new(),
            };
            for (i, c) in l["knobConfigs"].as_array().into_iter().flatten().take(KNOBS).enumerate() {
                controls[i].light = light(c, false);
            }
            for (i, c) in l["sliderConfigs"].as_array().into_iter().flatten().take(CONTROLS - KNOBS).enumerate() {
                controls[KNOBS + i].light = light(c, true);
            }
            for (i, c) in l["sliderLabelConfigs"].as_array().into_iter().flatten().take(CONTROLS - KNOBS).enumerate() {
                controls[KNOBS + i].label_color = if c["mode"] == "STATIC" { color(&c["color"]) } else { String::new() };
            }
            let g = &l["logoConfig"];
            out.logo = Logo {
                mode: match g["mode"].as_str().unwrap_or("NONE") { "STATIC" => "static", "RAINBOW" => "rainbow", "BREATH" => "breath", _ => "none" }.into(),
                color: color(&g["color"]), hue: byte(&g["hue"]), speed: byte(&g["speed"]), brightness: byte(&g["brightness"]),
            };
        }
        "" => {}
        other => notes.push(format!("lighting mode '{other}' not imported")),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"devices":{"X":{"currentProfile":"p1","dialData":[[],[],[],[],[],[],[],[],[]],"profiles":[{"name":"p1",
      "buttonData":[["shortcut","C:\\Apps\\Spotify.exe"],["media","media1"],["media","media4"],["end_program","focused"],["weird"]],
      "dialData":[["app_volume","","","",null],["app_volume","firefox.exe","","",null],["app_volume","C:\\x\\Spotify.exe","","*"],
        ["device_volume","{0.0.0.00000000}.{abc}"],["focus_volume",null],["app_volume","Discord.exe","",""],null,null,null],
      "knobSettings":[{"minTrim":10,"maxTrim":90,"logarithmic":true}],
      "lightingConfig":{"lightingMode":"ALL_RAINBOW","rainbowPhaseShift":101,"rainbowBrightness":-1,"rainbowSpeed":-96,"rainbowReverse":0,"rainbowVertical":1}}]}}}"#;

    #[test]
    fn converts_stock_save() {
        let named = |id: &str| (id == "{0.0.0.00000000}.{abc}").then(|| "Headphones".to_string());
        let r = convert(SAMPLE, &named).unwrap();
        assert_eq!(r.active.as_deref(), Some("p1"));
        let p = &r.profiles["p1"];
        assert_eq!(p.controls[0].turn, Turn::None);
        assert_eq!(p.controls[0].min, 10.0);
        assert_eq!(p.controls[2].turn, Turn::App { apps: vec!["spotify.exe".into()] });
        assert_eq!(p.controls[3].turn, Turn::Device { device: "Headphones".into() });
        assert_eq!(p.controls[4].turn, Turn::App { apps: vec!["focused".into()] });
        assert_eq!(p.controls[0].press, vec![Action::Run { cmd: "start \"\" \"C:\\Apps\\Spotify.exe\"".into() }]);
        assert_eq!(p.controls[1].press, vec![Action::Keys { keys: "play_pause".into() }]);
        assert_eq!(p.controls[2].press, vec![Action::Keys { keys: "next".into() }]);
        assert_eq!(p.controls[3].press, vec![Action::Kill { process: "focused".into() }]);
        assert!(p.controls[4].press.is_empty());
        assert_eq!((p.lighting.mode.as_str(), p.lighting.hue, p.lighting.anim_brightness, p.lighting.speed, p.lighting.vertical), ("rainbow", 101, 255, 160, true));
        assert_eq!(p.controls[0].curve, "log");
        assert_eq!(r.notes.len(), 2); // missing shortcut, 'weird'
        assert!(r.notes.iter().any(|n| n.contains("doesn't exist")));
    }
}
