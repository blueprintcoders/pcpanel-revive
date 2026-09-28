//! The Linux side of the tray app: one copy at a time (a socket in the user's runtime folder, which
//! a second copy uses to ask for the settings window), the GTK loop the tray icon needs, local time.
use crate::{Shared, UiMsg};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;

static PROXY: OnceLock<Mutex<EventLoopProxy<UiMsg>>> = OnceLock::new();
/// Without a desktop session there's no GTK loop: messages come through here instead.
static HEADLESS: OnceLock<Mutex<std::sync::mpsc::Sender<UiMsg>>> = OnceLock::new();
static SETTINGS_SOCKET: Mutex<Option<UnixListener>> = Mutex::new(None);

fn socket(name: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("pcpanel-revive-{}", unsafe { libc::getuid() })));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name)
}

/// Listen on a socket that only one copy can own. None if a live copy already has it.
fn claim(name: &str) -> Option<UnixListener> {
    let path = socket(name);
    if UnixStream::connect(&path).is_ok() {
        return None;
    }
    let _ = std::fs::remove_file(&path); // left behind by a copy that crashed
    UnixListener::bind(&path).ok()
}

/// Send a word to the copy that owns a socket. False if none answers.
pub fn tell(name: &str, word: &str) -> bool {
    UnixStream::connect(socket(name)).and_then(|mut s| s.write_all(word.as_bytes())).is_ok()
}

/// Every word sent to a socket, on its own thread.
fn serve(l: UnixListener, on: impl Fn(&str) + Send + 'static) {
    std::thread::spawn(move || {
        for mut c in l.incoming().map_while(Result::ok) {
            let mut word = String::new();
            let _ = c.read_to_string(&mut word);
            on(word.trim());
        }
    });
}

pub const TRAY_SOCKET: &str = "pcpanel-revive.sock";
pub const SETTINGS_SOCKET_NAME: &str = "pcpanel-revive-settings.sock";

/// Become the only running copy. False when another copy is running (it's asked to open its settings).
pub fn single_instance(updated: bool) -> bool {
    for _ in 0..if updated { 40 } else { 1 } {
        if let Some(l) = claim(TRAY_SOCKET) {
            serve(l, |word| crate::open_settings(word == "devtools"));
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    tell(TRAY_SOCKET, "open");
    false
}

/// Only one settings window: a second one brings the first to the front instead. False if one is open.
pub fn settings_single_instance() -> bool {
    match claim(SETTINGS_SOCKET_NAME) {
        Some(l) => {
            *SETTINGS_SOCKET.lock().unwrap() = Some(l);
            true
        }
        None => {
            tell(SETTINGS_SOCKET_NAME, "focus");
            false
        }
    }
}

/// The settings window calls `on_focus` when another copy asks it to come to the front.
pub fn on_settings_focus(on_focus: impl Fn() + Send + 'static) {
    if let Some(l) = SETTINGS_SOCKET.lock().unwrap().take() {
        serve(l, move |_| on_focus());
    }
}

pub fn init() {}

/// Hand a message to the tray thread (from any thread).
pub fn post(m: UiMsg) {
    if let Some(p) = PROXY.get() {
        let _ = p.lock().unwrap().send_event(m);
    } else if let Some(tx) = HEADLESS.get() {
        let _ = tx.lock().unwrap().send(m);
    }
}

/// Run the tray thread (GTK, through tao) until Quit. `on` handles our messages (a Refresh first, to set up)
/// and returns false to quit.
pub fn run(shared: &Mutex<Shared>, mut on: impl FnMut(UiMsg) -> bool) {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // No desktop (started too early, or over SSH): the panel still works, just without the tray icon.
        crate::log(shared, "no desktop session, so no tray icon".into());
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = HEADLESS.set(Mutex::new(tx));
        while let Ok(m) = rx.recv() {
            if m == UiMsg::Quit {
                return;
            }
        }
        return;
    }
    let mut event_loop = EventLoopBuilder::<UiMsg>::with_user_event().build();
    let _ = PROXY.set(Mutex::new(event_loop.create_proxy()));
    let mut started = false;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        let m = match event {
            Event::NewEvents(_) if !started => {
                started = true;
                UiMsg::Refresh
            }
            Event::UserEvent(m) => m,
            _ => return,
        };
        if !on(m) {
            *flow = ControlFlow::Exit;
        }
    });
}

/// Local time as (year, month, day, hour, minute, second).
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        ((tm.tm_year + 1900) as u16, (tm.tm_mon + 1) as u16, tm.tm_mday as u16, tm.tm_hour as u16, tm.tm_min as u16, tm.tm_sec as u16)
    }
}

/// Like "Ubuntu 24.04.1 LTS (KDE, wayland)", for problem reports.
pub fn os_version() -> String {
    let name = std::fs::read_to_string("/etc/os-release").unwrap_or_default().lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME=")).unwrap_or("Linux").trim_matches('"').to_string();
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| "?".into());
    format!("{name} ({}, {})", var("XDG_CURRENT_DESKTOP"), var("XDG_SESSION_TYPE"))
}
