//! Ports of local-video, thumbnail and recordings: the Sources page (Powder recordings + imports),
//! posters, video playback URLs and timeline thumbnails.
use super::projects;
use crate::host::Host;
use crate::server::Ctx;
use base64::Engine;
use parking_lot::{Condvar, Mutex};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

fn key(path: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path.as_bytes())
}
fn unkey(k: &str) -> Option<String> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(k).ok().and_then(|b| String::from_utf8(b).ok())
}

pub fn poster_url(ctx: &Ctx, path: &str) -> String {
    crate::protocols::url(&ctx.token, "powder-local-video", &format!("/powder-local-video/{}/poster.jpg", key(path)))
}
pub fn video_url(ctx: &Ctx, path: &str) -> String {
    let ext = Path::new(path).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
    crate::protocols::url(&ctx.token, "powder-local-video", &format!("/powder-local-video/{}/video.{ext}", key(path)))
}

/// small limiter like misc/ConcurrencyLimiter (thumbnails run 2 at a time)
struct Limiter {
    n: Mutex<usize>,
    cv: Condvar,
    max: usize,
}
impl Limiter {
    fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut n = self.n.lock();
        while *n >= self.max {
            self.cv.wait(&mut n);
        }
        *n += 1;
        drop(n);
        let r = f();
        *self.n.lock() -= 1;
        self.cv.notify_one();
        r
    }
}
fn limiter() -> &'static Limiter {
    static L: OnceLock<Limiter> = OnceLock::new();
    L.get_or_init(|| Limiter { n: Mutex::new(0), cv: Condvar::new(), max: 2 })
}
fn thumb_cache() -> &'static Mutex<HashMap<String, String>> {
    static C: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn thumbs(video: &str, times: &[f64], w: u32, h: u32) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    let mut todo = vec![];
    for (i, t) in times.iter().enumerate() {
        match thumb_cache().lock().get(&format!("{video}-{t}-{w}-{h}")) {
            Some(c) => {
                out.insert(i.to_string(), json!(c));
            }
            None => todo.push((i.to_string(), *t)),
        }
    }
    if !todo.is_empty() {
        let got = limiter().run(|| crate::media::b64_thumbnails(Path::new(video), &todo, w, h))?;
        for (id, t) in &todo {
            if let Some(b) = got.get(id) {
                let data = format!("data:image/png;base64, {b}");
                thumb_cache().lock().insert(format!("{video}-{t}-{w}-{h}"), data.clone());
                out.insert(id.clone(), json!(data));
            }
        }
    }
    Ok(Value::Object(out))
}

fn as_recording(p: &Value) -> Value {
    let dur_ms = p["videoMetadata"]["duration"]
        .as_f64()
        .filter(|d| *d > 0.0)
        .or_else(|| p["durationAtCreation"].as_f64())
        .or_else(|| p["remoteDurationSec"].as_f64().map(|s| s * 1000.0))
        .unwrap_or(0.0);
    json!({
        "id": p["id"], "source": p["source"], "createdAt": p["createdAt"], "publishedAt": p["publishedAt"],
        "durationSec": p["durationSec"], "durationMs": dur_ms, "size": p["videoMetadata"]["size"],
        "videoUrl": p["videoUrl"], "posterUrl": p["posterUrl"], "game": p["game"], "unwatched": p["unwatched"],
        "path": p["videoPath"], "title": p["title"], "ownerId": p["ownerId"], "ownerDisplayName": p["ownerDisplayName"],
        "extractionProjectId": p["id"], "remoteRecordingId": p["remoteRecordingId"], "extractionProjectStatus": p["status"],
        "automontageEventsCount": p["automontageEventsCount"].as_u64().unwrap_or(0), "missingVideoFile": p["missingVideoFile"],
        "width": p["videoMetadata"]["width"], "height": p["videoMetadata"]["height"],
    })
}

fn recordings(mut v: Vec<Value>) -> Value {
    v.sort_by(|a, b| b["createdAt"].as_str().unwrap_or("").cmp(a["createdAt"].as_str().unwrap_or("")));
    Value::Array(v.iter().map(as_recording).collect())
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    // powder-local-video:///powder-local-video/<key>/(video|poster).<ext>
    crate::protocols::register("powder-local-video", |rest| {
        let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
        let (k, file) = (parts.get(1)?, parts.get(2)?);
        let path = PathBuf::from(unkey(k)?);
        if file.starts_with("video") {
            path.exists().then_some(path)
        } else {
            crate::media::ensure_snapshot(&path)
        }
    });
    // "powder-local-video-by-path://C:/Users/x/v.mp4" (the interface builds this one itself)
    crate::protocols::register("powder-local-video-by-path", |rest| {
        let r = rest.trim_start_matches('/');
        let p = if cfg!(windows) && r.len() > 1 && r.as_bytes()[1] != b':' && r.contains('/') {
            // "C/Users/..." (host without colon) -> "C:\Users\..."
            let (drive, tail) = r.split_once('/').unwrap();
            format!("{drive}:\\{}", tail.replace('/', "\\"))
        } else if cfg!(windows) {
            r.replace('/', "\\")
        } else {
            format!("/{r}")
        };
        Some(PathBuf::from(p))
    });

    h.handle("local-video", "revealInFolder", |_, a| {
        super::core::reveal(a.first().and_then(|v| v.as_str()).unwrap_or(""));
        Ok(Value::Null)
    });
    h.handle("local-video", "deleteLocalFile", |_, a| {
        let p = a.first().and_then(|v| v.as_str()).unwrap_or("");
        crate::gpu::trash(Path::new(p))?;
        Ok(Value::Null)
    });
    h.handle("local-video", "readVideoMetadata", |_, a| Ok(crate::media::metadata(Path::new(a.first().and_then(|v| v.as_str()).unwrap_or("")))?.to_json()));

    h.handle("thumbnail", "getB64ThumbnailWithConcurrencyLimiter", |_, a| {
        let o = a.into_iter().next().unwrap_or(Value::Null);
        let v = o["videoPath"].as_str().unwrap_or("");
        let r = thumbs(v, &[o["timeInS"].as_f64().unwrap_or(0.0)], o["width"].as_u64().unwrap_or(160) as u32, o["height"].as_u64().unwrap_or(90) as u32)?;
        Ok(r.get("0").cloned().unwrap_or(json!("")))
    });
    h.handle("thumbnail", "getB64ThumbnailListWithConcurrencyLimiter", |_, a| {
        let o = a.into_iter().next().unwrap_or(Value::Null);
        let times: Vec<f64> = o["timeListInS"].as_array().map(|x| x.iter().filter_map(|t| t.as_f64()).collect()).unwrap_or_default();
        thumbs(o["videoPath"].as_str().unwrap_or(""), &times, o["width"].as_u64().unwrap_or(160) as u32, o["height"].as_u64().unwrap_or(90) as u32)
    });
    h.handle("thumbnail", "cancelGetThumbnail", |_, _| Ok(Value::Null));
    h.handle("thumbnail", "cancelGetThumbnailList", |_, _| Ok(Value::Null));

    // recordings
    let sections = super::user_data().join("Feature Storage").join("recordings_sections.json");
    let mut coll = json!({"TWITCH": false, "YOUTUBE": false, "IMPORTED": false, "POWDER": false});
    if let Some(Value::Object(m)) = std::fs::read_to_string(&sections).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        for (k, v) in m {
            coll[&k] = v;
        }
    }
    h.set("recordings", "collapsedSections", coll);
    h.handle("recordings", "toggleCollapseSectionByType", move |h, a| {
        let s = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut c = h.get("recordings", "collapsedSections").unwrap_or(json!({}));
        c[&s] = json!(!c[&s].as_bool().unwrap_or(false));
        super::core::write_json(&sections, &c)?;
        h.set("recordings", "collapsedSections", c);
        Ok(Value::Null)
    });
    let p = projects::get();
    p.query("recordings", "importedRecordings", |p| ["kick", "twitch", "youtube", "local-file-import"].iter().any(|s| p["source"] == *s), recordings);
    p.query("recordings", "powderRecordings", |p| p["source"] == "recorder", recordings);
    let _ = ctx;
}
