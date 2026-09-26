//! Windows toast notifications, read from the notification center's own database
//! (%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db) with the SQLite built into Windows.
//! The official notification-listener API only works for Store apps; this works for any app.
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;
use windows::core::{s, w};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// One app's toasts currently in the notification center.
pub struct Source {
    /// The app's notification ID, e.g. "com.squirrel.Discord.Discord".
    pub handler: String,
    /// Newest arrival time (Windows FILETIME ticks).
    pub latest: i64,
    pub count: i64,
}

type Open = unsafe extern "system" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int;
type Prepare = unsafe extern "system" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int;
type Step = unsafe extern "system" fn(*mut c_void) -> c_int;
type Text = unsafe extern "system" fn(*mut c_void, c_int) -> *const u8;
type Int64 = unsafe extern "system" fn(*mut c_void, c_int) -> i64;
type Close = unsafe extern "system" fn(*mut c_void) -> c_int;

struct Sqlite { open: Open, prepare: Prepare, step: Step, text: Text, int64: Int64, finalize: Close, close: Close }

fn sqlite() -> Option<&'static Sqlite> {
    static LIB: OnceLock<Option<Sqlite>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let lib = LoadLibraryW(w!("winsqlite3.dll")).ok()?;
        macro_rules! f { ($name:expr) => { std::mem::transmute(GetProcAddress(lib, $name)?) } }
        Some(Sqlite {
            open: f!(s!("sqlite3_open_v2")), prepare: f!(s!("sqlite3_prepare_v2")), step: f!(s!("sqlite3_step")),
            text: f!(s!("sqlite3_column_text")), int64: f!(s!("sqlite3_column_int64")),
            finalize: f!(s!("sqlite3_finalize")), close: f!(s!("sqlite3_close")),
        })
    }).as_ref()
}

/// Toasts per app currently in the notification center.
pub fn sources() -> Result<Vec<Source>, String> {
    const READONLY: c_int = 1;
    const ROW: c_int = 100;
    let lib = sqlite().ok_or("Windows' SQLite (winsqlite3.dll) isn't available")?;
    let base = std::env::var("LOCALAPPDATA").map_err(|e| e.to_string())?;
    let path = CString::new(format!("{base}\\Microsoft\\Windows\\Notifications\\wpndatabase.db")).map_err(|e| e.to_string())?;
    let sql = c"select h.PrimaryId, max(n.ArrivalTime), count(*) from Notification n join NotificationHandler h on h.RecordId = n.HandlerId where n.Type = 'toast' group by h.PrimaryId";
    let mut out = vec![];
    unsafe {
        let mut db = std::ptr::null_mut();
        if (lib.open)(path.as_ptr(), &mut db, READONLY, std::ptr::null()) != 0 {
            (lib.close)(db);
            return Err("couldn't open the notification database".into());
        }
        let mut stmt = std::ptr::null_mut();
        if (lib.prepare)(db, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) == 0 {
            while (lib.step)(stmt) == ROW {
                let t = (lib.text)(stmt, 0);
                if t.is_null() {
                    continue;
                }
                out.push(Source {
                    handler: CStr::from_ptr(t as *const c_char).to_string_lossy().into_owned(),
                    latest: (lib.int64)(stmt, 1),
                    count: (lib.int64)(stmt, 2),
                });
            }
            (lib.finalize)(stmt);
        }
        (lib.close)(db);
    }
    Ok(out)
}

/// Does a notification source belong to this alert? `pattern` wins; otherwise the exe's name
/// ("discord.exe" matches "com.squirrel.Discord.Discord").
pub fn matches(handler: &str, exe: &str, pattern: &str) -> bool {
    let h = handler.to_lowercase();
    let want = if pattern.trim().is_empty() { exe.trim_end_matches(".exe").to_lowercase() } else { pattern.trim().to_lowercase() };
    !want.is_empty() && h.contains(&want)
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_sources() {
        assert!(super::matches("com.squirrel.slack.slack", "slack.exe", ""));
        assert!(super::matches("com.squirrel.Discord.Discord", "discord.exe", ""));
        assert!(!super::matches("Chrome", "slack.exe", ""));
        assert!(super::matches("MSTeams_8wekyb3d8bbwe!MSTeams", "ms-teams.exe", "msteams"));
    }
}
