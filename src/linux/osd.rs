//! On-screen popups on Linux. KDE Plasma: its own volume/text popup (what nvdweem/PCPanel uses).
//! Elsewhere: the desktop's notification bubble, replaced in place on every change and marked
//! transient so it doesn't pile up in the notification list. The cheat sheet is its own GTK window.
pub use crate::osd_info::{Icon, Info, SheetItem};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// The notification we keep replacing (0 = none yet).
static LAST: AtomicU32 = AtomicU32::new(0);
/// One call at a time, so each knob step replaces the last bubble instead of racing it to a new one.
static BUSY: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn init() {}

fn kde() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.contains("KDE"))
}

/// A GVariant string literal.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn gdbus(dest: &str, path: &str, method: &str, args: &[String]) -> Option<String> {
    let out = Command::new("gdbus").args(["call", "--session", "--dest", dest, "--object-path", path, "--method", method])
        .args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn show(info: &Info) {
    if !info.sheet.is_empty() || info.hide {
        return sheet::show(info);
    }
    let title = match (&info.icon, info.exe_name) {
        (Icon::Exe(path), true) if !path.is_empty() => crate::apps::friendly_name(path),
        _ => info.title.clone(),
    };
    let pct = info.level.filter(|_| info.marker.is_none()).map(|v| (v * 100.0).round() as i32);
    let theme_icon = match info.icon {
        Icon::Mic => "audio-input-microphone",
        Icon::Light => "display-brightness",
        Icon::Profile => "preferences-desktop",
        _ if info.muted => "audio-volume-muted",
        _ => "audio-volume-high",
    };
    let body = match pct {
        Some(p) if info.muted => format!("{p}% (muted)"),
        Some(p) => format!("{p}%"),
        None => info.hint.clone(),
    };
    if kde() {
        let text = if body.is_empty() { title } else { format!("{title}: {body}") };
        std::thread::spawn(move || gdbus("org.kde.plasmashell", "/org/kde/osdService", "org.kde.osdService.showText", &[theme_icon.into(), text]));
        return;
    }
    let icon = match &info.icon {
        Icon::Exe(path) => crate::apps::icon_file(path.rsplit('/').next().unwrap_or(path))
            .map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| theme_icon.into()),
        _ => theme_icon.into(),
    };
    // A level bar where the desktop draws one (XFCE, Ubuntu); the text says the same everywhere else.
    let mut hints = vec!["'transient': <true>".to_string(), "'x-canonical-private-synchronous': <'pcpanel-revive'>".into()];
    if let Some(p) = pct {
        hints.push(format!("'value': <int32 {}>", p.clamp(0, 100)));
    }
    let mut args = [
        "'PCPanel Revive'".to_string(),
        String::new(), // the id to replace, filled in below
        quote(&icon),
        quote(&title),
        quote(&body),
        "[]".into(),
        format!("{{{}}}", hints.join(", ")),
        (if info.stay_ms > 0 { info.stay_ms } else { 1500 }).to_string(),
    ];
    // Off the tray thread; the reply gives the id to replace next time.
    std::thread::spawn(move || {
        let _one = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        args[1] = LAST.load(Ordering::Relaxed).to_string();
        let reply = gdbus("org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications.Notify", &args);
        // "(uint32 42,)"
        if let Some(id) = reply.and_then(|r| r.split_whitespace().nth(1).and_then(|n| n.trim_end_matches(",)").parse().ok())) {
            LAST.store(id, Ordering::Relaxed);
        }
    });
}

/// The cheat sheet: a map of the panel with a card for each control where it sits on the device
/// (Pro: two knobs, three knobs, four sliders; Mini and original: one row of four), centered on
/// screen, drawn like the Windows one. Runs on the tray thread, which is GTK's.
mod sheet {
    use super::Info;
    use gtk::cairo::{Context, FontSlant, FontWeight};
    use gtk::prelude::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    thread_local! {
        static WINDOW: RefCell<Option<(gtk::Window, Rc<RefCell<Info>>)>> = const { RefCell::new(None) };
        /// Bumped on every show, so an older hide timer leaves a newer sheet alone.
        static SHOWN: Cell<u64> = const { Cell::new(0) };
    }

    const CW: f64 = 222.0;
    const GAP: f64 = 12.0;
    const PAD: f64 = 20.0;
    const HEAD: f64 = 56.0;
    const SPLIT: f64 = 14.0;

    fn rows(n: usize) -> Vec<Vec<usize>> {
        if n > 4 { vec![vec![0, 1], vec![2, 3, 4], (5..n).collect()] } else { vec![(0..n).collect()] }
    }

    /// Width, height and card height. Cards are as tall as the busiest one needs: tag, name, up to four lines.
    fn size(info: &Info) -> (f64, f64, f64) {
        let most = info.sheet.iter().map(|it| it.lines.len().min(4)).max().unwrap_or(0).max(1) as f64;
        let ch = 72.0 + 19.0 * most;
        let n = rows(info.sheet.len()).len() as f64;
        let w = PAD * 2.0 + CW * 4.0 + GAP * 3.0;
        let h = HEAD + n * (ch + GAP) - GAP + if info.sheet.len() > 4 { SPLIT } else { 0.0 } + PAD;
        (w, h, ch)
    }

    fn rgb(c: &Context, r: u8, g: u8, b: u8) {
        c.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    }

    fn rounded(c: &Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
        use std::f64::consts::PI;
        c.new_sub_path();
        c.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
        c.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
        c.arc(x + r, y + h - r, r, PI / 2.0, PI);
        c.arc(x + r, y + r, r, PI, 1.5 * PI);
        c.close_path();
    }

    /// One line of text from (x, baseline y), cut with "..." to fit `room`.
    fn text(c: &Context, s: &str, x: f64, y: f64, room: f64, size: f64, bold: bool) {
        c.select_font_face("Sans", FontSlant::Normal, if bold { FontWeight::Bold } else { FontWeight::Normal });
        c.set_font_size(size);
        let fits = |t: &str| c.text_extents(t).map_or(true, |e| e.x_advance() <= room);
        let mut t = s.to_string();
        if !fits(&t) {
            while t.chars().count() > 1 && !fits(&format!("{t}...")) {
                t.pop();
            }
            t += "...";
        }
        c.move_to(x, y);
        let _ = c.show_text(&t);
    }

    fn draw(c: &Context, info: &Info) {
        let (w, h, ch) = size(info);
        c.set_operator(gtk::cairo::Operator::Source);
        c.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        let _ = c.paint();
        c.set_operator(gtk::cairo::Operator::Over);
        rgb(c, 24, 24, 30);
        rounded(c, 0.0, 0.0, w, h, 14.0);
        let _ = c.fill();
        rgb(c, 240, 240, 245);
        text(c, &info.title, PAD, 36.0, w - PAD * 2.0 - 200.0, 19.0, true);
        rgb(c, 140, 140, 152);
        c.set_font_size(13.0);
        let hint_w = c.text_extents(&info.hint).map_or(0.0, |e| e.x_advance()).min(200.0);
        text(c, &info.hint, w - PAD - hint_w, 36.0, 200.0, 13.0, false);
        let mut y = HEAD;
        for (r, row) in rows(info.sheet.len()).iter().enumerate() {
            if r == 2 {
                y += SPLIT; // a little space between the knobs and the sliders, as on the panel
            }
            let mut x = (w - (row.len() as f64 * CW + (row.len() as f64 - 1.0) * GAP)) / 2.0;
            for &idx in row {
                let it = &info.sheet[idx];
                rgb(c, 38, 38, 47);
                rounded(c, x, y, CW, ch, 12.0);
                let _ = c.fill();
                // The control's shape in its light color: a ring for a knob, a bar for a slider.
                let [cr, cg, cb] = it.color;
                rgb(c, cr, cg, cb);
                if it.knob {
                    c.set_line_width(3.0);
                    c.arc(x + 20.0, y + 20.0, 7.0, 0.0, std::f64::consts::TAU);
                    let _ = c.stroke();
                } else {
                    rounded(c, x + 17.0, y + 11.0, 6.0, 18.0, 3.0);
                    let _ = c.fill();
                }
                // The tag in a lighter shade of the light's color, so dark colors stay readable.
                let light = |v: u8| (v as u32 + (255 - v as u32) * 2 / 5) as u8;
                rgb(c, light(cr), light(cg), light(cb));
                text(c, &it.tag, x + 36.0, y + 26.0, CW - 46.0, 14.0, true);
                let unset = it.title.is_empty() && it.lines.is_empty();
                if unset { rgb(c, 110, 110, 120) } else { rgb(c, 240, 240, 245) }
                text(c, if unset { "Not used" } else { &it.title }, x + 12.0, y + 53.0, CW - 22.0, 15.0, true);
                rgb(c, 170, 170, 182);
                for (l, line) in it.lines.iter().take(4).enumerate() {
                    text(c, line, x + 12.0, y + 76.0 + 19.0 * l as f64, CW - 22.0, 13.0, false);
                }
                x += CW + GAP;
            }
            y += ch + GAP;
        }
    }

    fn hide() {
        WINDOW.with(|w| if let Some((win, _)) = &*w.borrow() { win.hide() });
    }

    fn window(info: &Info) -> (gtk::Window, Rc<RefCell<Info>>) {
        // A popup (no border, no focus, above everything) on X11; Wayland only allows ordinary windows.
        let x11 = std::env::var_os("WAYLAND_DISPLAY").is_none();
        let win = gtk::Window::new(if x11 { gtk::WindowType::Popup } else { gtk::WindowType::Toplevel });
        win.set_decorated(false);
        win.set_keep_above(true);
        win.set_accept_focus(false);
        win.set_skip_taskbar_hint(true);
        win.set_app_paintable(true);
        // See-through corners where the desktop can blend windows.
        if let Some(v) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
            win.set_visual(Some(&v));
        }
        let current = Rc::new(RefCell::new(info.clone()));
        let area = gtk::DrawingArea::new();
        let data = current.clone();
        area.connect_draw(move |_, c| {
            draw(c, &data.borrow());
            gtk::glib::Propagation::Stop
        });
        win.add(&area);
        (win, current)
    }

    pub fn show(info: &Info) {
        if info.hide {
            return hide();
        }
        let shown = SHOWN.with(|s| {
            s.set(s.get() + 1);
            s.get()
        });
        WINDOW.with(|cell| {
            let mut cell = cell.borrow_mut();
            let (win, current) = cell.get_or_insert_with(|| window(info));
            *current.borrow_mut() = info.clone();
            let (w, h, _) = size(info);
            win.set_size_request(w as i32, h as i32);
            win.resize(w as i32, h as i32);
            win.set_position(gtk::WindowPosition::CenterAlways);
            win.show_all();
            win.queue_draw();
        });
        // Under a press it shows for a few seconds; under Hold, until the knob is let go (a hide).
        let ms = if info.stay_ms > 0 { info.stay_ms } else { 8000 };
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || {
            if SHOWN.with(|s| s.get()) == shown {
                hide();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_for_gdbus() {
        assert_eq!(super::quote("Tom's \\ app"), r"'Tom\'s \\ app'");
    }
}
