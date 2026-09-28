#![cfg_attr(not(test), windows_subsystem = "windows")]
mod audio;
mod config;
mod engine;
mod import;
mod apps;
mod settings;
mod hid;
mod icon;
mod notif;
mod obs;
mod shellhook;
mod osd;
mod sys;
mod update;
mod viz;
mod wavelink;
mod web;

use config::{Config, CONTROLS, KNOBS};
use std::collections::VecDeque;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const PORT: u16 = 47831;
pub const WM_REFRESH: u32 = WM_APP;
const WM_QUIT_APP: u32 = WM_APP + 1;
pub const WM_OSD: u32 = WM_APP + 2;

pub enum Msg {
    Hid(hid::Event),
    Profile(String),
    Reload,
    /// Run one action now, from the settings window's Test button.
    Test(usize, config::Action),
    /// An app's taskbar button started flashing (exe name).
    Flash(String),
    /// Show alert N for a few seconds (settings window preview).
    PreviewAlert(usize),
    /// Panel self-test: while on, controls report input but run no actions.
    TestMode(bool),
    /// Panel self-test: light only this LED (0-8 controls, 9 logo), or back to normal.
    TestLight(Option<usize>),
}

#[derive(Default)]
pub struct Shared {
    pub config: Config,
    pub values: [u8; CONTROLS],
    pub connected: bool,
    pub log: VecDeque<String>,
    pub lights: Vec<hid::Report>,
    pub lights_gen: u64,
    /// Lighting updates the device accepted (diagnostics).
    pub lights_written: u64,
    pub muted: [bool; CONTROLS],
    /// Current volume each control is driving, when known.
    pub levels: [Option<f32>; CONTROLS],
    /// False when a control's app isn't running / device isn't connected.
    pub present: [bool; CONTROLS],
    pub osd: Option<osd::Info>,
    /// Which configured alerts are lit right now.
    pub alerts_on: Vec<bool>,
    pub model: hid::Model,
    /// Knob buttons held down right now (self-test).
    pub buttons: [bool; KNOBS],
    /// Live audio levels of "pulse with audio" lights (0..1).
    pub peaks: [f32; CONTROLS],
    /// The official PCPanel software is running too (both apps would react to the panel).
    pub official_running: bool,
    /// A newer version on GitHub, and the result of the last check or install, for the settings window.
    pub update: Option<update::Release>,
    pub update_note: String,
    last_log: String,
}

static UI_THREAD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Ask the tray thread to refresh the tray menu and tooltip.
pub fn refresh_tray() {
    unsafe { let _ = PostThreadMessageW(UI_THREAD.load(std::sync::atomic::Ordering::Relaxed), WM_REFRESH, WPARAM(0), LPARAM(0)); }
}

/// Check GitHub for a newer version now; `manual` also reports "up to date" and errors.
pub fn check_update(shared: &Mutex<Shared>, manual: bool) {
    match update::check() {
        Ok(found) => {
            let mut s = lock(shared);
            if found.is_some() && s.update.is_none() {
                drop(s);
                log(shared, format!("version {} is available", found.as_ref().unwrap().version));
                s = lock(shared);
            }
            s.update_note = if found.is_some() { String::new() } else { "You have the latest version.".into() };
            s.update = found;
        }
        Err(e) if manual => lock(shared).update_note = format!("Couldn't check: {e}"),
        Err(_) => {}
    }
    refresh_tray();
}

/// Download and install the update, then quit so the new version takes over.
pub fn install_update(shared: &Mutex<Shared>) {
    let Some(r) = lock(shared).update.clone() else { return };
    lock(shared).update_note = format!("Downloading version {}...", r.version);
    match update::install(&r) {
        Ok(()) => {
            log(shared, format!("updated to version {}", r.version));
            unsafe { let _ = PostThreadMessageW(UI_THREAD.load(std::sync::atomic::Ordering::Relaxed), WM_QUIT_APP, WPARAM(0), LPARAM(0)); }
        }
        Err(e) => {
            log(shared, format!("update failed: {e}"));
            lock(shared).update_note = format!("Update failed: {e}");
        }
    }
}

/// Lock that survives a panic in another thread (a poisoned lock would take every other thread down too).
pub fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Local time as "2026-09-26 13:45:02".
fn now() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

pub fn log_path() -> std::path::PathBuf {
    config::path().with_file_name("pcpanel-revive.log")
}

/// Append to the log file; keeps it under ~1 MB by rolling to .old.
pub fn log_file(msg: &str) {
    use std::io::Write;
    let path = log_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}  {msg}", now());
    }
}

pub fn log(shared: &Mutex<Shared>, msg: String) {
    {
        let mut s = lock(shared);
        if s.last_log == msg {
            return;
        }
        s.last_log = msg.clone();
        let line = format!("{}  {msg}", &now()[11..]);
        s.log.push_back(line);
        if s.log.len() > 200 {
            s.log.pop_front();
        }
    }
    log_file(&msg);
}

fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
    }
}

/// The 32x32 app icon for the tray and the settings window.
pub fn icon_rgba() -> Vec<u8> {
    icon::rgba(32)
}

fn build_menu(shared: &Mutex<Shared>) -> Menu {
    let s = crate::lock(&shared);
    let menu = Menu::new();
    if let Some(u) = &s.update {
        let _ = menu.append(&MenuItem::with_id("update", format!("Update to version {}", u.version), true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
    }
    let _ = menu.append(&MenuItem::with_id("open", "Settings", true, None));
    let profiles = Submenu::new("Profile", true);
    for name in s.config.profiles.keys() {
        let _ = profiles.append(&CheckMenuItem::with_id(format!("p:{name}"), name, true, *name == s.config.active, None));
    }
    let _ = menu.append(&profiles);
    let _ = menu.append(&MenuItem::with_id("reload", "Reload config", true, None));
    let _ = menu.append(&CheckMenuItem::with_id("autostart", "Start with Windows", true, sys::autostart_enabled(), None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit", "Quit", true, None));
    menu
}

fn tooltip(shared: &Mutex<Shared>) -> String {
    let s = crate::lock(&shared);
    let state = if s.connected { "connected" } else { "not connected" };
    format!("PCPanel Revive - {state} - {}", s.config.active)
}

fn main() {
    if std::env::args().any(|a| a == "--settings") {
        return settings();
    }
    // Right after an update, the old version may still be closing: wait for it.
    let updated = std::env::args().any(|a| a == "--updated");
    for tries in 0.. {
        unsafe {
            let m = CreateMutexW(None, true, w!("Local\\PCPanelReviveSingleInstance"));
            if GetLastError() != ERROR_ALREADY_EXISTS {
                break;
            }
            if !updated || tries >= 40 {
                open_settings();
                return;
            }
            if let Ok(h) = m {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    update::cleanup();
    std::panic::set_hook(Box::new(|info| {
        let t = std::thread::current();
        log_file(&format!("CRASH in {} thread: {info}", t.name().unwrap_or("unnamed")));
    }));
    // Crisp popup and window rendering on high-DPI screens.
    unsafe { let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2); }
    let shared = Arc::new(Mutex::new(Shared { present: [true; CONTROLS], ..Default::default() }));
    let cfg = config::load().unwrap_or_else(|e| {
        log(&shared, format!("config error, using defaults (file untouched): {e}"));
        let mut c = Config::default();
        c.normalize();
        c
    });
    crate::lock(&shared).config = cfg.clone();

    let ui_thread = unsafe { GetCurrentThreadId() };
    UI_THREAD.store(ui_thread, std::sync::atomic::Ordering::Relaxed);
    let post = move |m: u32| unsafe { let _ = PostThreadMessageW(ui_thread, m, WPARAM(0), LPARAM(0)); };
    let (tx, rx) = channel();

    hid::spawn(tx.clone(), shared.clone());
    web::spawn(tx.clone(), shared.clone());
    {
        // The engine restarts itself after a crash, reloading the config from disk.
        let shared = shared.clone();
        std::thread::Builder::new().name("engine".into()).spawn(move || {
            let mut cfg = Some(cfg);
            loop {
                let c = cfg.take().unwrap_or_else(|| config::load().unwrap_or_else(|_| { let mut c = Config::default(); c.normalize(); c }));
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine::run(&rx, c, shared.clone(), Box::new(post))));
                if run.is_ok() {
                    return; // channel closed: shutting down
                }
                log(&shared, "the engine crashed and was restarted (details in the log file)".into());
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }).expect("engine thread");
    }
    log_file(&format!("started version {}", env!("CARGO_PKG_VERSION")));
    {
        // Look for updates a little after startup, then once a day.
        let shared = shared.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            if lock(&shared).config.update_check && update::REPO.is_some() {
                check_update(&shared, false);
            }
            std::thread::sleep(std::time::Duration::from_secs(24 * 3600));
        });
    }

    let tray: TrayIcon = TrayIconBuilder::new()
        .with_icon(Icon::from_rgba(icon_rgba(), 32, 32).unwrap())
        .with_tooltip(tooltip(&shared))
        .with_menu(Box::new(build_menu(&shared)))
        .with_menu_on_left_click(false)
        .build()
        .expect("tray icon");

    osd::init();
    shellhook::init(tx.clone());

    TrayIconEvent::set_event_handler(Some(|e| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            open_settings();
        }
    }));
    let menu_shared = shared.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let id = e.id.0.as_str();
        match id {
            "open" => open_settings(),
            "reload" => { let _ = tx.send(Msg::Reload); }
            "autostart" => { sys::set_autostart(!sys::autostart_enabled()); post(WM_REFRESH); }
            "quit" => post(WM_QUIT_APP),
            "update" => {
                let shared = menu_shared.clone();
                std::thread::spawn(move || install_update(&shared));
            }
            _ => if let Some(p) = id.strip_prefix("p:") { let _ = tx.send(Msg::Profile(p.to_string())); },
        }
    }));

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_REFRESH => {
                    let _ = tray.set_tooltip(Some(tooltip(&shared)));
                    tray.set_menu(Some(Box::new(build_menu(&shared))));
                }
                WM_OSD => {
                    let info = crate::lock(&shared).osd.take();
                    if let Some(info) = info {
                        osd::show(&info);
                    }
                }
                WM_QUIT_APP => break,
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    // Leave the LEDs as they are; the device keeps its last state.
}

/// Settings window process; focuses the existing window instead of opening a second one.
fn settings() {
    unsafe {
        let _m = CreateMutexW(None, true, w!("Local\\PCPanelReviveSettings"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let title: Vec<u16> = settings::TITLE.encode_utf16().chain([0]).collect();
            if let Ok(hwnd) = FindWindowW(None, windows::core::PCWSTR(title.as_ptr())) {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
    }
    settings::run();
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump_icon() {
        std::fs::write(std::env::var("ICON_OUT").unwrap(), super::icon_rgba()).unwrap();
    }

    #[test]
    fn a_crashed_thread_does_not_lock_out_the_others() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(super::Shared::default()));
        let s2 = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = s2.lock().unwrap();
            panic!("simulated crash while holding the lock");
        }).join();
        assert!(shared.is_poisoned());
        super::lock(&shared).connected = true; // still usable
        assert!(super::lock(&shared).connected);
    }

    #[test]
    fn icon_is_centered() {
        // Alpha-weighted centroid of the non-tile (dot) pixels sits on the tile center.
        let px = super::icon_rgba();
        let (mut sx, mut n) = (0.0, 0.0);
        for (i, p) in px.chunks(4).enumerate() {
            if p[3] == 255 && (p[0], p[1], p[2]) != (34, 34, 40) {
                sx += (i % 32) as f64 + 0.5;
                n += 1.0;
            }
        }
        assert!((sx / n - 16.0).abs() < 0.6, "x centroid {}", sx / n);
        let top = px.chunks(4).enumerate().filter(|(i, p)| i / 32 < 16 && p[0] == 255 && p[1] == 59).count();
        assert!(top > 0, "K1 (red) must be on the top row");
    }
}
