//! Port of src/main/features/native-video-stream: the editor's live preview. The native montage maker
//! renders the timeline in small chunks the interface plays back as they're ready.
use super::montage::MontageProcess;
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

const B: &str = "nativeVideoStream";
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static UPDATE_ID: AtomicU64 = AtomicU64::new(1);

fn procs() -> &'static Mutex<HashMap<u64, Arc<MontageProcess>>> {
    static P: OnceLock<Mutex<HashMap<u64, Arc<MontageProcess>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}
fn illustrations() -> &'static Mutex<HashMap<String, Arc<MontageProcess>>> {
    static P: OnceLock<Mutex<HashMap<String, Arc<MontageProcess>>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}
fn base() -> std::path::PathBuf {
    std::env::temp_dir().join("Powder").join("videoStreamer")
}

/// bridges/native-video-stream.ts formatAutoMontageConfigForNativeStream
fn for_stream(c: &Value) -> Value {
    let w = c["output_resolution"]["width"].as_f64().unwrap_or(1920.0);
    let h = c["output_resolution"]["height"].as_f64().unwrap_or(1080.0);
    let ratio = w / h;
    let (nw, nh) = if c["format"] == "vertical" {
        if ratio.round() == (9.0f64 / 16.0).round() {
            (w, h)
        } else {
            let nw = w.min(1280.0);
            (nw, 2.0 * ((nw / ratio).round() * 9.0 / 16.0 / 2.0).round())
        }
    } else {
        let nw = w.min(1280.0);
        (nw, (nw / ratio).round())
    };
    let mut o = c.clone();
    o["stream"] = json!(true);
    o["output_framerate"] = json!(30);
    o["output_resolution"] = json!({"width": nw, "height": nh});
    o
}

fn stream_input(d: &Value) -> Result<Value> {
    super::montage::to_raw(d)
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    let _ = std::fs::remove_dir_all(base());
    h.handle(B, "newStream", |_, a| {
        let d = stream_input(&a.into_iter().next().unwrap_or(json!({})))?;
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst) + 1;
        std::fs::create_dir_all(base().join(id.to_string()))?;
        let p = MontageProcess::start(&d, |_| {})?;
        procs().lock().insert(id, p);
        Ok(json!(id))
    });
    h.handle(B, "updateStream", |_, a| {
        let id = a.first().and_then(|v| v.as_u64()).unwrap_or(0);
        let d = stream_input(&a.get(1).cloned().unwrap_or(json!({})))?;
        let u = UPDATE_ID.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(p) = procs().lock().get(&id) {
            p.update(&d);
        }
        Ok(json!(u))
    });
    h.handle(B, "stopStream", |_, a| {
        if let Some(p) = procs().lock().remove(&a.first().and_then(|v| v.as_u64()).unwrap_or(0)) {
            p.stop();
        }
        Ok(Value::Null)
    });
    h.handle(B, "getChunk", |_, a| {
        let id = a.first().and_then(|v| v.as_u64()).unwrap_or(0);
        let idx = a.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
        let p = procs().lock().get(&id).cloned();
        let r: Result<Value> = (|| {
            let p = p.context("stream not found")?;
            let path = base().join(id.to_string()).join(format!("{}-{idx}.mp4", UPDATE_ID.load(Ordering::SeqCst)));
            let res = p.render_chunk(idx, &path.display().to_string())?;
            Ok(json!({"result": res, "path": path}))
        })();
        Ok(r.unwrap_or_else(|e| {
            crate::log(&format!("Error while rendering chunk: {e:#}"));
            Value::Null
        }))
    });
    h.handle(B, "cancelAllChunks", |_, a| {
        if let Some(p) = procs().lock().get(&a.first().and_then(|v| v.as_u64()).unwrap_or(0)).cloned() {
            let _ = p.cancel_all_chunks();
        }
        Ok(Value::Null)
    });
    h.handle(B, "generateThumbnail", |_, a| {
        let t = a.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let p = procs().lock().values().last().cloned();
        Ok(p.and_then(|p| p.thumbnail(t, true).ok()).and_then(|v| v.as_str().map(|s| json!(format!("data:image/png;base64, {s}")))).unwrap_or(Value::Null))
    });
    h.handle(B, "getStreamIllustration", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let id = o["id"].as_str().unwrap_or("").to_string();
        let r = (|| -> Result<Value> {
            let d = for_stream(&o["autoMontageConfig"]);
            let p = MontageProcess::start(&d, |_| {})?;
            illustrations().lock().insert(id.clone(), p.clone());
            let b64 = p.thumbnail(0.0, true);
            p.stop();
            Ok(json!(format!("data:image/png;base64, {}", b64?.as_str().unwrap_or(""))))
        })();
        illustrations().lock().remove(&id);
        Ok(r.unwrap_or(Value::Null))
    });
    h.handle(B, "stopStreamIllustration", |_, a| {
        if let Some(p) = illustrations().lock().remove(a.first().and_then(|v| v.as_str()).unwrap_or("")) {
            p.stop();
        }
        Ok(Value::Null)
    });
}
