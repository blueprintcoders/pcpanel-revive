//! Notifications on Linux, followed live on the session bus with `dbus-monitor`: each Notify call
//! names its app, its reply gives the notification's id, and NotificationClosed says when it's gone.
//! One that only timed out still counts (desktops keep those in their list); dismissed or withdrawn
//! ones don't.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

pub struct Source {
    pub handler: String,
    pub latest: i64,
    pub count: i64,
}

#[derive(Default)]
struct State {
    /// Every Notify call so far (a clock for "newest").
    calls: i64,
    /// Newest Notify call per app.
    latest: HashMap<String, i64>,
    /// Open notifications: id -> app.
    open: HashMap<u32, String>,
    /// Notify calls waiting for their reply: (caller, serial) -> app.
    waiting: HashMap<(String, String), String>,
    /// What the next argument line belongs to.
    expect: Expect,
    running: bool,
}

#[derive(Default)]
enum Expect {
    #[default]
    Nothing,
    App(String, String),
    Id(String),
    Closed,
    /// After a closed notification's id: 1 expired, 2 dismissed, 3 closed by the app.
    Reason(u32),
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key)).map(|v| v.trim_end_matches(';'))
}

impl State {
    fn line(&mut self, line: &str) {
        let t = line.trim_start();
        if !line.starts_with(' ') {
            // A new message.
            self.expect = Expect::Nothing;
            if t.starts_with("method call") && t.contains("member=Notify") {
                if let (Some(from), Some(serial)) = (field(t, "sender="), field(t, "serial=")) {
                    self.expect = Expect::App(from.into(), serial.into());
                }
            } else if t.starts_with("method return") {
                if let (Some(to), Some(serial)) = (field(t, "destination="), field(t, "reply_serial=")) {
                    if let Some(app) = self.waiting.remove(&(to.to_string(), serial.to_string())) {
                        self.expect = Expect::Id(app);
                    }
                }
            } else if t.starts_with("signal") && t.contains("member=NotificationClosed") {
                self.expect = Expect::Closed;
            }
            return;
        }
        match std::mem::take(&mut self.expect) {
            Expect::App(from, serial) => {
                // The first argument: the app's name.
                let app = t.strip_prefix("string \"").map(|s| s.trim_end_matches('"').to_string()).unwrap_or_default();
                self.calls += 1;
                self.latest.insert(app.clone(), self.calls);
                self.waiting.insert((from, serial), app);
            }
            Expect::Id(app) => {
                if let Some(id) = t.strip_prefix("uint32 ").and_then(|n| n.parse().ok()) {
                    self.open.insert(id, app);
                }
            }
            Expect::Closed => {
                if let Some(id) = t.strip_prefix("uint32 ").and_then(|n| n.parse().ok()) {
                    self.expect = Expect::Reason(id);
                }
            }
            Expect::Reason(id) => {
                if t != "uint32 1" {
                    self.open.remove(&id);
                }
            }
            Expect::Nothing => {}
        }
    }
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Follow the notification service from now on (once).
fn watch() {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    if s.running {
        return;
    }
    s.running = true;
    std::thread::spawn(|| loop {
        let child = Command::new("dbus-monitor")
            .args(["--session", "interface='org.freedesktop.Notifications'", "type='method_return'"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        let Ok(mut child) = child else {
            state().lock().unwrap_or_else(|e| e.into_inner()).running = false;
            return;
        };
        for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
            state().lock().unwrap_or_else(|e| e.into_inner()).line(&line);
        }
        let _ = child.wait();
        std::thread::sleep(std::time::Duration::from_secs(10));
    });
}

/// Notifications per app since the app started watching.
pub fn sources() -> Result<Vec<Source>, String> {
    watch();
    let s = state().lock().unwrap_or_else(|e| e.into_inner());
    if !s.running {
        return Err("notification alerts need dbus-monitor (the dbus package)".into());
    }
    Ok(s.latest.iter().map(|(app, &latest)| Source {
        handler: app.clone(),
        latest,
        count: s.open.values().filter(|a| *a == app).count() as i64,
    }).collect())
}

/// Does a notification source belong to this alert? Same rule as on Windows.
pub fn matches(handler: &str, exe: &str, pattern: &str) -> bool {
    let h = handler.to_lowercase();
    let want = if pattern.trim().is_empty() { exe.trim_end_matches(".exe").to_lowercase() } else { pattern.trim().to_lowercase() };
    !want.is_empty() && h.contains(&want)
}

#[cfg(test)]
mod tests {
    use super::State;

    const TRACE: &str = r#"method call time=1.0 sender=:1.50 -> destination=org.freedesktop.Notifications serial=7 path=/org/freedesktop/Notifications; interface=org.freedesktop.Notifications; member=Notify
   string "discord"
   uint32 0
   string "icon"
method return time=1.1 sender=:1.20 -> destination=:1.50 serial=90 reply_serial=7
   uint32 42
method call time=2.0 sender=:1.61 -> destination=org.freedesktop.Notifications serial=3 path=/org/freedesktop/Notifications; interface=org.freedesktop.Notifications; member=Notify
   string "Slack"
   uint32 0
method return time=2.1 sender=:1.20 -> destination=:1.61 serial=91 reply_serial=3
   uint32 43
method return time=2.2 sender=:1.9 -> destination=:1.61 serial=5 reply_serial=4
   uint32 999
signal time=3.0 sender=:1.20 -> destination=(null destination) serial=92 path=/org/freedesktop/Notifications; interface=org.freedesktop.Notifications; member=NotificationClosed
   uint32 42
   uint32 1
signal time=4.0 sender=:1.20 -> destination=(null destination) serial=93 path=/org/freedesktop/Notifications; interface=org.freedesktop.Notifications; member=NotificationClosed
   uint32 43
   uint32 2
"#;

    /// On a desktop: sends a notification with notify-send and expects to see it.
    #[test]
    #[ignore]
    fn notif_live() {
        super::sources().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        std::process::Command::new("notify-send").args(["-a", "PcpTest", "PCPanel Revive test", "you can ignore this"]).status().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let s = super::sources().unwrap();
        let t = s.iter().find(|s| s.handler == "PcpTest").expect("saw the notification");
        println!("PcpTest latest={} open={}", t.latest, t.count);
        assert_eq!(t.count, 1);
    }

    #[test]
    fn follows_notifications() {
        let mut s = State::default();
        for l in TRACE.lines() {
            s.line(l);
        }
        assert_eq!(s.latest["discord"], 1);
        assert_eq!(s.latest["Slack"], 2);
        // Discord's only timed out, so it still counts; Slack's was dismissed.
        assert_eq!(s.open.len(), 1);
        assert_eq!(s.open[&42], "discord");
        assert!(super::matches("discord", "discord.exe", ""));
    }
}
