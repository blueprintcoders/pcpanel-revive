//! Updates from GitHub Releases. The repository is set at build time (CI sets PCPANEL_REPO), so a local
//! build never updates itself.
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const REPO: Option<&str> = option_env!("PCPANEL_REPO");
/// The release file for this platform.
const EXE: &str = if cfg!(windows) { "pcpanel-revive.exe" } else { "pcpanel-revive-linux-x86_64" };

#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub version: String,
    pub page: String,
    exe_url: String,
    sha_url: String,
}

fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// Is `latest` ("v1.2.0") a newer version than `current` ("1.1.9")?
pub fn newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(a), Some(b)) if a > b)
}

/// The latest release, when it's newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let repo = REPO.ok_or("this copy was built on this PC, so it doesn't update itself")?;
    let body = crate::sys::http_get(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let v: Value = serde_json::from_str(&body).map_err(|e| format!("unexpected reply from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or("");
    if !newer(tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let asset = |name: &str| {
        v["assets"].as_array().into_iter().flatten()
            .find(|a| a["name"] == name)
            .and_then(|a| a["browser_download_url"].as_str())
            .map(String::from)
    };
    Ok(Some(Release {
        version: tag.trim_start_matches('v').into(),
        page: v["html_url"].as_str().unwrap_or("").into(),
        exe_url: asset(EXE).ok_or(format!("the new release has no {EXE}"))?,
        sha_url: asset(&format!("{EXE}.sha256")).ok_or("the new release has no checksum")?,
    }))
}

/// Download the new version, check it against its published checksum, put it in place of this exe,
/// and start it. The caller quits right after.
pub fn install(r: &Release) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = exe.with_extension("exe.new");
    crate::sys::download(&r.exe_url, &new)?;
    let want = crate::sys::http_get(&r.sha_url)?.split_whitespace().next().unwrap_or("").to_lowercase();
    let got = format!("{:x}", Sha256::digest(std::fs::read(&new).map_err(|e| e.to_string())?));
    if want != got {
        let _ = std::fs::remove_file(&new);
        return Err("the download didn't match its checksum, so it wasn't installed".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    // Windows lets a running exe be renamed, just not replaced.
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    let cant = |e: std::io::Error| format!("couldn't replace {} ({e}). Download the new version from the release page instead.", exe.display());
    std::fs::rename(&exe, &old).map_err(cant)?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(cant(e));
    }
    std::process::Command::new(&exe).arg("--updated").spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Remove what the last update left behind.
/// Right after an update the old version may still be exiting, so keep trying for a little while.
pub fn cleanup() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = exe.with_extension("exe.old");
    std::thread::spawn(move || {
        for _ in 0..40 {
            if !old.exists() || std::fs::remove_file(&old).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::newer("v0.2.0", "0.1.9"));
        assert!(super::newer("v1.0.0", "0.9.12"));
        assert!(!super::newer("v0.1.0", "0.1.0"));
        assert!(!super::newer("v0.1.0", "0.2.0"));
        assert!(!super::newer("nightly", "0.1.0"));
    }
}
