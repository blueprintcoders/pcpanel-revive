//! Tells the engine when the PC locks or goes to sleep, from systemd-logind and the desktop's screen
//! saver over D-Bus (watched with gdbus), and when an app asks for attention (on X11, the Linux
//! version of a flashing taskbar button).
use crate::{Idle, Msg};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

pub fn init(tx: Sender<Msg>) {
    watch(&["--system", "--dest", "org.freedesktop.login1"], tx.clone());
    watch(&["--session", "--dest", "org.freedesktop.ScreenSaver"], tx.clone());
    watch(&["--session", "--dest", "org.gnome.ScreenSaver"], tx.clone());
    std::thread::spawn(move || urgent::watch(tx));
}

/// Windows that want attention: marked "demands attention" or "urgent", the way chat apps flag a
/// new message on X11. Wayland has no shared way to see this, so there it does nothing.
mod urgent {
    use crate::Msg;
    use std::collections::HashSet;
    use std::ffi::{c_int, c_ulong, CString};
    use std::sync::mpsc::Sender;
    use x11_dl::xlib::{self, Xlib};

    /// A window closing while we read it is normal; Xlib's default reaction (quitting) isn't.
    unsafe extern "C" fn ignore(_: *mut xlib::Display, _: *mut xlib::XErrorEvent) -> c_int {
        0
    }

    /// The 32-bit items of a window property (Xlib hands them over as longs).
    unsafe fn property(x: &Xlib, d: *mut xlib::Display, w: c_ulong, name: c_ulong) -> Vec<c_ulong> {
        let (mut kind, mut format, mut n, mut after, mut data) = (0, 0, 0, 0, std::ptr::null_mut());
        let ok = (x.XGetWindowProperty)(d, w, name, 0, 4096, 0, xlib::AnyPropertyType as c_ulong, &mut kind, &mut format, &mut n, &mut after, &mut data);
        if ok != 0 || data.is_null() {
            return vec![];
        }
        let out = if format == 32 { std::slice::from_raw_parts(data as *const c_ulong, n as usize).to_vec() } else { vec![] };
        (x.XFree)(data as *mut _);
        out
    }

    pub fn watch(tx: Sender<Msg>) {
        if std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let Ok(x) = Xlib::open() else { return };
        unsafe {
            let d = (x.XOpenDisplay)(std::ptr::null());
            if d.is_null() {
                return;
            }
            (x.XSetErrorHandler)(Some(ignore));
            let atom = |n: &str| (x.XInternAtom)(d, CString::new(n).unwrap().as_ptr(), 0);
            let (clients, state, attention, pid) =
                (atom("_NET_CLIENT_LIST"), atom("_NET_WM_STATE"), atom("_NET_WM_STATE_DEMANDS_ATTENTION"), atom("_NET_WM_PID"));
            let root = (x.XDefaultRootWindow)(d);
            let mut before: HashSet<c_ulong> = HashSet::new();
            loop {
                let mut now = HashSet::new();
                for w in property(&x, d, root, clients) {
                    let hints = (x.XGetWMHints)(d, w);
                    let flagged = !hints.is_null() && (*hints).flags & xlib::XUrgencyHint != 0;
                    if !hints.is_null() {
                        (x.XFree)(hints as *mut _);
                    }
                    if flagged || property(&x, d, w, state).contains(&attention) {
                        now.insert(w);
                        if !before.contains(&w) {
                            let exe = property(&x, d, w, pid).first().map(|&p| crate::audio::process_name(p as u32)).unwrap_or_default();
                            if !exe.is_empty() && tx.send(Msg::Flash(exe)).is_err() {
                                return;
                            }
                        }
                    }
                }
                before = now;
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}

/// What a line of `gdbus monitor` means for us.
fn parse(line: &str) -> Option<Msg> {
    let on = line.contains("(true,)");
    if line.contains(".PrepareForSleep") {
        Some(Msg::Idle(Idle::Asleep, on))
    } else if line.contains("login1.Session.Lock ") || line.contains("login1.Session.Unlock ") {
        Some(Msg::Idle(Idle::Locked, line.contains(".Lock ")))
    } else if line.contains("ScreenSaver.ActiveChanged") {
        Some(Msg::Idle(Idle::Locked, on))
    } else {
        None
    }
}

/// Follow a D-Bus service's signals for as long as the app runs (restarting gdbus if it stops).
fn watch(args: &'static [&'static str], tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let Ok(mut child) = Command::new("gdbus").arg("monitor").args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            return; // no gdbus: nothing to watch with
        };
        for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            if let Some(m) = parse(&line) {
                if tx.send(m).is_err() {
                    return;
                }
            }
        }
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}

#[cfg(test)]
mod tests {
    use crate::{Idle, Msg};

    /// On an X11 desktop: a window that asks for attention is reported with its program.
    #[test]
    #[ignore]
    fn urgent_live() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || super::urgent::watch(tx));
        let mut w = std::process::Command::new("python3").args(["-c", "import gi, time\ngi.require_version('Gtk', '3.0')\nfrom gi.repository import Gtk, GLib\nw = Gtk.Window(title='urgent test'); w.show_all()\nGLib.timeout_add(1500, lambda: w.set_urgency_hint(True))\nGLib.timeout_add(5000, Gtk.main_quit)\nGtk.main()"]).spawn().unwrap();
        let got = rx.recv_timeout(std::time::Duration::from_secs(5));
        let _ = w.kill();
        match got {
            Ok(Msg::Flash(exe)) => assert!(exe.starts_with("python3"), "{exe}"),
            _ => panic!("no attention request seen"),
        }
    }

    #[test]
    fn reads_logind_and_screensaver_signals() {
        let is = |line: &str, want: (u8, bool)| match super::parse(line) {
            Some(Msg::Idle(what, on)) => assert_eq!((what as u8, on), want, "{line}"),
            _ => panic!("{line}"),
        };
        is("/org/freedesktop/login1: org.freedesktop.login1.Manager.PrepareForSleep (true,)", (Idle::Asleep as u8, true));
        is("/org/freedesktop/login1: org.freedesktop.login1.Manager.PrepareForSleep (false,)", (Idle::Asleep as u8, false));
        is("/org/freedesktop/login1/session/_32: org.freedesktop.login1.Session.Lock ()", (Idle::Locked as u8, true));
        is("/org/freedesktop/login1/session/_32: org.freedesktop.login1.Session.Unlock ()", (Idle::Locked as u8, false));
        is("/ScreenSaver: org.freedesktop.ScreenSaver.ActiveChanged (true,)", (Idle::Locked as u8, true));
        assert!(super::parse("/org/freedesktop/login1: org.freedesktop.login1.Manager.SessionNew ('3', '/x')").is_none());
    }
}
