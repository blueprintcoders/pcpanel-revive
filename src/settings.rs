//! Settings window: a native window hosting the HTML UI (served by the tray app) in WebView2.
//! Runs as its own process (`--settings`) so the tray app stays small.
use crate::PORT;
use std::rc::Rc;
use std::cell::Cell;
use std::time::Duration;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, Theme, WindowBuilder};
use wry::{WebContext, WebViewBuilder, WebViewBuilderExtWindows};

pub const TITLE: &str = "PCPanel Revive Settings";

/// Close the settings window if it's open, and wait (up to 3 s) until it has saved and gone.
/// Returns whether it was open.
pub fn close_window() -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
    let title: Vec<u16> = TITLE.encode_utf16().chain([0]).collect();
    let find = || unsafe { FindWindowW(None, windows::core::PCWSTR(title.as_ptr())) }.ok().filter(|h| !h.is_invalid());
    let Some(hwnd) = find() else { return false };
    unsafe { let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)); }
    for _ in 0..30 {
        if find().is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

enum UserEvent { Focus, Dirty(bool), Exit }

/// Ask the running tray app to open the settings window. Returns false when it isn't running.
pub fn ask_tray(devtools: bool) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
    match unsafe { FindWindowW(crate::shellhook::CLASS, None) } {
        Ok(hwnd) if !hwnd.is_invalid() => unsafe {
            PostMessageW(Some(hwnd), crate::shellhook::WM_OPEN_SETTINGS, WPARAM(devtools as usize), LPARAM(0)).is_ok()
        },
        _ => false,
    }
}

pub fn run() {
    let devtools = std::env::args().any(|a| a == "--devtools");
    // Only the tray app hands out the key to its settings server, so a window started any other way
    // asks the tray app to open one (starting it first if needed).
    let Ok(token) = std::env::var("PCP_TOKEN") else {
        if !ask_tray(devtools) {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if ask_tray(devtools) {
                    break;
                }
            }
        }
        return;
    };
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(TITLE)
        .with_inner_size(LogicalSize::new(1180.0, 820.0))
        .with_min_inner_size(LogicalSize::new(560.0, 400.0))
        .with_theme(Some(Theme::Dark))
        .with_window_icon(Icon::from_rgba(crate::icon_rgba(), 32, 32).ok())
        .build(&event_loop)
        .expect("window");
    // Keep WebView2's cache next to the config, not next to the exe.
    let mut ctx = WebContext::new(Some(crate::config::path().with_file_name("webview")));
    let proxy = event_loop.create_proxy();
    let exit = event_loop.create_proxy();
    // `--devtools` opens Chromium's DevTools protocol on localhost so tools (e.g. the Chrome DevTools MCP
    // server) can drive this window without the real mouse and keyboard. Off by default:
    // anything on this PC could control the page while it's on.
    let port = std::env::var("PCP_DEVTOOLS_PORT").unwrap_or_else(|_| "9222".into());
    let mut browser_args = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string(); // wry's defaults
    if devtools {
        browser_args += &format!(" --remote-debugging-port={port} --remote-debugging-address=127.0.0.1");
    }
    let webview = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_additional_browser_args(browser_args)
        .with_url(format!("http://127.0.0.1:{PORT}/"))
        .with_initialization_script(&format!("window.PCP_TOKEN = {:?};", token))
        .with_background_color((20, 20, 24, 255))
        .with_ipc_handler(move |req| {
            let _ = proxy.send_event(match req.body().as_str() {
                "focus" => UserEvent::Focus,
                "dirty" => UserEvent::Dirty(true),
                _ => UserEvent::Dirty(false),
            });
        })
        .build(&window)
        .expect("WebView2 is missing; install the Evergreen runtime from Microsoft");

    let dirty = Rc::new(Cell::new(false));
    let closing = Rc::new(Cell::new(false));
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Focus) => {
                window.set_minimized(false);
                window.set_focus();
            }
            Event::UserEvent(UserEvent::Dirty(d)) => {
                dirty.set(d);
                if !d && closing.get() {
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::Exit) => *flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if !dirty.get() {
                    *flow = ControlFlow::Exit;
                    return;
                }
                // A change is still waiting for its autosave: save it now, then close (give up after 2 s).
                closing.set(true);
                window.set_visible(false);
                let _ = webview.evaluate_script("autosave()");
                let exit = exit.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(2));
                    let _ = exit.send_event(UserEvent::Exit);
                });
            }
            _ => {}
        }
    });
}
