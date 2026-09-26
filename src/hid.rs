//! PCPanel USB HID (Pro, Mini, original wooden PCPanel): input reports and lighting reports.
//! Protocol per nvdweem/PCPanel (OutputInterpreter.java, DeviceType.java) and neoyagami/PanelPCLlit.
use crate::config::{Light, Lighting, Profile, CONTROLS, KNOBS};
use crate::{Msg, Shared};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Report = [u8; 64];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Model {
    #[default]
    Pro,
    Mini,
    /// The 2017 wooden original (called "PCPanel RGB" in nvdweem/PCPanel): 4 knobs, knob values 0-100.
    Original,
}

impl Model {
    fn find(vid: u16, pid: u16) -> Option<Model> {
        match (vid, pid) {
            (0x0483, 0xa3c5) => Some(Model::Pro),
            (0x0483, 0xa3c4) => Some(Model::Mini),
            (0x04d8, 0xeb52) => Some(Model::Original),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self { Model::Pro => "pro", Model::Mini => "mini", Model::Original => "original" }
    }
    pub fn name(self) -> &'static str {
        match self { Model::Pro => "PCPanel Pro", Model::Mini => "PCPanel Mini", Model::Original => "PCPanel (original)" }
    }
    /// Knobs + sliders (HID analog indexes 0..n).
    pub fn analogs(self) -> usize {
        match self { Model::Pro => CONTROLS, _ => 4 }
    }
    pub fn buttons(self) -> usize {
        match self { Model::Pro => KNOBS, _ => 4 }
    }
}

pub enum Event {
    Connected(bool),
    Turn { index: usize, value: u8, initial: bool },
    Button { index: usize, down: bool },
}

pub fn parse(model: Model, data: &[u8]) -> Option<Event> {
    let (&kind, &index, &value) = (data.first()?, data.get(1)?, data.get(2)?);
    let index = index as usize;
    match kind {
        1 if index < model.analogs() => {
            // The original reports 0-100; everything else uses 0-255.
            let value = if model == Model::Original { (value.min(100) as u32 * 255 / 100) as u8 } else { value };
            Some(Event::Turn { index, value, initial: false })
        }
        2 if index < model.buttons() => Some(Event::Button { index, down: value == 1 }),
        _ => None,
    }
}

pub fn spawn(tx: Sender<Msg>, shared: Arc<Mutex<Shared>>) {
    std::thread::Builder::new().name("device".into()).spawn(move || loop {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session(&tx, &shared))) {
            Ok(Err(e)) => crate::log(&shared, format!("device: {e}")),
            Err(_) => crate::log(&shared, "device connection crashed and was restarted (details in the log file)".into()),
            Ok(Ok(())) => {}
        }
        let _ = tx.send(Msg::Hid(Event::Connected(false)));
        std::thread::sleep(Duration::from_secs(2));
    }).expect("device thread");
}

/// One connection to the device; returns when it is unplugged or not found.
fn session(tx: &Sender<Msg>, shared: &Arc<Mutex<Shared>>) -> Result<(), String> {
    let api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
    let Some((info, model)) = api.device_list().find_map(|d| Model::find(d.vendor_id(), d.product_id()).map(|m| (d, m))) else {
        return Ok(()); // not plugged in; retry later
    };
    let dev = info.open_device(&api).map_err(|e| e.to_string())?;
    write(&dev, &[1])?; // init
    crate::lock(shared).model = model;
    let _ = tx.send(Msg::Hid(Event::Connected(true)));
    let connected = Instant::now();
    let mut written_gen = 0u64;
    let mut buf = [0u8; 64];
    loop {
        let n = dev.read_timeout(&mut buf, 20).map_err(|e| e.to_string())?;
        if n > 0 {
            if let Some(mut ev) = parse(model, &buf[..n]) {
                // The firmware reports every position right after init.
                if let Event::Turn { initial, .. } = &mut ev {
                    *initial = connected.elapsed() < Duration::from_millis(500);
                }
                let _ = tx.send(Msg::Hid(ev));
            }
        }
        let pending = {
            let s = crate::lock(shared);
            (s.lights_gen != written_gen).then(|| (s.lights_gen, s.lights.clone()))
        };
        if let Some((gen, reports)) = pending {
            for r in &reports {
                write(&dev, r)?;
            }
            written_gen = gen;
            crate::lock(shared).lights_written += 1;
        }
    }
}

fn write(dev: &hidapi::HidDevice, data: &[u8]) -> Result<(), String> {
    let mut out = [0u8; 65]; // report id 0 + 64 bytes
    out[1..1 + data.len()].copy_from_slice(data);
    dev.write(&out).map(|_| ()).map_err(|e| e.to_string())
}

fn rgb(hex: &str, brightness: u8) -> [u8; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(0);
    let b = brightness.min(100) as u32;
    [p(0), p(2), p(4)].map(|c| (c as u32 * b / 100) as u8)
}

fn put(r: &mut Report, at: usize, bytes: &[u8]) {
    r[at..at + bytes.len()].copy_from_slice(bytes);
}

/// Build the lighting reports for a profile. `muted[i]` swaps in the control's mute color.
pub fn lighting(model: Model, p: &Profile, muted: &[bool; CONTROLS]) -> Vec<Report> {
    let l: &Lighting = &p.lighting;
    let b = l.brightness;
    let scaled = |v: u8| (v as u32 * b as u32 / 100) as u8;
    let mut r = [0u8; 64];
    if model == Model::Original {
        // The original speaks a different dialect (code 2, then the sub-command); harmless if it has no LEDs.
        match l.mode.as_str() {
            "color" => { put(&mut r, 0, &[2, 1, 0]); put(&mut r, 3, &rgb(&l.color, b)); }
            "rainbow" => put(&mut r, 0, &[2, 3, l.hue, 0xff, scaled(l.anim_brightness), l.speed, l.reverse as u8]),
            "wave" => put(&mut r, 0, &[2, 4, l.hue, 0xff, scaled(l.anim_brightness), l.speed, l.reverse as u8, l.bounce as u8]),
            "breath" => put(&mut r, 0, &[2, 5, l.hue, 0xff, scaled(l.anim_brightness), l.speed]),
            _ => return vec![rgb_custom(p, muted)],
        }
        return vec![r];
    }
    let prefix = if model == Model::Mini { 6 } else { 5 };
    match l.mode.as_str() {
        "color" => { put(&mut r, 0, &[prefix, 4, if model == Model::Mini { 5 } else { 2 }]); put(&mut r, 3, &rgb(&l.color, b)); }
        "rainbow" => put(&mut r, 0, &[prefix, 4, if l.vertical { 2 } else { 1 }, l.hue, 0xff, scaled(l.anim_brightness), l.speed, l.reverse as u8]),
        "wave" => put(&mut r, 0, &[prefix, 4, 3, l.hue, 0xff, scaled(l.anim_brightness), l.speed, l.reverse as u8, l.bounce as u8]),
        "breath" => put(&mut r, 0, &[prefix, 4, 4, l.hue, 0xff, scaled(l.anim_brightness), l.speed]),
        _ if model == Model::Mini => return vec![knob_report(6, 4, p, muted)],
        _ => return custom(p, muted),
    }
    vec![r]
}

fn light(p: &Profile, muted: &[bool; CONTROLS], i: usize) -> Light {
    let l = &p.controls[i].light;
    if muted[i] && !l.mute_color.is_empty() {
        Light { mode: "static".into(), color: l.mute_color.clone(), ..l.clone() }
    } else {
        l.clone()
    }
}

/// Knob rings: 7 bytes per knob (mode, color, color2).
fn knob_report(prefix: u8, knobs: usize, p: &Profile, muted: &[bool; CONTROLS]) -> Report {
    let b = p.lighting.brightness;
    let mut r = [0u8; 64];
    put(&mut r, 0, &[prefix, 2]);
    for i in 0..knobs {
        let l = light(p, muted, i);
        let at = 2 + i * 7;
        match l.mode.as_str() {
            "static" => { r[at] = 1; put(&mut r, at + 1, &rgb(&l.color, b)); }
            "gradient" | "volume" => {
                r[at] = 2;
                put(&mut r, at + 1, &rgb(&l.color, b));
                put(&mut r, at + 4, &rgb(&l.color2, b));
            }
            _ => {}
        }
    }
    r
}

/// Original per-knob: one color each, optionally with brightness following the knob.
fn rgb_custom(p: &Profile, muted: &[bool; CONTROLS]) -> Report {
    let b = p.lighting.brightness;
    let mut r = [0u8; 64];
    put(&mut r, 0, &[2, 0]);
    for i in 0..4 {
        let l = light(p, muted, i);
        let (color, track) = match l.mode.as_str() {
            "static" => (rgb(&l.color, b), 0),
            "gradient" | "volume" => (rgb(&l.color2, b), 1),
            _ => ([0; 3], 0),
        };
        put(&mut r, 2 + i * 4, &[1, color[0], color[1], color[2]]);
        r[18 + i] = track;
    }
    r
}

fn custom(p: &Profile, muted: &[bool; CONTROLS]) -> Vec<Report> {
    let b = p.lighting.brightness;
    let knobs = knob_report(5, KNOBS, p, muted);
    let mut labels = [0u8; 64];
    put(&mut labels, 0, &[5, 1]);
    let mut sliders = [0u8; 64];
    put(&mut sliders, 0, &[5, 0]);
    for s in 0..CONTROLS - KNOBS {
        let c = &p.controls[KNOBS + s];
        let at = 2 + s * 7;
        if !c.label_color.is_empty() {
            labels[at] = 1;
            put(&mut labels, at + 1, &rgb(&c.label_color, b));
        }
        let l = light(p, muted, KNOBS + s);
        let (mode, c2) = match l.mode.as_str() {
            "static" => (1, &l.color),
            "gradient" => (1, &l.color2),
            "volume" => (3, &l.color2),
            _ => continue,
        };
        sliders[at] = mode;
        put(&mut sliders, at + 1, &rgb(&l.color, b));
        put(&mut sliders, at + 4, &rgb(c2, b));
    }
    let lg = &p.lighting.logo;
    let mut logo = [0u8; 64];
    put(&mut logo, 0, &[5, 3]);
    match lg.mode.as_str() {
        "static" => { logo[2] = 1; put(&mut logo, 3, &rgb(&lg.color, b)); }
        "rainbow" => put(&mut logo, 2, &[2, 0xff, (lg.brightness as u32 * b as u32 / 100) as u8, lg.speed]),
        "breath" => put(&mut logo, 2, &[3, lg.hue, 0xff, (lg.brightness as u32 * b as u32 / 100) as u8, lg.speed]),
        _ => {}
    }
    vec![knobs, labels, sliders, logo]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn parses_input() {
        assert!(matches!(parse(Model::Pro, &[1, 7, 200]), Some(Event::Turn { index: 7, value: 200, .. })));
        assert!(matches!(parse(Model::Pro, &[2, 4, 1]), Some(Event::Button { index: 4, down: true })));
        assert!(parse(Model::Pro, &[2, 5, 1]).is_none());
        assert!(parse(Model::Pro, &[9, 0, 0]).is_none());
        assert!(parse(Model::Mini, &[1, 4, 10]).is_none(), "Mini has 4 knobs");
        assert!(matches!(parse(Model::Original, &[1, 3, 100]), Some(Event::Turn { index: 3, value: 255, .. })));
        assert!(matches!(parse(Model::Original, &[1, 0, 50]), Some(Event::Turn { value: 127, .. })));
    }

    #[test]
    fn builds_custom_lighting() {
        let mut cfg = Config::default();
        cfg.normalize();
        let mut p = cfg.profile().clone();
        p.lighting.brightness = 100;
        p.controls[0].light.color = "#102030".into();
        p.controls[5].light = Light { mode: "volume".into(), color: "#ff0000".into(), color2: "#00ff00".into(), mute_color: String::new() };
        let mut muted = [false; CONTROLS];
        muted[1] = true;
        let r = lighting(Model::Pro, &p, &muted);
        assert_eq!(r.len(), 4);
        assert_eq!(&r[0][..6], &[5, 2, 1, 0x10, 0x20, 0x30]);
        assert_eq!(&r[0][9..13], &[1, 0xff, 0, 0]); // knob 2 muted -> red
        assert_eq!(&r[2][..9], &[5, 0, 3, 0xff, 0, 0, 0, 0xff, 0]);
        p.lighting.mode = "wave".into();
        assert_eq!(lighting(Model::Pro, &p, &muted)[0][..3], [5, 4, 3]);
    }

    #[test]
    fn builds_mini_and_original_lighting() {
        let mut cfg = Config::default();
        cfg.normalize();
        let mut p = cfg.profile().clone();
        p.lighting.brightness = 100;
        p.controls[0].light.color = "#102030".into();
        let muted = [false; CONTROLS];
        let mini = lighting(Model::Mini, &p, &muted);
        assert_eq!(mini.len(), 1);
        assert_eq!(&mini[0][..6], &[6, 2, 1, 0x10, 0x20, 0x30]);
        assert_eq!(mini[0][2 + 4 * 7], 0, "only 4 knobs");
        let rgbr = lighting(Model::Original, &p, &muted);
        assert_eq!(&rgbr[0][..6], &[2, 0, 1, 0x10, 0x20, 0x30]);
        p.lighting.mode = "color".into();
        p.lighting.color = "#ff0000".into();
        assert_eq!(&lighting(Model::Mini, &p, &muted)[0][..6], &[6, 4, 5, 0xff, 0, 0]);
        assert_eq!(&lighting(Model::Original, &p, &muted)[0][..6], &[2, 1, 0, 0xff, 0, 0]);
    }
}
