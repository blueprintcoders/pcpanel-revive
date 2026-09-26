//! Minimal OBS WebSocket v5 client: lazily connects, reconnects after a failure.
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

#[derive(Default)]
pub struct Obs {
    ws: Option<WebSocket<TcpStream>>,
    next_id: u64,
}

impl Obs {
    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    pub fn request(&mut self, cfg: &crate::config::Obs, kind: &str, data: Value) -> Result<(), String> {
        if self.ws.is_none() {
            self.ws = Some(connect(cfg)?);
        }
        self.next_id += 1;
        let id = self.next_id.to_string();
        let ws = self.ws.as_mut().unwrap();
        let res = send(ws, json!({"op": 6, "d": {"requestType": kind, "requestId": id, "requestData": data}}))
            .and_then(|_| loop {
                let m = recv(ws)?;
                if m["op"] == 7 && m["d"]["requestId"] == id.as_str() {
                    let st = &m["d"]["requestStatus"];
                    break if st["result"] == true { Ok(()) } else { Err(format!("OBS {kind}: {}", st["comment"])) };
                }
            });
        if res.as_ref().is_err_and(|e| !e.starts_with("OBS ")) {
            self.ws = None; // transport error: reconnect next time
        }
        res
    }
}

fn send(ws: &mut WebSocket<TcpStream>, v: Value) -> Result<(), String> {
    ws.send(Message::text(v.to_string())).map_err(|e| e.to_string())
}

fn recv(ws: &mut WebSocket<TcpStream>) -> Result<Value, String> {
    loop {
        match ws.read().map_err(|e| e.to_string())? {
            Message::Text(t) => return serde_json::from_str(&t).map_err(|e| e.to_string()),
            Message::Close(_) => return Err("OBS closed the connection".into()),
            _ => {}
        }
    }
}

fn connect(cfg: &crate::config::Obs) -> Result<WebSocket<TcpStream>, String> {
    let url = cfg.url.trim();
    let host = url.trim_start_matches("ws://").split('/').next().unwrap_or("");
    let addr = host.to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("bad OBS url")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(|e| format!("OBS not reachable: {e}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let (mut ws, _) = tungstenite::client(url, tcp).map_err(|e| e.to_string())?;
    let hello = recv(&mut ws)?;
    let mut identify = json!({"rpcVersion": 1, "eventSubscriptions": 0});
    if let Some(auth) = hello["d"]["authentication"].as_object() {
        let (salt, challenge) = (auth["salt"].as_str().unwrap_or(""), auth["challenge"].as_str().unwrap_or(""));
        identify["authentication"] = auth_string(&cfg.password, salt, challenge).into();
    }
    send(&mut ws, json!({"op": 1, "d": identify}))?;
    match recv(&mut ws)?["op"].as_u64() {
        Some(2) => Ok(ws),
        _ => Err("OBS authentication failed".into()),
    }
}

fn auth_string(password: &str, salt: &str, challenge: &str) -> String {
    let secret = B64.encode(Sha256::digest(format!("{password}{salt}")));
    B64.encode(Sha256::digest(format!("{secret}{challenge}")))
}

