//! Music visualizer: splits what the speakers are playing into four bands, finds the beat,
//! and paints the panel with it.
use crate::config::{Light, Logo, Profile, CONTROLS, KNOBS};
use crate::engine::{hsv_hex, live_light, mix_hex, scale_hex};

/// Band edges: bass < 150 Hz < low mids < 600 Hz < high mids < 3 kHz < treble.
const EDGES: [f32; 3] = [150.0, 600.0, 3000.0];
/// Each band shows how far it is from its own recent average: this many dB below reads as dark...
const BELOW_DB: f32 = 3.0;
/// ...and this many above as full. Music is loud all the time; its movement is in these swings.
const ABOVE_DB: f32 = 4.5;
/// A band this far below the whole mix stays dark, so a pure bass note doesn't light the treble.
const GATE_DB: f32 = 30.0;

#[derive(Clone, Copy, Default, Debug)]
pub struct Frame {
    /// Bass, low mids, high mids, treble (0..1).
    pub bands: [f32; 4],
    /// Everything together (0..1).
    pub level: f32,
    /// Flash on the beat: 1 right on it, fading to 0.
    pub glow: f32,
}

pub struct Analyzer {
    coef: [f32; 3],
    lp: [f32; 3],
    /// Running average loudness per band (and overall), in dB; None until there's sound.
    avg_db: [Option<f32>; 5],
    frame: Frame,
    bass_avg: f32,
    since_beat: u32,
}

impl Analyzer {
    pub fn new(rate: f32) -> Self {
        let coef = EDGES.map(|f| 1.0 - (-std::f32::consts::TAU * f / rate.max(8000.0)).exp());
        Analyzer { coef, lp: [0.0; 3], avg_db: [None; 5], frame: Frame::default(), bass_avg: 0.0, since_beat: 0 }
    }

    /// Mono samples since the last frame -> the new frame.
    pub fn feed(&mut self, samples: &[f32]) -> Frame {
        let mut energy = [0.0f32; 5];
        for &x in samples {
            // One-pole low-passes; the differences between them are the bands.
            for k in 0..3 {
                self.lp[k] += self.coef[k] * (x - self.lp[k]);
            }
            let b = [self.lp[0], self.lp[1] - self.lp[0], self.lp[2] - self.lp[1], x - self.lp[2], x];
            for k in 0..5 {
                energy[k] += b[k] * b[k];
            }
        }
        let n = samples.len().max(1) as f32;
        let rms = energy.map(|e| (e / n).sqrt());
        let db = rms.map(|r| 20.0 * r.max(1e-6).log10());
        let mut level = [0.0f32; 5];
        for k in 0..5 {
            if rms[k] < 1e-4 {
                continue; // silence
            }
            let avg = self.avg_db[k].get_or_insert(db[k]);
            *avg = *avg * 0.94 + db[k] * 0.06; // about half a second
            let gated = k < 4 && db[k] < db[4] - GATE_DB;
            let v = ((db[k] - *avg + BELOW_DB) / (BELOW_DB + ABOVE_DB)).clamp(0.0, 1.0);
            // An S-curve for contrast: small swings still read as dark or bright.
            level[k] = if gated { 0.0 } else { v * v * (3.0 - 2.0 * v) };
        }
        // Instant rise, quick fall: lively without flicker.
        let ease = |cur: f32, v: f32| if v > cur { v } else { cur * 0.7 + v * 0.3 };
        let f = &mut self.frame;
        for k in 0..4 {
            f.bands[k] = ease(f.bands[k], level[k]);
        }
        f.level = ease(f.level, level[4]);
        // Beat: bass jumps well above its running average.
        self.since_beat += 1;
        if self.bass_avg == 0.0 {
            self.bass_avg = rms[0];
        }
        let beat = rms[0] > 1.5 * self.bass_avg && level[0] > 0.6 && self.since_beat > 6;
        self.bass_avg = self.bass_avg * 0.92 + rms[0] * 0.08;
        if beat {
            self.since_beat = 0;
        }
        f.glow = if beat { 1.0 } else { f.glow * 0.6 };
        *f
    }
}

/// The profile painted with one visualizer frame. `t` (seconds) drifts the rainbow; `knobs` is 4 on the Mini.
pub fn paint(p: &Profile, f: &Frame, t: f32, knobs: usize) -> Profile {
    let mut out = p.clone();
    let l = &p.lighting;
    // "pulse": one color, everything pulsing together with the whole mix.
    let pulse = l.viz == "pulse";
    let rainbow = !pulse && l.viz != "colors";
    let f = &if pulse { Frame { bands: [f.level; 4], ..*f } } else { *f };
    // Band hues run red (bass) to violet (treble), slowly turning.
    let hue = |band: f32| band * 75.0 + t * 12.0;
    let color_of = |band: usize, v: f32| match () {
        _ if pulse => l.viz_high.clone(),
        _ if rainbow => hsv_hex(hue(band as f32), 1.0, 1.0),
        _ => mix_hex(&l.viz_low, &l.viz_high, v),
    };
    let flash = |c: String| mix_hex(&c, "#ffffff", f.glow * 0.6);
    let solid = |c: String| Light { mode: "static".into(), color: c.clone(), color2: c, mute_color: String::new() };
    // Pro knobs: bass, low mids, everything, high mids, treble. Mini: the four bands.
    let knob_bands: &[Option<usize>] = if knobs < KNOBS { &[Some(0), Some(1), Some(2), Some(3)] } else { &[Some(0), Some(1), None, Some(2), Some(3)] };
    for (i, band) in knob_bands.iter().enumerate() {
        let (v, c) = match band {
            Some(b) => (f.bands[*b], color_of(*b, f.bands[*b])),
            None => (f.level, if rainbow { hsv_hex(hue(1.5) + 180.0, 1.0, 1.0) } else { color_of(0, f.level) }),
        };
        out.controls[i].light = solid(flash(scale_hex(&c, 0.06 + 0.94 * v)));
    }
    for k in 0..CONTROLS - KNOBS {
        let v = f.bands[k];
        let (c1, c2) = match () {
            _ if pulse => (l.viz_high.clone(), l.viz_high.clone()),
            _ if rainbow => (color_of(k, v), hsv_hex(hue(k as f32) + 40.0, 1.0, 1.0)),
            _ => (l.viz_low.clone(), l.viz_high.clone()),
        };
        let meter = Light { mode: "meter".into(), color: c1.clone(), color2: c2, mute_color: String::new() };
        out.controls[KNOBS + k].light = live_light(&meter, v, true);
        out.controls[KNOBS + k].label_color = scale_hex(&c1, 0.15 + 0.85 * v);
    }
    let logo = if rainbow { hsv_hex(t * 30.0, 1.0, 1.0) } else { l.viz_high.clone() };
    out.lighting.logo = Logo { mode: "static".into(), color: flash(scale_hex(&logo, 0.1 + 0.9 * f.level)), ..Logo::default() };
    out.lighting.mode = "custom".into();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(48000.0 * secs) as usize).map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / 48000.0).sin()).collect()
    }

    #[test]
    fn bands_follow_the_music() {
        let mut a = Analyzer::new(48000.0);
        let mut f = Frame::default();
        for chunk in tone(60.0, 1.0, 0.1).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!((0.2..0.7).contains(&f.bands[0]), "a steady bass note sits mid-way: {:?}", f.bands);
        assert_eq!(f.bands[3], 0.0, "no treble in a bass note: {:?}", f.bands);
        for chunk in tone(60.0, 0.1, 0.4).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!(f.bands[0] > 0.95, "it gets louder: full: {:?}", f.bands);
        for chunk in tone(60.0, 0.3, 0.03).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!(f.bands[0] < 0.1, "then quieter: dark: {:?}", f.bands);
        let mut a = Analyzer::new(48000.0);
        for chunk in tone(8000.0, 1.0, 0.5).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!(f.bands[3] > 0.2 && f.bands[0] == 0.0, "treble, no bass: {:?}", f.bands);
        // Silence: everything fades out.
        for _ in 0..30 {
            f = a.feed(&[0.0; 1600]);
        }
        assert!(f.bands.iter().all(|&b| b < 0.01) && f.level < 0.01);
    }

    #[test]
    fn a_kick_drum_is_a_beat() {
        let mut a = Analyzer::new(48000.0);
        let quiet = tone(60.0, 0.033, 0.05);
        let kick = tone(60.0, 0.033, 0.8);
        let mut beats = 0;
        for n in 0..90 {
            let f = a.feed(if n % 15 == 0 && n > 0 { &kick } else { &quiet });
            if f.glow == 1.0 {
                beats += 1;
            }
        }
        assert_eq!(beats, 5, "one beat per kick");
    }

    #[test]
    fn pulse_moves_everything_together() {
        let mut cfg = crate::config::Config::default();
        cfg.normalize();
        let mut p = cfg.profile().clone();
        p.lighting.viz = "pulse".into();
        p.lighting.viz_high = "#ff0000".into();
        let out = paint(&p, &Frame { bands: [1.0, 0.0, 0.0, 0.0], level: 1.0, glow: 0.0 }, 0.0, KNOBS);
        assert!(out.controls[..KNOBS].iter().all(|c| c.light.color == "#ff0000"), "every knob full red");
        let quiet = paint(&p, &Frame { bands: [1.0; 4], level: 0.0, glow: 0.0 }, 0.0, KNOBS);
        assert!(quiet.controls[..KNOBS].iter().all(|c| c.light.color == "#0f0000"), "every knob dim with a quiet mix");
    }

    #[test]
    fn paints_every_light() {
        let mut cfg = crate::config::Config::default();
        cfg.normalize();
        let mut p = cfg.profile().clone();
        p.lighting.viz_when = "always".into();
        let f = Frame { bands: [1.0, 0.0, 0.5, 0.2], level: 0.7, glow: 0.0 };
        let out = paint(&p, &f, 0.0, KNOBS);
        assert_eq!(out.lighting.mode, "custom");
        assert_eq!(out.controls[0].light.color, "#ff0000", "bass knob full red");
        assert_eq!(out.controls[1].light.color, "#0b0f00", "quiet low mids nearly dark");
        assert_eq!(out.controls[5].light.mode, "gradient");
        let r = crate::hid::lighting(crate::hid::Model::Pro, &out, &[false; CONTROLS]);
        assert_eq!(r.len(), 4);
    }
}

/// Tuning aid: play some music, then `cargo test live_capture_stats -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn live_capture_stats() {
    crate::audio::com_init();
    let audio = crate::audio::Audio::new().unwrap();
    let lb = audio.loopback().unwrap();
    let mut a = Analyzer::new(lb.rate);
    let (mut buf, mut frames) = (vec![], vec![]);
    for n in 0..300 {
        std::thread::sleep(std::time::Duration::from_millis(33));
        buf.clear();
        lb.read(&mut buf).unwrap();
        let f = a.feed(&buf);
        frames.push(f);
        if (60..90).contains(&n) {
            println!("{:>4} bands {:.2} {:.2} {:.2} {:.2} level {:.2} glow {:.1}", n, f.bands[0], f.bands[1], f.bands[2], f.bands[3], f.level, f.glow);
        }
    }
    for k in 0..4 {
        let v: Vec<f32> = frames.iter().map(|f| f.bands[k]).collect();
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
        let jump = v.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f32>() / v.len() as f32;
        println!("band {k}: mean {mean:.2} sd {sd:.2} avg change per frame {jump:.2}");
    }
}
