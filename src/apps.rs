//! App picker data: apps with audio sessions or visible windows, with friendly names and icons.
use crate::audio::{self, Audio};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use std::collections::HashMap;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Windows plumbing that owns windows but is never something you'd put on a knob.
const HIDDEN: &[&str] = &[
    "pcpanel-revive.exe", "msedgewebview2.exe", "applicationframehost.exe", "textinputhost.exe", "shellexperiencehost.exe",
    "startmenuexperiencehost.exe", "searchhost.exe", "searchapp.exe", "rundll32.exe", "dllhost.exe", "conhost.exe",
];

#[derive(Serialize, Clone)]
pub struct App {
    pub exe: String,
    pub name: String,
    pub icon: String, // data: URI, "" if none
    pub audio: bool,
    pub path: String,
}

/// Remembers name/icon per exe path; icons are the slow part.
#[derive(Default)]
pub struct Cache(HashMap<String, (String, String)>);

impl Cache {
    pub fn list(&mut self, audio: Option<&Audio>) -> Vec<App> {
        let mut apps: HashMap<String, App> = HashMap::new();
        let mut add = |cache: &mut Self, path: String, is_audio: bool| {
            let exe = path.rsplit('\\').next().unwrap_or("").to_lowercase();
            if exe.is_empty() || HIDDEN.contains(&exe.as_str()) { return }
            if let Some(a) = apps.get_mut(&exe) { a.audio |= is_audio; return }
            let (name, icon) = cache.0.entry(path.clone()).or_insert_with(|| describe(&path)).clone();
            apps.insert(exe.clone(), App { exe, name, icon, audio: is_audio, path: path.clone() });
        };
        if let Some(a) = audio {
            for pid in a.session_pids() {
                let p = audio::process_path(pid);
                if !p.is_empty() { add(self, p, true) }
            }
        }
        for pid in window_pids() {
            let p = audio::process_path(pid);
            if !p.is_empty() { add(self, p, false) }
        }
        let mut v: Vec<App> = apps.into_values().collect();
        v.sort_by(|a, b| b.audio.cmp(&a.audio).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }
}

/// Processes owning a visible, titled, non-tool top-level window.
fn window_pids() -> Vec<u32> {
    unsafe extern "system" fn cb(hwnd: HWND, out: LPARAM) -> BOOL {
        let out = &mut *(out.0 as *mut Vec<u32>);
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 && ex & WS_EX_TOOLWINDOW.0 == 0 {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if !out.contains(&pid) { out.push(pid) }
        }
        true.into()
    }
    let mut out: Vec<u32> = vec![];
    unsafe { let _ = EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize)); }
    out
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// (friendly name from the exe's FileDescription, icon as a data: URI)
fn describe(path: &str) -> (String, String) {
    let stem = path.rsplit('\\').next().unwrap_or(path).trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    let name = file_description(path).filter(|d| !d.trim().is_empty()).unwrap_or(stem);
    (name.trim().to_string(), icon_uri(path).unwrap_or_default())
}

/// "Spotify" for C:\...\Spotify.exe: the exe's FileDescription, else its file name.
pub fn friendly_name(path: &str) -> String {
    describe_name(path)
}

fn describe_name(path: &str) -> String {
    let stem = path.rsplit('\\').next().unwrap_or(path).trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
    file_description(path).filter(|d| !d.trim().is_empty()).unwrap_or(stem).trim().to_string()
}

fn file_description(path: &str) -> Option<String> {
    unsafe {
        let p = wide(path);
        let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if size == 0 { return None }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, buf.as_mut_ptr() as _).ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        let q = wide("\\VarFileInfo\\Translation");
        let lang = if VerQueryValueW(buf.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() && len >= 4 {
            let t = ptr as *const u16;
            format!("{:04x}{:04x}", *t, *t.add(1))
        } else {
            "040904b0".into()
        };
        let q = wide(&format!("\\StringFileInfo\\{lang}\\FileDescription"));
        if !VerQueryValueW(buf.as_ptr() as _, PCWSTR(q.as_ptr()), &mut ptr, &mut len).as_bool() || len == 0 { return None }
        let s = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        Some(String::from_utf16_lossy(s).split('\0').next().unwrap_or("").to_string())
    }
}

/// The exe's 32x32 icon as a .ico data URI (alpha preserved).
fn icon_uri(path: &str) -> Option<String> {
    unsafe {
        let mut info = SHFILEINFOW::default();
        let p = wide(path);
        if SHGetFileInfoW(PCWSTR(p.as_ptr()), Default::default(), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON) == 0 {
            return None;
        }
        let hicon = info.hIcon;
        let mut ii = ICONINFO::default();
        let ok = GetIconInfo(hicon, &mut ii).is_ok();
        let _ = DestroyIcon(hicon);
        if !ok { return None }
        // The shell's "large" icon grows with DPI (48 px at 150%), so read the real size.
        let mut bm = BITMAP::default();
        GetObjectW(ii.hbmColor.into(), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _));
        let n = bm.bmWidth.clamp(16, 256);
        let pixels = bitmap_bgra(ii.hbmColor, n);
        let _ = DeleteObject(ii.hbmColor.into());
        let _ = DeleteObject(ii.hbmMask.into());
        Some(format!("data:image/x-icon;base64,{}", B64.encode(ico(&pixels?, n as u32))))
    }
}

/// Bottom-up BGRA pixels of an n x n bitmap.
unsafe fn bitmap_bgra(bmp: HBITMAP, n: i32) -> Option<Vec<u8>> {
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: n, biHeight: n, biPlanes: 1, biBitCount: 32, ..Default::default()
    };
    let mut px = vec![0u8; (n * n * 4) as usize];
    let dc = GetDC(None);
    let lines = GetDIBits(dc, bmp, 0, n as u32, Some(px.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if lines == 0 { return None }
    if px.chunks(4).all(|p| p[3] == 0) {
        px.chunks_mut(4).for_each(|p| p[3] = 255); // old icons without alpha
    }
    Some(px)
}

/// Wrap bottom-up BGRA pixels in a single-image .ico file.
fn ico(px: &[u8], n: u32) -> Vec<u8> {
    let mask = (n.div_ceil(32) * 4 * n) as usize; // 1bpp AND mask, all zero
    let img_len = 40 + px.len() + mask;
    let mut f = vec![0, 0, 1, 0, 1, 0]; // ICONDIR
    let dim = if n >= 256 { 0 } else { n as u8 }; // 0 means 256 in ICONDIRENTRY
    f.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
    f.extend_from_slice(&(img_len as u32).to_le_bytes());
    f.extend_from_slice(&22u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER, height doubled for XOR+AND
    f.extend_from_slice(&(n as i32).to_le_bytes());
    f.extend_from_slice(&(2 * n as i32).to_le_bytes());
    f.extend_from_slice(&[1, 0, 32, 0]);
    f.extend_from_slice(&[0; 24]);
    f.extend_from_slice(px);
    f.resize(22 + img_len, 0);
    f
}

#[cfg(test)]
mod tests {
    #[test]
    fn ico_layout() {
        let f = super::ico(&vec![0u8; 32 * 32 * 4], 32);
        assert_eq!(f.len(), 22 + 40 + 4096 + 128);
        assert_eq!(u32::from_le_bytes(f[14..18].try_into().unwrap()) as usize, f.len() - 22);
    }
}
