//! Serves the settings UI (shown in the settings window's WebView2) on 127.0.0.1 only.
use crate::{apps, audio, config, log, Msg, Shared, PORT};
use serde_json::json;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Method, Request, Response, Server};

const UI: &str = include_str!("ui.html");

pub fn spawn(tx: Sender<Msg>, shared: Arc<Mutex<Shared>>) {
    std::thread::spawn(move || {
        let server = match Server::http(("127.0.0.1", PORT)) {
            Ok(s) => s,
            Err(e) => return log(&shared, format!("settings server: {e}")),
        };
        audio::com_init();
        let audio = audio::Audio::new().ok();
        let mut cache = apps::Cache::default();
        for req in server.incoming_requests() {
            if let Err(e) = handle(req, &tx, &shared, audio.as_ref(), &mut cache) {
                log(&shared, format!("settings server: {e}"));
            }
        }
    });
}

fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.to_string())
}

fn json_response(v: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(v.to_string()).with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

fn handle(mut req: Request, tx: &Sender<Msg>, shared: &Mutex<Shared>, audio: Option<&audio::Audio>, cache: &mut apps::Cache) -> std::io::Result<()> {
    // Block DNS rebinding and cross-site writes: the config can run commands and holds the OBS password.
    let host_ok = header(&req, "Host").is_some_and(|h| h == format!("127.0.0.1:{PORT}") || h == format!("localhost:{PORT}"));
    if !host_ok {
        return req.respond(Response::from_string("forbidden").with_status_code(403));
    }
    let url = req.url().split('?').next().unwrap_or("").to_string();
    match (req.method(), url.as_str()) {
        (Method::Get, "/") => req.respond(
            Response::from_string(UI).with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap()),
        ),
        (Method::Get, "/api/state") => {
            let s = crate::lock(&shared);
            let body = json!({
                "config": s.config, "values": s.values, "connected": s.connected, "muted": s.muted,
                "levels": s.levels, "present": s.present, "alerts_on": s.alerts_on, "model": s.model.id(), "model_name": s.model.name(), "buttons": s.buttons, "peaks": s.peaks, "official_running": s.official_running, "lights": {"gen": s.lights_gen, "written": s.lights_written, "reports": s.lights.iter().map(|r| r[..9].to_vec()).collect::<Vec<_>>()},
                "log": s.log, "autostart": crate::sys::autostart_enabled(),
            });
            drop(s);
            req.respond(json_response(body))
        }
        (Method::Get, "/api/apps") => req.respond(json_response(json!(cache.list(audio)))),
        (Method::Get, "/api/devices") => {
            let devices: Vec<_> = audio.map(|a| a.devices()).unwrap_or_default().into_iter()
                .map(|d| json!({"name": d.name, "capture": d.capture})).collect();
            req.respond(json_response(json!(devices)))
        }
        (Method::Post, "/api/selftest") if header(&req, "X-PCP").is_some() => {
            // Body: "on", "off", or the LED to light ("0".."9", "none").
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            let _ = tx.send(match body.trim() {
                "on" => Msg::TestMode(true),
                "off" => Msg::TestMode(false),
                led => Msg::TestLight(led.parse().ok().filter(|&n: &usize| n <= crate::config::CONTROLS)),
            });
            req.respond(json_response(json!({"ok": true})))
        }
        (Method::Post, "/api/close-official") if header(&req, "X-PCP").is_some() => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            match crate::sys::close_official_app(body.trim() == "stop-autostart") {
                Ok(n) => {
                    crate::lock(shared).official_running = false;
                    log(shared, format!("closed the official PCPanel software ({n} processes){}", if body.trim() == "stop-autostart" { " and stopped it starting with Windows" } else { "" }));
                    req.respond(json_response(json!({"closed": n})))
                }
                Err(e) => req.respond(Response::from_string(e).with_status_code(500)),
            }
        }
        (Method::Post, "/api/open-log") if header(&req, "X-PCP").is_some() => {
            let _ = crate::sys::open(&crate::log_path().to_string_lossy());
            req.respond(json_response(json!({"ok": true})))
        }
        (Method::Post, "/api/alert-preview") if header(&req, "X-PCP").is_some() => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            let _ = tx.send(Msg::PreviewAlert(body.trim().parse().unwrap_or(usize::MAX)));
            req.respond(json_response(json!({"ok": true})))
        }
        (Method::Get, "/api/displays") => {
            let list: Vec<_> = crate::sys::displays().into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect();
            req.respond(json_response(json!(list)))
        }
        (Method::Get, "/api/pick") => {
            // Wait (off this thread) for the user to focus another app's window.
            std::thread::spawn(move || {
                let start = Instant::now();
                let mut exe = String::new();
                while start.elapsed() < Duration::from_secs(8) {
                    std::thread::sleep(Duration::from_millis(100));
                    let fg = audio::foreground_exe();
                    if !fg.is_empty() && fg != "pcpanel-revive.exe" && fg != "msedgewebview2.exe" {
                        exe = fg;
                        break;
                    }
                }
                let _ = req.respond(json_response(json!({"exe": exe})));
            });
            Ok(())
        }
        (Method::Get, "/api/stock") => {
            let path = crate::import::stock_path();
            req.respond(json_response(json!({"found": path.is_file(), "path": path})))
        }
        // Body = contents of a save.json the user picked; empty body = the stock app's own save.
        (Method::Post, "/api/import") if header(&req, "X-PCP").is_some() => {
            let path = crate::import::stock_path();
            let mut text = String::new();
            req.as_reader().read_to_string(&mut text)?;
            if text.trim().is_empty() {
                match std::fs::read_to_string(&path) {
                    Ok(t) => text = t,
                    Err(_) => return req.respond(Response::from_string(format!("No PCPanel save found at {}", path.display())).with_status_code(404)),
                }
            }
            let devices = audio.map(|a| a.devices()).unwrap_or_default();
            let name = |id: &str| devices.iter().find(|d| d.id.eq_ignore_ascii_case(id)).map(|d| d.name.clone());
            match crate::import::convert(&text, &name) {
                Ok(r) => req.respond(json_response(json!({"profiles": r.profiles, "active": r.active, "notes": r.notes, "path": path}))),
                Err(e) => req.respond(Response::from_string(e).with_status_code(400)),
            }
        }
        (Method::Post, "/api/test") if header(&req, "X-PCP").is_some() => {
            #[derive(serde::Deserialize)]
            struct Test { control: usize, action: config::Action }
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            match serde_json::from_str::<Test>(&body) {
                Ok(t) => {
                    let _ = tx.send(Msg::Test(t.control, t.action));
                    req.respond(json_response(json!({"ok": true})))
                }
                Err(e) => req.respond(Response::from_string(e.to_string()).with_status_code(400)),
            }
        }
        (Method::Get, "/api/backups") => {
            let list: Vec<_> = config::backups().into_iter().map(|(name, secs)| json!({"name": name, "time": secs})).collect();
            req.respond(json_response(json!(list)))
        }
        (Method::Post, "/api/restore") if header(&req, "X-PCP").is_some() => {
            let mut name = String::new();
            req.as_reader().read_to_string(&mut name)?;
            // Only names from our own listing; no paths.
            let Some(found) = config::backups().into_iter().find(|(n, _)| *n == name.trim()) else {
                return req.respond(Response::from_string("backup not found").with_status_code(404));
            };
            let text = std::fs::read_to_string(config::backup_dir().join(&found.0))?;
            match serde_json::from_str::<config::Config>(&text) {
                Ok(mut cfg) => {
                    cfg.normalize();
                    config::backup();
                    match config::save(&cfg) {
                        Ok(()) => {
                            let _ = tx.send(Msg::Reload);
                            req.respond(json_response(json!({"ok": true})))
                        }
                        Err(e) => req.respond(Response::from_string(e).with_status_code(500)),
                    }
                }
                Err(e) => req.respond(Response::from_string(format!("backup is unreadable: {e}")).with_status_code(400)),
            }
        }
        (Method::Post, "/api/config") if header(&req, "X-PCP").is_some() => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            match serde_json::from_str::<config::Config>(&body) {
                Ok(mut cfg) => {
                    cfg.normalize();
                    config::backup_every(600);
                    match config::save(&cfg) {
                        Ok(()) => {
                            let _ = tx.send(Msg::Reload);
                            req.respond(json_response(json!({"ok": true})))
                        }
                        Err(e) => req.respond(Response::from_string(e).with_status_code(500)),
                    }
                }
                Err(e) => req.respond(Response::from_string(e.to_string()).with_status_code(400)),
            }
        }
        (Method::Post, "/api/autostart") if header(&req, "X-PCP").is_some() => {
            let mut body = String::new();
            req.as_reader().read_to_string(&mut body)?;
            crate::sys::set_autostart(body.trim() == "1");
            req.respond(json_response(json!({"autostart": crate::sys::autostart_enabled()})))
        }
        _ => req.respond(Response::from_string("not found").with_status_code(404)),
    }
}
