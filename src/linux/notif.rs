//! Notification alerts aren't on Linux yet (desktops keep their notifications to themselves).

pub struct Source {
    pub handler: String,
    pub latest: i64,
    pub count: i64,
}

pub fn sources() -> Result<Vec<Source>, String> {
    Err("notification alerts aren't on Linux yet".into())
}

/// Does a notification source belong to this alert? Same rule as on Windows.
pub fn matches(handler: &str, exe: &str, pattern: &str) -> bool {
    let h = handler.to_lowercase();
    let want = if pattern.trim().is_empty() { exe.trim_end_matches(".exe").to_lowercase() } else { pattern.trim().to_lowercase() };
    !want.is_empty() && h.contains(&want)
}
