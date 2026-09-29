//! Linux system actions. Keystrokes, typing and window control use xdotool, so they reach X11 apps
//! (and XWayland ones), not native Wayland windows. The window in front comes from hyprctl (Hyprland),
//! kdotool (KDE) or xdotool, like nvdweem/PCPanel does.
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Run a program quietly; Err with its message if it fails or isn't installed.
fn tool(prog: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(prog).args(args).stdin(Stdio::null()).output().map_err(|_| format!("{prog} isn't installed"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{prog}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Our key names as X keysyms.
fn keysym(name: &str) -> Option<String> {
    let n = name.trim().to_lowercase();
    let k = match n.as_str() {
        "ctrl" | "control" => "ctrl",
        "shift" => "shift",
        "alt" => "alt",
        "win" | "super" | "meta" => "super",
        "enter" | "return" => "Return",
        "space" => "space",
        "tab" => "Tab",
        "esc" | "escape" => "Escape",
        "backspace" => "BackSpace",
        "delete" | "del" => "Delete",
        "insert" | "ins" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pgup" | "pageup" => "Prior",
        "pgdn" | "pagedown" => "Next",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "printscreen" | "prtsc" => "Print",
        "play_pause" | "play" => "XF86AudioPlay",
        "next" => "XF86AudioNext",
        "prev" | "previous" => "XF86AudioPrev",
        "stop" => "XF86AudioStop",
        "vol_up" => "XF86AudioRaiseVolume",
        "vol_down" => "XF86AudioLowerVolume",
        "mute" => "XF86AudioMute",
        "[" => "bracketleft",
        "]" => "bracketright",
        "," => "comma",
        "." => "period",
        "-" => "minus",
        "=" => "equal",
        ";" => "semicolon",
        "'" => "apostrophe",
        "/" => "slash",
        "\\" => "backslash",
        "`" => "grave",
        _ if n.len() > 1 && n.starts_with('f') => {
            let f: u8 = n[1..].parse().ok().filter(|f| (1..=24).contains(f))?;
            return Some(format!("F{f}"));
        }
        _ if n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric()) => return Some(n),
        _ => return None,
    };
    Some(k.into())
}

/// Media and volume keys work without X: through the media player (playerctl) and the audio server.
fn media_key(key: &str) -> Option<Result<(), String>> {
    let run = |prog: &str, args: &[&str]| tool(prog, args).map(drop);
    Some(match key.trim().to_lowercase().as_str() {
        "play_pause" | "play" => run("playerctl", &["play-pause"]),
        "next" => run("playerctl", &["next"]),
        "prev" | "previous" => run("playerctl", &["previous"]),
        "stop" => run("playerctl", &["stop"]),
        "vol_up" => run("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+2%"]),
        "vol_down" => run("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "-2%"]),
        "mute" => run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "toggle"]),
        _ => return None,
    })
}

pub fn send_keys(combo: &str) -> Result<(), String> {
    if let Some(r) = media_key(combo).filter(|r| r.is_ok()) {
        return r;
    }
    let mut parts: Vec<&str> = combo.split('+').collect();
    // Mouse buttons 4-7 are the wheel: up, down, left, right.
    let wheel = match parts.last().map(|k| k.trim().to_lowercase()).as_deref() {
        Some("scroll_up") => Some("4"),
        Some("scroll_down") => Some("5"),
        Some("scroll_left") => Some("6"),
        Some("scroll_right") => Some("7"),
        _ => None,
    };
    if wheel.is_some() {
        parts.pop();
    }
    let keys: Vec<String> = parts.iter().map(|k| keysym(k).ok_or_else(|| format!("unknown key '{k}'"))).collect::<Result<_, _>>()?;
    let args: Vec<String> = match wheel {
        Some(button) => keys.iter().flat_map(|k| ["keydown".into(), k.clone()])
            .chain(["click".into(), button.into()])
            .chain(keys.iter().rev().flat_map(|k| ["keyup".into(), k.clone()]))
            .collect(),
        None => vec!["key".into(), "--clearmodifiers".into(), keys.join("+")],
    };
    tool("xdotool", &args.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
}

pub fn run(cmd: &str) -> Result<(), String> {
    Command::new("sh").args(["-c", cmd]).stdin(Stdio::null()).spawn().map(drop).map_err(|e| e.to_string())
}

/// Every process of a program (as "firefox.exe").
fn pids_of(exe: &str) -> Vec<u32> {
    std::fs::read_dir("/proc").into_iter().flatten().flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .filter(|&pid| crate::audio::process_name(pid) == exe)
        .collect()
}

pub fn kill(process: &str) -> Result<(), String> {
    let exe = process.trim().to_lowercase();
    let exe = if exe.ends_with(".exe") { exe } else { exe + ".exe" };
    let pids = pids_of(&exe);
    if pids.is_empty() {
        return Err(format!("{process} isn't running"));
    }
    for pid in pids {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    Ok(())
}

pub fn voicemeeter(_script: &str) -> Result<(), String> {
    Err("Voicemeeter is only on Windows".into())
}

/// Visible windows of a program, as X window ids.
fn app_windows(exe: &str) -> Vec<String> {
    pids_of(exe).into_iter()
        .flat_map(|pid| tool("xdotool", &["search", "--onlyvisible", "--pid", &pid.to_string()]).unwrap_or_default().lines().map(String::from).collect::<Vec<_>>())
        .collect()
}

pub fn window_titles(exe: &str) -> Vec<String> {
    app_windows(exe).iter().filter_map(|w| tool("xdotool", &["getwindowname", w]).ok()).map(|t| t.trim().to_string()).collect()
}

pub fn focus_app(exe: &str, launch: &str, toggle: bool) -> Result<(), String> {
    if exe.is_empty() {
        return Err("no app chosen".into());
    }
    let Some(w) = app_windows(exe).into_iter().next() else {
        return open(if launch.trim().is_empty() { exe.trim_end_matches(".exe") } else { launch });
    };
    if toggle && focused_pid().is_some_and(|p| crate::audio::process_name(p) == exe) {
        return tool("xdotool", &["windowminimize", &w]).map(drop);
    }
    tool("xdotool", &["windowactivate", &w]).map(drop)
}

/// Open a URL, file or folder with its default app, or start a program.
pub fn open(target: &str) -> Result<(), String> {
    let t = target.trim().trim_matches('"');
    if t.is_empty() {
        return Err("nothing to open".into());
    }
    if t.contains("://") || std::path::Path::new(t).exists() && !is_program(t) {
        return Command::new("xdg-open").arg(t).stdin(Stdio::null()).spawn().map(drop).map_err(|_| "xdg-open isn't installed".into());
    }
    run(t)
}

fn is_program(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn type_text(text: &str) {
    let _ = tool("xdotool", &["type", "--delay", "5", "--", text]);
}

/// HTTP request via curl (handles https without bundling TLS). Blocks up to 10 s.
pub fn http(method: &str, url: &str, headers: &str, body: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("the URL must start with http:// or https://".into());
    }
    let method = if method.trim().is_empty() { "POST" } else { method.trim() };
    let mut args = vec!["-sS", "-f", "--max-time", "10", "-o", "/dev/null", "-X", method];
    for h in headers.lines().map(str::trim).filter(|h| !h.is_empty()) {
        args.extend(["-H", h]);
    }
    if !body.is_empty() {
        args.extend(["--data-raw", body]);
    }
    args.extend(["--", url]);
    tool("curl", &args).map(drop)
}

/// GET a URL (following redirects) and return the body.
pub fn http_get(url: &str) -> Result<String, String> {
    tool("curl", &["-sSfL", "--max-time", "20", "--", url])
}

/// Download a URL (following redirects) to a file.
pub fn download(url: &str, to: &std::path::Path) -> Result<(), String> {
    tool("curl", &["-sSfL", "--max-time", "300", "-o", &to.to_string_lossy(), "--", url]).map(drop)
}

pub fn lock() {
    let _ = tool("loginctl", &["lock-session"]);
}

/// Monitors ddcutil can talk to, as (its display number, "Display 1 · LG HDR WQHD").
pub fn displays() -> Vec<(String, String)> {
    parse_detect(&tool("ddcutil", &["detect", "--brief"]).unwrap_or_default())
}

fn parse_detect(out: &str) -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = vec![];
    for line in out.lines() {
        if let Some(n) = line.strip_prefix("Display ").map(str::trim) {
            list.push((n.to_string(), format!("Display {n}")));
        } else if let (Some(last), Some(m)) = (list.last_mut(), line.trim().strip_prefix("Monitor:")) {
            // "GSM:LG HDR WQHD:123456" -> the model
            if let Some(model) = m.trim().split(':').nth(1).filter(|m| !m.is_empty()) {
                last.1 = format!("{} · {model}", last.1);
            }
        }
    }
    list
}

/// Toggle chosen monitors off/on over their cable (DDC/CI power mode, like on Windows), which leaves
/// the screen layout alone. Needs ddcutil and access to /dev/i2c-* (the i2c group, usually).
pub fn toggle_displays(ids: &[String]) -> Result<(), String> {
    let mut done = 0;
    for n in ids {
        let d = ["--display", n.as_str()];
        // "VCP D6 SNC x01": 1 = on.
        let on = tool("ddcutil", &[&d[..], &["getvcp", "D6", "--terse"]].concat()).map_or(true, |o| o.trim().ends_with("x01"));
        let code = if on {
            crate::ddc::off_code(&tool("ddcutil", &[&d[..], &["capabilities", "--verbose"]].concat()).unwrap_or_default())
        } else {
            1
        };
        if tool("ddcutil", &[&d[..], &["setvcp", "D6", &format!("{code:02x}")]].concat()).is_ok() {
            done += 1;
        }
    }
    if done == 0 {
        return Err("those displays didn't respond - install ddcutil, add yourself to the i2c group and turn on DDC/CI in the monitor's menu, or use 'All displays'".into());
    }
    Ok(())
}

/// Screens off: KDE (Wayland or X11), else X11's DPMS.
pub fn monitor_off() -> Result<(), String> {
    tool("kscreen-doctor", &["--dpms", "off"]).or_else(|_| tool("xset", &["dpms", "force", "off"])).map(drop)
        .map_err(|_| "turning the screens off needs KDE or X11 (xset)".into())
}

/// Apps recording from a microphone right now.
pub fn mic_users() -> Vec<String> {
    crate::audio::recording_apps()
}

/// The official PCPanel software doesn't run on Linux.
pub fn official_app_pids() -> Vec<u32> {
    vec![]
}

pub fn close_official_app(_stop_autostart: bool) -> Result<usize, String> {
    Ok(0)
}

fn autostart_file() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("autostart").join("pcpanel-revive.desktop")
}

pub fn autostart_enabled() -> bool {
    autostart_file().exists()
}

pub fn set_autostart(on: bool) {
    let f = autostart_file();
    if on {
        let exe = std::env::current_exe().unwrap_or_default();
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(&f, format!(
            "[Desktop Entry]\nType=Application\nName=PCPanel Revive\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\n", exe.display()));
    } else {
        let _ = std::fs::remove_file(f);
    }
}

/// Process id of the window in front, if the desktop tells us. Cached briefly: it's asked often.
pub fn focused_pid() -> Option<u32> {
    static CACHE: Mutex<Option<(Instant, Option<u32>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, pid)) = *c {
        if at.elapsed() < Duration::from_millis(250) {
            return pid;
        }
    }
    let pid = ask_focused_pid();
    *c = Some((Instant::now(), pid));
    pid
}

fn ask_focused_pid() -> Option<u32> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let v: serde_json::Value = serde_json::from_str(&tool("hyprctl", &["-j", "activewindow"]).ok()?).ok()?;
        return v["pid"].as_u64().filter(|&p| p > 0).map(|p| p as u32);
    }
    ["kdotool", "xdotool"].iter()
        .find_map(|t| tool(t, &["getactivewindow", "getwindowpid"]).ok())
        .and_then(|out| out.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_ddcutil_displays() {
        let out = "Display 1\n   I2C bus:  /dev/i2c-4\n   DRM connector:           card1-DP-1\n   Monitor:                 GSM:LG HDR WQHD:123456\n\nDisplay 2\n   I2C bus:  /dev/i2c-5\n   Monitor:                 DEL::\n\nInvalid display\n   I2C bus:  /dev/i2c-7\n";
        assert_eq!(super::parse_detect(out), [("1".to_string(), "Display 1 · LG HDR WQHD".to_string()), ("2".into(), "Display 2".into())]);
    }

    #[test]
    fn parses_keys() {
        assert_eq!(super::keysym("a").as_deref(), Some("a"));
        assert_eq!(super::keysym("Ctrl").as_deref(), Some("ctrl"));
        assert_eq!(super::keysym("F13").as_deref(), Some("F13"));
        assert_eq!(super::keysym("]").as_deref(), Some("bracketright"));
        assert!(super::keysym("f25").is_none());
        assert!(super::keysym("bogus").is_none());
    }
}
