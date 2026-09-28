//! A hidden window on the tray thread that tells the engine when any app's taskbar button starts flashing
//! (how chat apps signal a new message), and when the PC locks, its screens turn off or it goes to sleep.
use crate::{Idle, Msg};
use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The hidden window's class: other copies of the app find the running one by it.
pub const CLASS: windows::core::PCWSTR = w!("PCPanelReviveShellHook");
/// Sent by another copy of the app: open the settings window (wParam 1 = with DevTools).
pub const WM_OPEN_SETTINGS: u32 = WM_APP + 20;

/// HSHELL_REDRAW | HSHELL_HIGHBIT: sent when a window flashes its taskbar button.
const HSHELL_FLASH: u32 = 0x8006;

/// Our hidden window, also used to send "monitor off" (Windows ignores it when broadcast from a windowless background thread).
pub static HWND_RAW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

thread_local! {
    static TX: RefCell<Option<Sender<Msg>>> = const { RefCell::new(None) };
    static HOOK_MSG: Cell<u32> = const { Cell::new(0) };
}

pub fn init(tx: Sender<Msg>) {
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&class);
        // Shell hooks need a real top-level window (not message-only); it's never shown.
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW, CLASS, w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None) else { return };
        HWND_RAW.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        HOOK_MSG.set(RegisterWindowMessageW(w!("SHELLHOOK")));
        TX.with(|t| *t.borrow_mut() = Some(tx));
        let _ = RegisterShellHookWindow(hwnd);
        // Lock/unlock, screens off/on (reported right away, then on every change), sleep/wake.
        let _ = windows::Win32::System::RemoteDesktop::WTSRegisterSessionNotification(hwnd, 0); // this session
        let _ = windows::Win32::System::Power::RegisterPowerSettingNotification(HANDLE(hwnd.0), &CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE);
    }
}

/// GUID_CONSOLE_DISPLAY_STATE: whether the screens are on.
const CONSOLE_DISPLAY_STATE: windows::core::GUID = windows::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);

fn send(m: Msg) {
    TX.with(|t| if let Some(tx) = t.borrow().as_ref() { let _ = tx.send(m); });
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_WTSSESSION_CHANGE {
        match wp.0 as u32 {
            WTS_SESSION_LOCK => send(Msg::Idle(Idle::Locked, true)),
            WTS_SESSION_UNLOCK => send(Msg::Idle(Idle::Locked, false)),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == WM_POWERBROADCAST {
        match wp.0 as u32 {
            PBT_APMSUSPEND => send(Msg::Idle(Idle::Asleep, true)),
            PBT_APMRESUMEAUTOMATIC => send(Msg::Idle(Idle::Asleep, false)),
            PBT_POWERSETTINGCHANGE => {
                let setting = &*(lp.0 as *const windows::Win32::System::Power::POWERBROADCAST_SETTING);
                if setting.PowerSetting == CONSOLE_DISPLAY_STATE {
                    send(Msg::Idle(Idle::ScreensOff, setting.Data[0] == 0)); // 0 off, 1 on, 2 dimmed
                }
            }
            _ => {}
        }
        return LRESULT(1);
    }
    if msg == WM_OPEN_SETTINGS {
        crate::open_settings(wp.0 != 0);
        return LRESULT(0);
    }
    if msg == HOOK_MSG.get() && msg != 0 && wp.0 as u32 == HSHELL_FLASH {
        let mut pid = 0;
        GetWindowThreadProcessId(HWND(lp.0 as _), Some(&mut pid));
        let exe = crate::audio::process_name(pid);
        if !exe.is_empty() {
            send(Msg::Flash(exe));
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
