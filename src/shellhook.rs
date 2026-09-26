//! Tells the engine when any app's taskbar button starts flashing (how chat apps signal a new message).
//! A hidden window on the tray thread receives Explorer's shell hook messages.
use crate::Msg;
use std::cell::{Cell, RefCell};
use std::sync::mpsc::Sender;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// HSHELL_REDRAW | HSHELL_HIGHBIT: sent when a window flashes its taskbar button.
const HSHELL_FLASH: u32 = 0x8006;

thread_local! {
    static TX: RefCell<Option<Sender<Msg>>> = const { RefCell::new(None) };
    static HOOK_MSG: Cell<u32> = const { Cell::new(0) };
}

pub fn init(tx: Sender<Msg>) {
    unsafe {
        let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: w!("PCPanelReviveShellHook"), ..Default::default() };
        RegisterClassW(&class);
        // Shell hooks need a real top-level window (not message-only); it's never shown.
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW, w!("PCPanelReviveShellHook"), w!(""), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None) else { return };
        HOOK_MSG.set(RegisterWindowMessageW(w!("SHELLHOOK")));
        TX.with(|t| *t.borrow_mut() = Some(tx));
        let _ = RegisterShellHookWindow(hwnd);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == HOOK_MSG.get() && msg != 0 && wp.0 as u32 == HSHELL_FLASH {
        let mut pid = 0;
        GetWindowThreadProcessId(HWND(lp.0 as _), Some(&mut pid));
        let exe = crate::audio::process_name(pid);
        if !exe.is_empty() {
            TX.with(|t| if let Some(tx) = t.borrow().as_ref() { let _ = tx.send(Msg::Flash(exe)); });
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
