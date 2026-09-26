#![cfg_attr(not(test), windows_subsystem = "windows")]
mod audio;
mod config;
mod engine;
mod import;
mod apps;
mod settings;
mod hid;
mod obs;
mod shellhook;
mod osd;
mod sys;
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
    last_log: String,
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

/// 32x32 RGBA: dark rounded tile with five dots, drawn at runtime to avoid shipping an asset.
pub fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    const SS: usize = 4; // supersampling per axis, for smooth edges
    // Knob layout of the Pro: K1 K2 on top, K3 K4 K5 below; everything centered on (16, 16).
    let dots = [(11.0, 12.0), (21.0, 12.0), (6.0, 20.0), (16.0, 20.0), (26.0, 20.0)];
    let colors = [[255.0, 59.0, 48.0], [255.0, 149.0, 0.0], [52.0, 199.0, 89.0], [0.0, 122.0, 255.0], [175.0, 82.0, 222.0]];
    let tile = [34.0, 34.0, 40.0];
    let mut px = Vec::with_capacity(N * N * 4);
    for y in 0..N {
        for x in 0..N {
            let (mut rgb, mut alpha) = ([0.0f32; 3], 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let (fx, fy) = (x as f32 + (sx as f32 + 0.5) / SS as f32, y as f32 + (sy as f32 + 0.5) / SS as f32);
                    // Rounded square: 30px wide, radius 6.
                    let (dx, dy) = (((fx - 16.0).abs() - 9.0).max(0.0), ((fy - 16.0).abs() - 9.0).max(0.0));
                    if dx * dx + dy * dy > 36.0 {
                        continue;
                    }
                    let c = dots.iter().position(|&(cx, cy)| (fx - cx).powi(2) + (fy - cy).powi(2) <= 6.5).map_or(tile, |i| colors[i]);
                    (0..3).for_each(|k| rgb[k] += c[k]);
                    alpha += 1.0;
                }
            }
            let n = (SS * SS) as f32;
            let [r, g, b] = rgb.map(|v| if alpha > 0.0 { (v / alpha) as u8 } else { 0 });
            px.extend_from_slice(&[r, g, b, (alpha / n * 255.0) as u8]);
        }
    }
    px
}

fn build_menu(shared: &Mutex<Shared>) -> Menu {
    let s = crate::lock(&shared);
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("open", "Settings...", true, None));
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
    unsafe {
        let _m = CreateMutexW(None, true, w!("Local\\PCPanelReviveSingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            open_settings();
            return;
        }
    }
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
    log_file("started");

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
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let id = e.id.0.as_str();
        match id {
            "open" => open_settings(),
            "reload" => { let _ = tx.send(Msg::Reload); }
            "autostart" => { sys::set_autostart(!sys::autostart_enabled()); post(WM_REFRESH); }
            "quit" => post(WM_QUIT_APP),
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
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for (i, p) in px.chunks(4).enumerate() {
            if p[3] == 255 && (p[0], p[1], p[2]) != (34, 34, 40) {
                sx += (i % 32) as f64 + 0.5;
                sy += (i / 32) as f64 + 0.5;
                n += 1.0;
            }
        }
        assert!((sx / n - 16.0).abs() < 0.6, "x centroid {}", sx / n);
        let top = px.chunks(4).enumerate().filter(|(i, p)| i / 32 < 16 && p[0] == 255 && p[1] == 59).count();
        assert!(top > 0, "K1 (red) must be on the top row");
    }
}
