//! On-screen popups on Linux: KDE Plasma's own volume/text popup over D-Bus (what nvdweem/PCPanel
//! uses). Other desktops have no popup that can update in place, so they show none; the cheat sheet
//! isn't on Linux yet.
pub use crate::osd_info::{Icon, Info, SheetItem};
use std::process::{Command, Stdio};

pub fn init() {}

fn plasma(method: &str, args: &[String]) {
    let mut cmd = Command::new("gdbus");
    cmd.args(["call", "--session", "--dest", "org.kde.plasmashell", "--object-path", "/org/kde/osdService",
        "--method", &format!("org.kde.osdService.{method}")]).args(args);
    // Fire and forget: the tray thread mustn't wait on it.
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|mut c| std::thread::spawn(move || c.wait()));
}

pub fn show(info: &Info) {
    if !info.sheet.is_empty() || info.hide || std::env::var("XDG_CURRENT_DESKTOP").map_or(true, |d| !d.contains("KDE")) {
        return;
    }
    let icon = match info.icon {
        Icon::Mic => "audio-input-microphone",
        Icon::Light => "brightness-high",
        Icon::Profile => "configure",
        _ if info.muted => "audio-volume-muted",
        _ => "audio-volume-high",
    };
    let text = match (info.level, info.hint.is_empty()) {
        (Some(v), _) if info.marker.is_none() => format!("{} {}%", info.title, (v * 100.0).round()),
        (_, true) => info.title.clone(),
        _ => format!("{}: {}", info.title, info.hint),
    };
    plasma("showText", &[icon.into(), text]);
}
