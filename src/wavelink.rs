//! Elgato Wave Link 3 over its local JSON-RPC WebSocket, the connection its Stream Deck plugin uses.
//! Experimental: written from the protocol as other open-source clients use it (nvdweem/PCPanel,
//! DarrellVS/node-wave-link-sdk), not yet tried against Wave Link itself. Levels are 0-1.
use serde_json::{json, Value};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

/// After a failed connect, don't try again for this long (turning a knob sends many requests).
const RETRY_AFTER: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct WaveLink {
    ws: Option<WebSocket<TcpStream>>,
    next_id: u64,
    failed_at: Option<Instant>,
}

/// Wave Link writes its port to this file; 1884 is the usual one.
fn port() -> u16 {
    let path = std::env::var("LOCALAPPDATA").unwrap_or_default() + r"\Packages\Elgato.WaveLink_g54w8ztgkx496\LocalState\ws-info.json";
    std::fs::read_to_string(path).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["port"].as_u64())
        .map_or(1884, |p| p as u16)
}

impl WaveLink {
    /// Send one request and return its result.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if self.ws.is_none() {
            if self.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
                return Err("Wave Link isn't running (it needs Wave Link 3)".into());
            }
            match connect() {
                Ok(ws) => self.ws = Some(ws),
                Err(e) => {
                    self.failed_at = Some(Instant::now());
                    return Err(e);
                }
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        let ws = self.ws.as_mut().unwrap();
        let res = request(ws, id, method, params);
        if res.as_ref().is_err_and(|e| !e.starts_with("Wave Link:")) {
            self.ws = None; // transport error: reconnect next time
        }
        res
    }

    /// Set a level: a channel's fader (no mix), a channel's send into one mix, or a mix's master (no channel).
    pub fn set_level(&mut self, channel: &str, mix: &str, level: f32) -> Result<(), String> {
        let level = level.clamp(0.0, 1.0) as f64;
        match (channel.is_empty(), mix.is_empty()) {
            (true, true) => Err("pick a Wave Link channel or mix".into()),
            (true, false) => self.call("setMix", json!({"id": mix, "level": level})).map(drop),
            (false, true) => self.call("setChannel", json!({"id": channel, "level": level})).map(drop),
            (false, false) => self.call("setChannel", json!({"id": channel, "mixes": [{"id": mix, "level": level}]})).map(drop),
        }
    }

    /// Toggle mute on the same kinds of target. Returns whether it's muted now.
    pub fn toggle_mute(&mut self, channel: &str, mix: &str) -> Result<bool, String> {
        let muted = match (channel.is_empty(), mix.is_empty()) {
            (true, true) => return Err("pick a Wave Link channel or mix".into()),
            (true, false) => find(&self.call("getMixes", json!(null))?["mixes"], mix)?["isMuted"].as_bool().unwrap_or(false),
            (false, _) => {
                let channels = self.call("getChannels", json!(null))?;
                let ch = find(&channels["channels"], channel)?;
                let target = if mix.is_empty() { ch } else { find(&ch["mixes"], mix)? };
                target["isMuted"].as_bool().unwrap_or(false)
            }
        };
        let mute = !muted;
        match (channel.is_empty(), mix.is_empty()) {
            (true, _) => self.call("setMix", json!({"id": mix, "isMuted": mute}))?,
            (false, true) => self.call("setChannel", json!({"id": channel, "isMuted": mute}))?,
            (false, false) => self.call("setChannel", json!({"id": channel, "mixes": [{"id": mix, "isMuted": mute}]}))?,
        };
        Ok(mute)
    }

    /// Channels and mixes, for the settings window's pickers.
    pub fn targets(&mut self) -> Result<Value, String> {
        let channels = self.call("getChannels", json!(null))?["channels"].clone();
        let mixes = self.call("getMixes", json!(null))?["mixes"].clone();
        let pick = |list: &Value| -> Vec<Value> {
            list.as_array().into_iter().flatten().map(|x| json!({"id": x["id"], "name": x["name"]})).collect()
        };
        Ok(json!({"channels": pick(&channels), "mixes": pick(&mixes)}))
    }
}

fn find<'a>(list: &'a Value, id: &str) -> Result<&'a Value, String> {
    list.as_array().into_iter().flatten().find(|x| x["id"] == id).ok_or_else(|| format!("Wave Link: no channel or mix with id {id}"))
}

fn request(ws: &mut WebSocket<TcpStream>, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let mut msg = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if !params.is_null() {
        msg["params"] = params;
    }
    ws.send(Message::text(msg.to_string())).map_err(|e| e.to_string())?;
    loop {
        // Wave Link also pushes change events; skip them until our reply arrives.
        let reply: Value = match ws.read().map_err(|e| e.to_string())? {
            Message::Text(t) => serde_json::from_str(&t).map_err(|e| e.to_string())?,
            Message::Close(_) => return Err("Wave Link closed the connection".into()),
            _ => continue,
        };
        if reply["id"] == id {
            return match reply.get("error") {
                Some(e) if !e.is_null() => Err(format!("Wave Link: {}", e["message"].as_str().unwrap_or("request failed"))),
                _ => Ok(reply["result"].clone()),
            };
        }
    }
}

fn connect() -> Result<WebSocket<TcpStream>, String> {
    // Wave Link 2 listens on 1824: try it too, so its users hear why it doesn't work.
    let (port, tcp) = [port(), 1824].into_iter()
        .find_map(|p| TcpStream::connect_timeout(&([127, 0, 0, 1], p).into(), Duration::from_millis(300)).ok().map(|t| (p, t)))
        .ok_or("Wave Link isn't running (it needs Wave Link 3)")?;
    tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut req = format!("ws://127.0.0.1:{port}").into_client_request().map_err(|e| e.to_string())?;
    // Wave Link only talks to the Stream Deck.
    req.headers_mut().insert("Origin", "streamdeck://".parse().unwrap());
    let (mut ws, _) = tungstenite::client(req, tcp).map_err(|e| e.to_string())?;
    let info = request(&mut ws, 0, "getApplicationInfo", json!(null))?;
    if !info["appID"].as_str().unwrap_or("").eq_ignore_ascii_case("ewl") {
        return Err("this Wave Link version isn't supported yet; it needs Wave Link 3".into());
    }
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A stand-in Wave Link 3: answers like the real one and records what it was asked to set.
    fn fake_wave_link(app_id: &'static str) -> (u16, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sets = Arc::new(Mutex::new(vec![]));
        let seen = sets.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let mut ws = tungstenite::accept(stream).unwrap();
                    // An unrelated event first, as Wave Link pushes them at any time.
                    let _ = ws.send(Message::text(r#"{"jsonrpc":"2.0","method":"levelMeterChanged","params":{}}"#));
                    while let Ok(Message::Text(t)) = ws.read() {
                        let req: Value = serde_json::from_str(&t).unwrap();
                        let result = match req["method"].as_str().unwrap() {
                            "getApplicationInfo" => json!({"appID": app_id, "interfaceRevision": 1}),
                            "getChannels" => json!({"channels": [{"id": "music", "name": "Music", "isMuted": false, "level": 0.5,
                                "mixes": [{"id": "stream", "isMuted": true, "level": 0.2}]}]}),
                            "getMixes" => json!({"mixes": [{"id": "stream", "name": "Stream Mix", "isMuted": false, "level": 1.0}]}),
                            _ => {
                                seen.lock().unwrap().push(json!({"method": req["method"], "params": req["params"]}));
                                json!({})
                            }
                        };
                        let _ = ws.send(Message::text(json!({"jsonrpc": "2.0", "id": req["id"], "result": result}).to_string()));
                    }
                });
            }
        });
        (port, sets)
    }

    fn point_at(port: u16) {
        let dir = std::env::temp_dir().join(format!("pcpanel-wl-test-{port}"));
        let info = dir.join(r"Packages\Elgato.WaveLink_g54w8ztgkx496\LocalState");
        std::fs::create_dir_all(&info).unwrap();
        std::fs::write(info.join("ws-info.json"), format!(r#"{{"port": {port}}}"#)).unwrap();
        std::env::set_var("LOCALAPPDATA", &dir);
    }

    #[test]
    fn talks_to_wave_link_3() {
        let (port, sets) = fake_wave_link("EWL");
        point_at(port);
        let mut wl = WaveLink::default();
        wl.set_level("music", "", 0.25).unwrap();
        wl.set_level("music", "stream", 2.0).unwrap(); // clamped
        wl.set_level("", "stream", 0.5).unwrap();
        assert!(wl.toggle_mute("music", "").unwrap(), "unmuted channel gets muted");
        assert!(!wl.toggle_mute("music", "stream").unwrap(), "its muted stream send gets unmuted");
        assert!(wl.toggle_mute("", "stream").unwrap());
        let t = wl.targets().unwrap();
        assert_eq!(t["channels"][0]["name"], "Music");
        assert_eq!(t["mixes"][0]["name"], "Stream Mix");
        let sets = sets.lock().unwrap().clone();
        assert_eq!(sets, vec![
            json!({"method": "setChannel", "params": {"id": "music", "level": 0.25}}),
            json!({"method": "setChannel", "params": {"id": "music", "mixes": [{"id": "stream", "level": 1.0}]}}),
            json!({"method": "setMix", "params": {"id": "stream", "level": 0.5}}),
            json!({"method": "setChannel", "params": {"id": "music", "isMuted": true}}),
            json!({"method": "setChannel", "params": {"id": "music", "mixes": [{"id": "stream", "isMuted": false}]}}),
            json!({"method": "setMix", "params": {"id": "stream", "isMuted": true}}),
        ]);
        // An older Wave Link (2.x says "egwl") is refused with a clear message.
        let (old, _) = fake_wave_link("egwl");
        point_at(old);
        let err = WaveLink::default().set_level("music", "", 0.5).unwrap_err();
        assert!(err.contains("needs Wave Link 3"), "{err}");
    }
}

/// Needs a running Wave Link: `cargo test wavelink_live -- --ignored --nocapture`.
#[cfg(test)]
#[test]
#[ignore]
fn wavelink_live() {
    let mut wl = WaveLink::default();
    println!("{:?}", wl.targets());
}
