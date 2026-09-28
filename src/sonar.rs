//! SteelSeries Sonar (part of SteelSeries GG) over its local HTTP API, the one its own apps use.
//! GG writes its address to coreProps.json; GG's /subApps names Sonar's web server, which moves each
//! time GG starts. Experimental: written from other open-source clients (nvdweem/PCPanel,
//! steelseries-sonar-py), not yet tried against Sonar itself. Levels are 0-1.
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::time::{Duration, Instant};

/// The channels Sonar has, as its API names them, and what GG calls them.
pub const CHANNELS: [(&str, &str); 6] =
    [("master", "Master"), ("game", "Game"), ("chatRender", "Chat"), ("media", "Media"), ("aux", "Aux"), ("chatCapture", "Mic")];

/// After a failed lookup, don't try again for this long (turning a knob sends many requests).
const RETRY_AFTER: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct Sonar {
    /// Sonar's web server, like http://127.0.0.1:61234.
    base: Option<String>,
    failed_at: Option<Instant>,
}

/// Find Sonar's web server through GG.
fn discover() -> Result<String, String> {
    const NOT_RUNNING: &str = "SteelSeries Sonar isn't running (open SteelSeries GG)";
    let path = std::env::var("PROGRAMDATA").unwrap_or_else(|_| r"C:\ProgramData".into()) + r"\SteelSeries\SteelSeries Engine 3\coreProps.json";
    let props: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).ok_or(NOT_RUNNING)?;
    let gg = props["ggEncryptedAddress"].as_str().unwrap_or_default();
    // Both addresses must be on this PC, whatever the files and replies say.
    if !local(gg) {
        return Err(NOT_RUNNING.into());
    }
    // GG answers over HTTPS with its own self-signed certificate, hence -k (only for this local hop).
    let mut curl = std::process::Command::new("curl");
    curl.args(["-sSfk", "--max-time", "3", &format!("https://{gg}/subApps")]);
    #[cfg(windows)]
    curl.creation_flags(crate::sys::NO_WINDOW);
    let out = curl.output().map_err(|e| format!("curl: {e}"))?;
    let apps: Value = serde_json::from_slice(&out.stdout).map_err(|_| NOT_RUNNING)?;
    let base = apps["subApps"]["sonar"]["metadata"]["webServerAddress"].as_str().unwrap_or_default().trim_end_matches('/');
    match base.strip_prefix("http://") {
        Some(host) if local(host) => Ok(base.to_string()),
        _ => Err(NOT_RUNNING.into()),
    }
}

fn local(host_port: &str) -> bool {
    ["127.0.0.1:", "localhost:"].iter().any(|p| host_port.starts_with(p))
}

impl Sonar {
    /// One request to Sonar; returns the body. Looks Sonar up again if it stopped answering.
    fn call(&mut self, method: &str, path: &str) -> Result<String, String> {
        if self.base.is_none() {
            if self.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
                return Err("SteelSeries Sonar isn't running (open SteelSeries GG)".into());
            }
            match discover() {
                Ok(b) => self.base = Some(b),
                Err(e) => {
                    self.failed_at = Some(Instant::now());
                    return Err(e);
                }
            }
        }
        let res = request(self.base.as_deref().unwrap(), method, path);
        if res.as_ref().is_err_and(|e| !e.starts_with("Sonar:")) {
            self.base = None; // GG restarted on a new port: look again next time
        }
        res
    }

    fn streamer(&mut self) -> Result<bool, String> {
        let mode = self.call("GET", "/mode")?;
        Ok(mode.trim().trim_matches('"') == "stream")
    }

    /// The route for a channel: classic mode has no mixes; streamer mode has monitoring and streaming.
    fn routes(&mut self, channel: &str, mix: &str) -> Result<Vec<String>, String> {
        if !CHANNELS.iter().any(|(id, _)| *id == channel) {
            return Err("pick a Sonar channel".into());
        }
        if !self.streamer()? {
            return Ok(vec![format!("/volumeSettings/classic/{channel}")]);
        }
        let mixes: &[&str] = match mix { "streaming" => &["streaming"], "both" => &["monitoring", "streaming"], _ => &["monitoring"] };
        Ok(mixes.iter().map(|m| format!("/volumeSettings/streamer/{m}/{channel}")).collect())
    }

    pub fn set_level(&mut self, channel: &str, mix: &str, level: f32) -> Result<(), String> {
        for route in self.routes(channel, mix)? {
            self.call("PUT", &format!("{route}/Volume/{:.4}", level.clamp(0.0, 1.0)))?;
        }
        Ok(())
    }

    /// Flip mute (going by the first mix when there are two) and return the new state.
    pub fn toggle_mute(&mut self, channel: &str, mix: &str) -> Result<bool, String> {
        let routes = self.routes(channel, mix)?;
        let classic = routes[0].contains("/classic/");
        let state: Value = serde_json::from_str(&self.call("GET", if classic { "/volumeSettings/classic" } else { "/volumeSettings/streamer" })?)
            .map_err(|_| "Sonar: unexpected reply")?;
        // The master sits under "masters", the rest under "devices".
        let node = if channel == "master" { &state["masters"] } else { &state["devices"][channel] };
        let node = if classic { &node["classic"] } else { &node["stream"][if mix == "streaming" { "streaming" } else { "monitoring" }] };
        let muted = !node["muted"].as_bool().unwrap_or(false);
        for route in routes {
            // Sonar spells it differently per mode; the other spelling answers 404.
            self.call("PUT", &format!("{route}/{}/{muted}", if classic { "Mute" } else { "isMuted" }))?;
        }
        Ok(muted)
    }
}

/// A minimal HTTP/1.1 request to Sonar's local server.
fn request(base: &str, method: &str, path: &str) -> Result<String, String> {
    let host = base.trim_start_matches("http://");
    let addr = host.replace("localhost", "127.0.0.1").parse().map_err(|_| format!("bad Sonar address {base}"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
    // One write: a server that reads the request in one go would otherwise see only part of it.
    let req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = vec![];
    // Some servers reset the connection right after replying; what arrived is still the reply.
    if let Err(e) = s.read_to_end(&mut raw) {
        if raw.is_empty() {
            return Err(e.to_string());
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no reply from Sonar")?;
    let status = head.split_whitespace().nth(1).unwrap_or("");
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { unchunk(body) } else { body.to_string() };
    if !status.starts_with('2') {
        return Err(format!("Sonar: {method} {path} answered {status} {}", body.trim()));
    }
    Ok(body)
}

fn unchunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = s.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        s = rest[n..].trim_start_matches("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A pretend Sonar in streamer mode: answers /mode and the volume settings, records every request line.
    fn fake_sonar(replies: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let t = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..replies {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = c.read(&mut buf).unwrap();
                let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap().to_string();
                let body = if line.contains("/mode") {
                    "\"stream\"".to_string()
                } else if line.starts_with("GET /volumeSettings/streamer") {
                    r#"{"masters":{"stream":{"monitoring":{"volume":1,"muted":false}}},"devices":{"game":{"stream":{"monitoring":{"volume":0.5,"muted":true},"streaming":{"volume":0.2,"muted":false}}}}}"#.to_string()
                } else {
                    String::new()
                };
                // Chunked, the way some servers send JSON.
                write!(c, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).unwrap();
                seen.push(line);
            }
            seen
        });
        (base, t)
    }

    #[test]
    fn talks_to_sonar_in_streamer_mode() {
        let (base, server) = fake_sonar(6);
        let mut s = Sonar { base: Some(base), failed_at: None };
        s.set_level("game", "both", 0.25).unwrap();
        // Game is muted in the personal mix, so toggling unmutes it (in the personal mix only).
        assert!(!s.toggle_mute("game", "monitoring").unwrap());
        assert!(s.set_level("nope", "", 0.5).is_err());
        let seen = server.join().unwrap();
        assert_eq!(seen, [
            "GET /mode HTTP/1.1",
            "PUT /volumeSettings/streamer/monitoring/game/Volume/0.2500 HTTP/1.1",
            "PUT /volumeSettings/streamer/streaming/game/Volume/0.2500 HTTP/1.1",
            "GET /mode HTTP/1.1",
            "GET /volumeSettings/streamer HTTP/1.1",
            "PUT /volumeSettings/streamer/monitoring/game/isMuted/false HTTP/1.1",
        ]);
    }

    #[test]
    #[ignore] // needs SteelSeries GG with Sonar running
    fn sonar_live() {
        let mut s = Sonar::default();
        println!("{:?}", s.call("GET", "/mode"));
    }
}
