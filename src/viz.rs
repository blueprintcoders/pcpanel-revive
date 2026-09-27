//! Music visualizer: splits what the speakers are playing into four bands, finds the beat,
//! and paints the panel with it.
use crate::config::{Light, Logo, Profile, CONTROLS, KNOBS};
use crate::engine::{hsv_hex, live_light, mix_hex, scale_hex};

/// Band edges: bass < 150 Hz < low mids < 600 Hz < high mids < 3 kHz < treble.
const EDGES: [f32; 3] = [150.0, 600.0, 3000.0];
/// How far below its recent peak a band reads as dark.
const RANGE_DB: f32 = 24.0;
/// A band's reference level is at least this share of the overall level.
const BAND_FLOOR: f32 = 0.15;

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
    /// Automatic gain: a slowly falling peak per band (and overall), so quiet and loud music both fill the range.
    peak: [f32; 5],
    frame: Frame,
    bass_avg: f32,
    since_beat: u32,
}

impl Analyzer {
    pub fn new(rate: f32) -> Self {
        let coef = EDGES.map(|f| 1.0 - (-std::f32::consts::TAU * f / rate.max(8000.0)).exp());
        Analyzer { coef, lp: [0.0; 3], peak: [1e-3; 5], frame: Frame::default(), bass_avg: 0.0, since_beat: 0 }
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
        let mut level = [0.0f32; 5];
        for k in (0..5).rev() {
            // A band never counts as loud when it's a small part of the whole (else a pure bass note lights the treble too).
            let floor = if k == 4 { 1e-3 } else { self.peak[4] * BAND_FLOOR };
            self.peak[k] = (self.peak[k] * 0.996).max(rms[k]).max(floor);
            level[k] = if rms[k] < 1e-4 { 0.0 } else { (1.0 + 20.0 * (rms[k] / self.peak[k]).log10() / RANGE_DB).clamp(0.0, 1.0) };
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
        let beat = rms[0] > 1.5 * self.bass_avg && rms[0] > 0.3 * self.peak[0] && self.since_beat > 6;
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
    let rainbow = l.viz != "colors";
    // Band hues run red (bass) to violet (treble), slowly turning.
    let hue = |band: f32| band * 75.0 + t * 12.0;
    let color_of = |band: usize, v: f32| if rainbow { hsv_hex(hue(band as f32), 1.0, 1.0) } else { mix_hex(&l.viz_low, &l.viz_high, v) };
    let flash = |c: String| mix_hex(&c, "#ffffff", f.glow * 0.6);
    let solid = |c: String| Light { mode: "static".into(), color: c.clone(), color2: c, mute_color: String::new() };
    // Pro knobs: bass, low mids, everything, high mids, treble. Mini: the four bands.
    let knob_bands: &[Option<usize>] = if knobs < KNOBS { &[Some(0), Some(1), Some(2), Some(3)] } else { &[Some(0), Some(1), None, Some(2), Some(3)] };
    for (i, band) in knob_bands.iter().enumerate() {
        let (v, c) = match band {
            Some(b) => (f.bands[*b], color_of(*b, f.bands[*b])),
            None => (f.level, if rainbow { hsv_hex(hue(1.5) + 180.0, 1.0, 1.0) } else { mix_hex(&l.viz_low, &l.viz_high, f.level) }),
        };
        out.controls[i].light = solid(flash(scale_hex(&c, 0.06 + 0.94 * v)));
    }
    for k in 0..CONTROLS - KNOBS {
        let v = f.bands[k];
        let (c1, c2) = if rainbow { (color_of(k, v), hsv_hex(hue(k as f32) + 40.0, 1.0, 1.0)) } else { (l.viz_low.clone(), l.viz_high.clone()) };
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
        for chunk in tone(60.0, 1.0, 0.5).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!(f.bands[0] > 0.9, "bass lights up: {:?}", f.bands);
        assert!(f.bands[3] < f.bands[0], "treble stays lower: {:?}", f.bands);
        let mut a = Analyzer::new(48000.0);
        for chunk in tone(8000.0, 1.0, 0.5).chunks(1600) {
            f = a.feed(chunk);
        }
        assert!(f.bands[3] > 0.9 && f.bands[0] < 0.5, "treble lights up: {:?}", f.bands);
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
