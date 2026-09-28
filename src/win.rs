//! The Windows side of the tray app: one copy at a time, the tray thread's message loop, local time.
use crate::{settings, Shared, UiMsg};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;

static UI_THREAD: AtomicU32 = AtomicU32::new(0);

/// Become the only running copy. False when another copy is running (it's asked to open its settings).
/// Right after an update the old version may still be closing, so `updated` waits for it.
pub fn single_instance(updated: bool) -> bool {
    for tries in 0.. {
        unsafe {
            let m = CreateMutexW(None, true, w!("Local\\PCPanelReviveSingleInstance"));
            if GetLastError() != ERROR_ALREADY_EXISTS {
                return true;
            }
            if !updated || tries >= 40 {
                settings::ask_tray(false); // already running: it opens its settings window
                return false;
            }
            if let Ok(h) = m {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    unreachable!()
}

/// Only one settings window: a second one brings the first to the front instead. False if one is open.
pub fn settings_single_instance() -> bool {
    unsafe {
        let _m = CreateMutexW(None, true, w!("Local\\PCPanelReviveSettings"));
        if GetLastError() != ERROR_ALREADY_EXISTS {
            std::mem::forget(_m); // held until the process exits
            return true;
        }
        let title: Vec<u16> = settings::TITLE.encode_utf16().chain([0]).collect();
        if let Ok(hwnd) = FindWindowW(None, windows::core::PCWSTR(title.as_ptr())) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        false
    }
}

/// Crisp popup and window rendering on high-DPI screens.
pub fn init() {
    unsafe { let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2); }
    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
}

/// Hand a message to the tray thread (from any thread).
pub fn post(m: UiMsg) {
    unsafe { let _ = PostThreadMessageW(UI_THREAD.load(Ordering::Relaxed), WM_APP + m as u32, WPARAM(0), LPARAM(0)); }
}

/// Run the tray thread until Quit. `on` handles our messages (a Refresh first, to set up) and returns false to quit.
pub fn run(_shared: &Mutex<Shared>, mut on: impl FnMut(UiMsg) -> bool) {
    if !on(UiMsg::Refresh) {
        return;
    }
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let ours = [UiMsg::Refresh, UiMsg::Osd, UiMsg::Quit].into_iter().find(|&m| msg.message == WM_APP + m as u32);
            match ours {
                Some(m) => if !on(m) { break },
                None => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// Local time as (year, month, day, hour, minute, second).
pub fn local_time() -> (u16, u16, u16, u16, u16, u16) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Like "Windows 11 Pro 24H2 (build 26100)", for problem reports.
pub fn os_version() -> String {
    windows_registry::LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion").map(|k| {
        let get = |n: &str| k.get_string(n).unwrap_or_default();
        format!("{} {} (build {})", get("ProductName"), get("DisplayVersion"), get("CurrentBuild"))
    }).unwrap_or_default()
}
