//! Local transport between the window and the Rust main process. Serves Powder's original interface
//! files, Powder's original preload.js (running on the Electron shim), and the IPC endpoints.
//! Bound to 127.0.0.1 only; IPC calls need the per-run random token.

use crate::dist::Dist;
use crate::host::{Host, Reply};
use serde_json::Value;
use std::sync::Arc;
use crate::http::Request;

const SHIM: &str = include_str!("../assets/shim.js");
const ADDGAME: &str = include_str!("../assets/addgame.js");

pub struct Ctx {
    pub host: Arc<Host>,
    pub dist: Arc<Dist>,
    pub token: String,
}

pub fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i ^ std::process::id() as u64);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn bytes(req: Request, data: Vec<u8>, ctype: &str) {
    req.respond(200, ctype, &data);
}

fn status(req: Request, code: u16) {
    req.status(code);
}

pub fn start(ctx: Arc<Ctx>, port: u16) -> anyhow::Result<u16> {
    let log = std::env::var("POWDER_RS_HTTPLOG").is_ok();
    Ok(crate::http::serve(port, move |req| {
        if log {
            crate::log(&format!("http {} {}", req.method, req.url));
        }
        handle(&ctx, req)
    })?)
}

fn authorized(ctx: &Ctx, req: &Request) -> bool {
    req.header("x-pw-token") == Some(ctx.token.as_str()) || req.url.contains(&format!("t={}", ctx.token))
}

fn handle(ctx: &Ctx, req: Request) {
    let url = req.url.clone();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = percent_decode(path);

    if path == "/__pw/invoke" {
        if !authorized(ctx, &req) {
            return status(req, 403);
        }
        let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let channel = v["channel"].as_str().unwrap_or("").to_string();
        let args = v["args"].as_array().cloned().unwrap_or_default();
        return match ctx.host.invoke(&channel, args) {
            Reply::Json(j) => bytes(req, serde_json::to_vec(&j).unwrap(), "application/json"),
            // a 204 can leave stray bytes on a keep-alive connection; send a marker the shim maps to undefined
            Reply::Undefined => bytes(req, br#"{"__pwUndefined":1}"#.to_vec(), "application/json"),
        };
    }
    if path == "/__pw/events" {
        if !authorized(ctx, &req) {
            return status(req, 403);
        }
        let since = query.split('&').find_map(|kv| kv.strip_prefix("since=")).and_then(|s| s.parse().ok()).unwrap_or(0);
        let evs = ctx.host.events_since(since);
        return bytes(req, serde_json::to_vec(&evs).unwrap(), "application/json");
    }
    if path == "/__pw/preload.js" {
        let pre = ctx.dist.read("dist/preload/preload.js").unwrap_or_default();
        let mut js = SHIM.replace("__PW_TOKEN__", &ctx.token).into_bytes();
        js.extend_from_slice(b"\n;\n");
        js.extend_from_slice(&pre);
        return bytes(req, js, "text/javascript");
    }
    if path == "/__pw/addgame.js" {
        return bytes(req, ADDGAME.replace("__PW_TOKEN__", &ctx.token).into_bytes(), "text/javascript");
    }
    if let Some(rest) = path.strip_prefix("/__pw/x/") {
        if !authorized(ctx, &req) {
            return status(req, 403);
        }
        let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let out = match (rest, req.method.as_str()) {
            ("pick-video", _) => serde_json::json!({"path": crate::dialogs::pick("video")}),
            ("addgame", "POST") => crate::features::customgame::start(v["title"].as_str().unwrap_or(""), v["video"].as_str().unwrap_or(""), v["addTo"].as_str(), v["model"] == true),
            ("addgame", _) => crate::features::customgame::status_json(),
            ("share-game", "POST") => crate::features::community::share(v["id"].as_str().unwrap_or("")),
            ("export-log", "POST") => {
                let (gid, log) = crate::features::customgame::log_snapshot();
                let id = v["id"].as_str().map(String::from).or(gid);
                crate::features::gameedit::export_log(id.as_deref(), &log)
            }
            ("edit-list", _) => crate::features::gameedit::list(),
            ("edit-load", "POST") => crate::features::gameedit::load(v["id"].as_str().unwrap_or("")),
            ("edit-save", "POST") => crate::features::gameedit::save(v["id"].as_str().unwrap_or(""), &v["rows"]),
            ("edit-reset", "POST") => crate::features::gameedit::reset(v["id"].as_str().unwrap_or("")),
            ("datasets", "GET") => serde_json::json!({"path": crate::features::gameedit::datasets_dir().display().to_string()}),
            ("datasets-open", "POST") => {
                let d = crate::features::gameedit::datasets_dir();
                let _ = std::fs::create_dir_all(&d);
                crate::features::core::open_path(&d.display().to_string());
                serde_json::json!({"ok": true})
            }
            ("edit-open", "POST") => crate::features::gameedit::open_folder(v["id"].as_str().unwrap_or("")),
            _ => return status(req, 404),
        };
        return bytes(req, serde_json::to_vec(&out).unwrap(), "application/json");
    }
    if let Some(rest) = path.strip_prefix("/__pw/proto/") {
        if !authorized(ctx, &req) {
            return status(req, 403);
        }
        return crate::protocols::serve(ctx, req, rest);
    }
    if path == "/" || path == "/index.html" {
        let html = String::from_utf8_lossy(&ctx.dist.read("dist/renderer/index.html").unwrap_or_default()).to_string();
        let html = html.replacen("<head>", "<head><script src=\"/__pw/preload.js\"></script><script src=\"/__pw/addgame.js\"></script>", 1);
        return bytes(req, html.into_bytes(), "text/html; charset=utf-8");
    }
    if let Some(rest) = path.strip_prefix("/splash/") {
        let p = if rest.is_empty() { "index.html" } else { rest };
        return match ctx.dist.read(&format!("dist/splash/{p}")) {
            Some(d) => bytes(req, d, mime_guess::from_path(p).first_or_octet_stream().as_ref()),
            None => status(req, 404),
        };
    }
    let rel = path.trim_start_matches('/');
    match ctx.dist.read(&format!("dist/renderer/{rel}")) {
        Some(mut d) => {
            if rel == "renderer.js" {
                // the one custom URL scheme the interface builds itself; route it to our local file protocol
                let s = String::from_utf8_lossy(&d).replace("\"powder-local-video-by-path://", &format!("\"/__pw/proto/powder-local-video-by-path/?t={}&p=", ctx.token));
                d = s.into_bytes();
            }
            bytes(req, d, mime_guess::from_path(rel).first_or_octet_stream().as_ref())
        }
        None => status(req, 404),
    }
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
