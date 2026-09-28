//! Tells the engine when the PC locks or goes to sleep, from systemd-logind and the desktop's screen
//! saver over D-Bus (watched with gdbus). Taskbar-flash alerts aren't on Linux.
use crate::{Idle, Msg};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

pub fn init(tx: Sender<Msg>) {
    watch(&["--system", "--dest", "org.freedesktop.login1"], tx.clone());
    watch(&["--session", "--dest", "org.freedesktop.ScreenSaver"], tx.clone());
    watch(&["--session", "--dest", "org.gnome.ScreenSaver"], tx);
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
