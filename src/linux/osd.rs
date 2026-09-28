//! On-screen popups on Linux. KDE Plasma: its own volume/text popup (what nvdweem/PCPanel uses).
//! Elsewhere: the desktop's notification bubble, replaced in place on every change and marked
//! transient so it doesn't pile up in the notification list. The cheat sheet isn't on Linux yet.
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
        return;
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

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_for_gdbus() {
        assert_eq!(super::quote("Tom's \\ app"), r"'Tom\'s \\ app'");
    }
}
