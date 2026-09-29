//! Keystrokes, processes, Voicemeeter, autostart.
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;
use windows::core::{s, w, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub const NO_WINDOW: u32 = 0x0800_0000;

fn vk(name: &str) -> Option<(u16, bool)> {
    let n = name.trim().to_lowercase();
    let ext = |v: VIRTUAL_KEY| Some((v.0, true));
    let key = |v: VIRTUAL_KEY| Some((v.0, false));
    match n.as_str() {
        "ctrl" | "control" => key(VK_CONTROL),
        "shift" => key(VK_SHIFT),
        "alt" => key(VK_MENU),
        "win" | "super" | "meta" => ext(VK_LWIN),
        "enter" | "return" => key(VK_RETURN),
        "space" => key(VK_SPACE),
        "tab" => key(VK_TAB),
        "esc" | "escape" => key(VK_ESCAPE),
        "backspace" => key(VK_BACK),
        "delete" | "del" => ext(VK_DELETE),
        "insert" | "ins" => ext(VK_INSERT),
        "home" => ext(VK_HOME),
        "end" => ext(VK_END),
        "pgup" | "pageup" => ext(VK_PRIOR),
        "pgdn" | "pagedown" => ext(VK_NEXT),
        "up" => ext(VK_UP),
        "down" => ext(VK_DOWN),
        "left" => ext(VK_LEFT),
        "right" => ext(VK_RIGHT),
        "printscreen" | "prtsc" => ext(VK_SNAPSHOT),
        "play_pause" | "play" => ext(VK_MEDIA_PLAY_PAUSE),
        "next" => ext(VK_MEDIA_NEXT_TRACK),
        "prev" | "previous" => ext(VK_MEDIA_PREV_TRACK),
        "stop" => ext(VK_MEDIA_STOP),
        "vol_up" => ext(VK_VOLUME_UP),
        "vol_down" => ext(VK_VOLUME_DOWN),
        "mute" => ext(VK_VOLUME_MUTE),
        _ if n.len() > 1 && n.starts_with('f') => {
            let f: u16 = n[1..].parse().ok().filter(|f| (1..=24).contains(f))?;
            key(VIRTUAL_KEY(VK_F1.0 + f - 1))
        }
        "[" => key(VK_OEM_4),
        "]" => key(VK_OEM_6),
        "," => key(VK_OEM_COMMA),
        "." => key(VK_OEM_PERIOD),
        "-" => key(VK_OEM_MINUS),
        "=" => key(VK_OEM_PLUS),
        ";" => key(VK_OEM_1),
        "'" => key(VK_OEM_7),
        "/" => key(VK_OEM_2),
        "\\" => key(VK_OEM_5),
        "`" => key(VK_OEM_3),
        _ if n.len() == 1 => {
            let c = n.chars().next()?.to_ascii_uppercase();
            c.is_ascii_alphanumeric().then_some((c as u16, false))
        }
        _ => None,
    }
}

/// Press a combo like "ctrl+shift+m" or "play_pause". It can end in a mouse wheel step:
/// "scroll_up", "scroll_down", "scroll_left", "scroll_right" (e.g. "ctrl+scroll_up" zooms in).
pub fn send_keys(combo: &str) -> Result<(), String> {
    let mut parts: Vec<&str> = combo.split('+').collect();
    let wheel = match parts.last().map(|k| k.trim().to_lowercase()).as_deref() {
        Some("scroll_up") => Some((MOUSEEVENTF_WHEEL, 120)),
        Some("scroll_down") => Some((MOUSEEVENTF_WHEEL, -120)),
        Some("scroll_right") => Some((MOUSEEVENTF_HWHEEL, 120)),
        Some("scroll_left") => Some((MOUSEEVENTF_HWHEEL, -120)),
        _ => None,
    };
    if wheel.is_some() {
        parts.pop();
    }
    let keys: Vec<(u16, bool)> = parts.iter()
        .map(|k| vk(k).ok_or_else(|| format!("unknown key '{k}'")))
        .collect::<Result<_, _>>()?;
    let input = |(v, ext): (u16, bool), up: bool| {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if ext { flags |= KEYEVENTF_EXTENDEDKEY; }
        if up { flags |= KEYEVENTF_KEYUP; }
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(v), dwFlags: flags, ..Default::default() } },
        }
    };
    let mut seq: Vec<INPUT> = keys.iter().map(|&k| input(k, false)).collect();
    if let Some((flags, delta)) = wheel {
        let mi = MOUSEINPUT { mouseData: delta as u32, dwFlags: flags, ..Default::default() };
        seq.push(INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi } });
    }
    seq.extend(keys.iter().rev().map(|&k| input(k, true)));
    unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
    Ok(())
}

pub fn run(cmd: &str) -> Result<(), String> {
    Command::new("cmd").raw_arg("/C").raw_arg(cmd).creation_flags(NO_WINDOW).spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn kill(process: &str) -> Result<(), String> {
    Command::new("taskkill").args(["/F", "/IM", process]).creation_flags(NO_WINDOW).spawn().map(|_| ()).map_err(|e| e.to_string())
}

type VmSet = unsafe extern "system" fn(*const u8) -> i32;

/// Run a Voicemeeter Remote script such as `Strip[0].Mute=1;`.
pub fn voicemeeter(script: &str) -> Result<(), String> {
    static API: OnceLock<Option<VmSet>> = OnceLock::new();
    let set = API.get_or_init(|| unsafe {
        let lib = LoadLibraryW(w!("C:\\Program Files (x86)\\VB\\Voicemeeter\\VoicemeeterRemote64.dll"))
            .or_else(|_| LoadLibraryW(w!("VoicemeeterRemote64.dll")))
            .ok()?;
        let login: unsafe extern "system" fn() -> i32 = std::mem::transmute(GetProcAddress(lib, s!("VBVMR_Login"))?);
        login();
        Some(std::mem::transmute::<_, VmSet>(GetProcAddress(lib, s!("VBVMR_SetParameters"))?))
    });
    let set = set.ok_or("Voicemeeter Remote API not found")?;
    let c = std::ffi::CString::new(script).map_err(|e| e.to_string())?;
    match unsafe { set(c.as_ptr() as _) } {
        0 => Ok(()),
        e => Err(format!("Voicemeeter error {e}")),
    }
}

/// Top-level app windows of a process (visible, titled, not owned), front-most first.
fn app_windows(exe: &str) -> Vec<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    struct Find<'a> { exe: &'a str, out: Vec<HWND> }
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> windows::core::BOOL {
        let f = &mut *(lp.0 as *mut Find);
        let visible = IsWindowVisible(h).as_bool() && GetWindowTextLengthW(h) > 0;
        let owned = GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if visible && !owned {
            let mut pid = 0;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if crate::audio::process_name(pid) == f.exe {
                f.out.push(h);
            }
        }
        true.into()
    }
    let mut f = Find { exe, out: vec![] };
    unsafe { let _ = EnumWindows(Some(cb), LPARAM(&mut f as *mut _ as isize)); }
    f.out
}

/// Titles of an app's top-level windows.
pub fn window_titles(exe: &str) -> Vec<String> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;
    app_windows(exe).into_iter().map(|h| {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(h, &mut buf) } as usize;
        String::from_utf16_lossy(&buf[..n])
    }).collect()
}

/// Bring an app to the front (restoring it if minimized), or start it.
pub fn focus_app(exe: &str, launch: &str, toggle: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::*;
    if exe.is_empty() {
        return Err("no app chosen".into());
    }
    let Some(&h) = app_windows(exe).first() else {
        return open(if launch.trim().is_empty() { exe } else { launch });
    };
    unsafe {
        if toggle && GetForegroundWindow() == h {
            let _ = ShowWindow(h, SW_MINIMIZE);
            return Ok(());
        }
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        // Windows only lets the app that got the last input take focus; a synthetic Alt tap counts.
        let alt = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_MENU, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
        };
        SendInput(&[alt(false), alt(true)], std::mem::size_of::<INPUT>() as i32);
        let _ = SetForegroundWindow(h);
    }
    Ok(())
}

/// Open a URL, file, folder or program with its default handler.
pub fn open(target: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let t = target.trim().trim_matches('"');
    if t.is_empty() {
        return Err("nothing to open".into());
    }
    let w: Vec<u16> = t.encode_utf16().chain([0]).collect();
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL) };
    if r.0 as usize > 32 { Ok(()) } else { Err(format!("couldn't open {t}")) }
}

/// Type text as if on the keyboard (any language; Enter for newlines).
pub fn type_text(text: &str) {
    let mut seq = vec![];
    for line in text.split('\n').enumerate().flat_map(|(i, l)| (i > 0).then_some(None).into_iter().chain([Some(l)])) {
        match line {
            None => for up in [false, true] {
                seq.push(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT {
                    wVk: VK_RETURN, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } } });
            },
            Some(l) => for u in l.trim_end_matches('\r').encode_utf16() {
                for up in [false, true] {
                    let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
                    seq.push(INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: u, dwFlags: flags, ..Default::default() } } });
                }
            },
        }
    }
    unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
}

/// HTTP request via Windows' built-in curl.exe (handles https without bundling TLS). Blocks up to 10 s.
pub fn http(method: &str, url: &str, headers: &str, body: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("the URL must start with http:// or https://".into());
    }
    let method = if method.trim().is_empty() { "POST" } else { method.trim() };
    let mut c = Command::new("curl.exe");
    c.args(["-sS", "-f", "--max-time", "10", "-o", "NUL", "-X", method]);
    for h in headers.lines().map(str::trim).filter(|h| !h.is_empty()) {
        c.args(["-H", h]);
    }
    if !body.is_empty() {
        c.args(["--data-raw", body]);
    }
    let out = c.arg("--").arg(url).creation_flags(NO_WINDOW).output().map_err(|e| format!("curl.exe: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// GET a URL (following redirects) and return the body.
pub fn http_get(url: &str) -> Result<String, String> {
    let out = Command::new("curl.exe").args(["-sSfL", "--max-time", "20", "--", url]).creation_flags(NO_WINDOW).output()
        .map_err(|e| format!("curl.exe: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Download a URL (following redirects) to a file.
pub fn download(url: &str, to: &std::path::Path) -> Result<(), String> {
    let out = Command::new("curl.exe").args(["-sSfL", "--max-time", "300", "-o"]).arg(to).arg("--").arg(url)
        .creation_flags(NO_WINDOW).output().map_err(|e| format!("curl.exe: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn lock() {
    unsafe { let _ = windows::Win32::System::Shutdown::LockWorkStation(); }
}

/// Connected displays as (id like \.\DISPLAY1, "Display 1 · 2560×1440").
pub fn displays() -> Vec<(String, String)> {
    let mut out = vec![];
    for_each_monitor(|_, id, w, h| {
        let n = id.trim_start_matches(r"\\.\DISPLAY");
        out.push((id.to_string(), format!("Display {n} · {w}×{h}")));
    });
    out.sort();
    out
}

fn for_each_monitor(mut f: impl FnMut(windows::Win32::Graphics::Gdi::HMONITOR, &str, i32, i32)) {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::*;
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> windows::core::BOOL {
        let f = &mut *(lp.0 as *mut &mut dyn FnMut(HMONITOR, &str, i32, i32));
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(m, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            let id = String::from_utf16_lossy(&info.szDevice).trim_end_matches('\0').to_string();
            let r = info.monitorInfo.rcMonitor;
            f(m, &id, r.right - r.left, r.bottom - r.top);
        }
        true.into()
    }
    let mut dynf: &mut dyn FnMut(HMONITOR, &str, i32, i32) = &mut f;
    unsafe { let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut dynf as *mut _ as isize)); }
}

/// Toggle chosen displays off/on with DDC/CI (VCP 0xD6: 1 = on, 5 = off).
pub fn toggle_displays(ids: &[String]) -> Result<(), String> {
    use windows::Win32::Devices::Display::*;
    let mut done = 0;
    for_each_monitor(|m, id, _, _| unsafe {
        if !ids.iter().any(|x| x.eq_ignore_ascii_case(id)) {
            return;
        }
        let mut n = 0u32;
        if GetNumberOfPhysicalMonitorsFromHMONITOR(m, &mut n).is_err() || n == 0 {
            return;
        }
        let mut phys = vec![PHYSICAL_MONITOR::default(); n as usize];
        if GetPhysicalMonitorsFromHMONITOR(m, &mut phys).is_err() {
            return;
        }
        for p in &phys {
            let (mut cur, mut max) = (0u32, 0u32);
            let on = GetVCPFeatureAndVCPFeatureReply(p.hPhysicalMonitor, 0xD6, None, &mut cur, Some(&mut max)) == 0 || cur == 1;
            // Monitors list which power codes they accept, e.g. "D6(01 04)"; many ignore 5 (off) but take 4 (standby).
            let mut caps = String::new();
            let mut len = 0u32;
            if GetCapabilitiesStringLength(p.hPhysicalMonitor, &mut len) != 0 && len > 0 {
                let mut buf = vec![0u8; len as usize];
                if CapabilitiesRequestAndCapabilitiesReply(p.hPhysicalMonitor, &mut buf) != 0 {
                    caps = String::from_utf8_lossy(&buf).trim_end_matches(' ').to_string();
                }
            }
            if SetVCPFeature(p.hPhysicalMonitor, 0xD6, if on { crate::ddc::off_code(&caps) } else { 1 }) != 0 {
                done += 1;
            }
        }
        let _ = DestroyPhysicalMonitors(&phys);
    });
    if done == 0 {
        return Err("those displays didn't respond - turn on DDC/CI in the monitor's menu, or use 'All displays'".into());
    }
    Ok(())
}

/// Turn every display off (they wake on mouse/keyboard input).
/// Sent to our own window so its default handling performs it; a broadcast from a background thread is often ignored.
pub fn monitor_off() -> Result<(), String> {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND};
    let raw = crate::shellhook::HWND_RAW.load(std::sync::atomic::Ordering::Relaxed);
    if raw == 0 {
        return Err("the app's helper window isn't ready yet".into());
    }
    // Give the knob release a moment to pass so it can't count as "activity" and wake the screens.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        unsafe { let _ = PostMessageW(Some(HWND(raw as _)), WM_SYSCOMMAND, WPARAM(SC_MONITORPOWER as usize), LPARAM(2)); }
    });
    Ok(())
}

/// Apps using a microphone right now (lowercase exe names, or Store app ids), read from the same
/// records Windows uses for its "microphone in use" icon.
pub fn mic_users() -> Vec<String> {
    const BASE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let mut out = vec![];
    for (path, desktop) in [(BASE.to_string(), false), (format!(r"{BASE}\NonPackaged"), true)] {
        let Ok(key) = windows_registry::CURRENT_USER.open(&path) else { continue };
        let Ok(names) = key.keys() else { continue };
        for name in names.filter(|n| desktop || n != "NonPackaged") {
            let Ok(app) = key.open(&name) else { continue };
            if app.get_u64("LastUsedTimeStop").is_ok_and(|t| t == 0) && app.get_u64("LastUsedTimeStart").is_ok_and(|t| t > 0) {
                // Desktop apps are stored as their path, with # in place of \.
                out.push(name.rsplit('#').next().unwrap_or(&name).to_lowercase());
            }
        }
    }
    out
}

/// Processes of the official PCPanel software (it runs as javaw.exe + sndctrl.exe from its install folder).
pub fn official_app_pids() -> Vec<u32> {
    let mut pids = vec![0u32; 4096];
    let mut used = 0u32;
    let ok = unsafe {
        windows::Win32::System::ProcessStatus::EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut used).is_ok()
    };
    if !ok {
        return vec![];
    }
    pids.truncate(used as usize / 4);
    pids.into_iter()
        .filter(|&pid| crate::audio::process_path(pid).to_lowercase().contains("\\pcpanel software\\"))
        .collect()
}

/// Close the official PCPanel software; optionally stop it starting with Windows.
pub fn close_official_app(stop_autostart: bool) -> Result<usize, String> {
    let pids = official_app_pids();
    for pid in &pids {
        let _ = Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).creation_flags(NO_WINDOW).status();
    }
    if stop_autostart {
        // Remove only Run entries that launch the official app.
        let key = windows_registry::CURRENT_USER.create("Software\\Microsoft\\Windows\\CurrentVersion\\Run").map_err(|e| e.to_string())?;
        for name in key.values().map_err(|e| e.to_string())?.map(|(n, _)| n).collect::<Vec<_>>() {
            if key.get_string(&name).is_ok_and(|cmd| cmd.to_lowercase().contains("pcpanel software\\pcpanel.exe")) {
                key.remove_value(&name).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(pids.len())
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP_VALUE: PCWSTR = w!("PCPanelRevive");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, APP_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_VALUE, REG_SZ.0, Some(v.as_ptr() as _), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_VALUE);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_keys() {
        assert_eq!(super::vk("a"), Some((b'A' as u16, false)));
        assert_eq!(super::vk("F13").map(|k| k.0), Some(0x7C));
        assert!(super::vk("f25").is_none());
        assert!(super::vk("bogus").is_none());
    }
}
