//! Port of src/main/features/recorder (+ initializeShortcuts): recording through Powder's native
//! recorder (DXGI capture, hardware encoders, game/mic/Discord audio, live analysis), manual bookmarks,
//! and the recording -> project with suggested moments.
use super::projects;
use crate::host::Host;
use crate::native::{Accel, NativeProcess};
use crate::server::Ctx;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

const B: &str = "recorder";

fn host() -> &'static Host {
    &super::ctx().host
}

fn proc_slot() -> &'static Mutex<Option<NativeProcess>> {
    static P: OnceLock<Mutex<Option<NativeProcess>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(None))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

pub fn is_recording() -> bool {
    host().get(B, "status") == Some(json!("IS_RECORDING"))
}

pub fn start() -> Result<()> {
    if proc_slot().lock().is_some() {
        return Ok(());
    }
    let s = super::settings::read();
    if s["allowRecorder"] != true {
        // the original opens the "allow recording" modal
        host().stream("window", "setModal", json!({"event": "ALLOW_RECORDING_MODAL"}));
        host().window.send("show");
        return Ok(());
    }
    let folder = s["recorderFolder"].as_str().context("Missing required settings")?.to_string();
    std::fs::create_dir_all(&folder)?;
    let mut args = vec![
        "--displayIndex".to_string(), s["recorderScreenDevice"].as_i64().unwrap_or(0).to_string(),
        "--recordingSizeHeight".into(), s["recorderSizeHeight"].as_i64().unwrap_or(-1).to_string(),
        "--framerate".into(), s["recorderFramerate"].as_i64().unwrap_or(60).to_string(),
        "--outputFolder".into(), folder,
        "--quality".into(), s["recorderQuality"].as_str().unwrap_or("LOW").to_string(),
    ];
    if s["recorderAudioInputEnabled"] == true {
        if let Some(d) = s["recorderAudioInputDevice"].as_str().filter(|d| !d.is_empty()) {
            args.extend(["--audioDeviceId".into(), d.to_string()]);
        }
    }
    if s["recorderAudioForegroundEnabled"] == true {
        args.extend(["--audioDeviceId".into(), "foreground".into()]);
    }
    if s["recorderAudioDiscordEnabled"] == true {
        args.extend(["--audioDeviceId".into(), "Discord.exe".into()]);
    }
    let live = host().get(B, "liveAnalysisGameCode").filter(|v| v.is_string());
    if let Some(g) = &live {
        args.extend(["--liveAnalyzerConfig".into(), json!({"videoConfig": {"gameCode": g, "debugAI": false}, "dumpMinipowData": s["dumpMinipowData"] == true}).to_string()]);
    }
    host().set(B, "status", json!("IS_RECORDING"));
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiting = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (st, wt) = (stopped.clone(), waiting.clone());
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let p = NativeProcess::start(
        "recorder",
        args,
        Accel::from_settings(),
        Box::new(move |name, data| {
            use std::sync::atomic::Ordering::SeqCst;
            match name {
                "hybrid_graphics_restart" => wt.store(true, SeqCst),
                "recorder_connected" => wt.store(false, SeqCst),
                "recording_early_exit" => {
                    std::thread::spawn(|| {
                        let _ = stop("EarlyExit");
                    });
                }
                "video_saved" => {
                    if !done.swap(true, SeqCst) {
                        let d = data.clone();
                        std::thread::spawn(move || {
                            if let Err(e) = on_saved(&d) {
                                crate::log(&format!("recording save: {e:#}"));
                            }
                            finished();
                        });
                    }
                }
                n if n.ends_with("ERROR_JSON") => {
                    crate::log(&format!("recorder error: {data}"));
                    if !done.swap(true, SeqCst) {
                        host().stream("toast", "push", json!({"type": "error", "message": data["message"].as_str().unwrap_or("Recording failed")}));
                        finished();
                    }
                }
                "connection-closed" => {
                    if !wt.load(SeqCst) && !st.load(SeqCst) && !done.swap(true, SeqCst) {
                        crate::log("RecorderProcess connection-closed unexpectedly");
                        finished();
                    }
                }
                _ => {}
            }
        }),
    );
    let p = match p {
        Ok(p) => p,
        Err(e) => {
            host().set(B, "status", json!("IS_READY"));
            return Err(e);
        }
    };
    *proc_slot().lock() = Some(p);
    STOPPED.get_or_init(|| Mutex::new(None)).lock().replace(stopped);
    host().set(B, "startDate", json!(now_ms()));
    crate::gpu::block_sleep("recording", true);
    Ok(())
}

static STOPPED: OnceLock<Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>> = OnceLock::new();

fn finished() {
    proc_slot().lock().take();
    host().set(B, "startDate", Value::Null);
    host().set(B, "status", json!("IS_READY"));
    crate::gpu::block_sleep("recording", false);
}

pub fn stop(_from: &str) -> Result<()> {
    let p = proc_slot().lock().clone();
    let Some(p) = p else { return Ok(()) };
    if let Some(start) = host().get(B, "startDate").and_then(|v| v.as_u64()) {
        if now_ms() - start < 1000 {
            return Ok(());
        }
    }
    host().set(B, "status", json!("IS_SAVING"));
    if let Some(s) = STOPPED.get().and_then(|s| s.lock().clone()) {
        s.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let _ = p.push("stop", None);
    Ok(())
}

pub fn bookmark() {
    let p = proc_slot().lock().clone();
    if let Some(p) = p {
        if host().get(B, "status") == Some(json!("IS_RECORDING")) {
            super::sounds::play("notification");
            let _ = p.push("bookmark", None);
        }
    }
}

fn on_saved(d: &Value) -> Result<()> {
    let path = d["path"].as_str().context("no path")?.to_string();
    let vm = crate::media::metadata(std::path::Path::new(&path)).with_context(|| format!("Not able to read video metadata (width, height, framerate). path={path}"))?;
    let bookmarks = d["bookmarkEvents"].clone().as_array().cloned().unwrap_or_default();
    let mut optional = json!({"bookmarkEvents": bookmarks});
    let live = host().get(B, "liveAnalysisGameCode").filter(|v| v.is_string());
    let mut filters: Vec<Value> = vec![];
    if let Some(g) = &live {
        let g = g.as_str().unwrap_or("");
        let visual = super::cues::model(g).and_then(|m| m["events"].as_array().cloned()).unwrap_or_default();
        for e in visual.iter().chain(super::cues::audio_events().iter()).chain(super::cues::other_events().iter()) {
            filters.push(json!({"id": e["name"], "checked": e["default"] == true}));
        }
        let mut raw = d["analysisResult"].clone();
        if let Some(o) = raw.as_object_mut() {
            o.remove("bookmarkEvents");
        }
        optional["rawResult"] = if raw.is_null() { json!({}) } else { raw };
        optional["powderGameId"] = json!(g);
        optional["status"] = json!("ANALYZED");
        optional["unwatched"] = json!(true);
        optional["keywordsAndFilter"] = json!({"events": filters});
    }
    if !bookmarks.is_empty() {
        let whitelist: Vec<Value> = if filters.is_empty() { vec![json!("bookmark")] } else { filters.iter().filter(|f| f["checked"] == true).map(|f| f["id"].clone()).collect() };
        let params = json!({"eventWhitelist": whitelist, "keywords": [], "audioEvents": optional["rawResult"]["audioEvents"].clone().as_array().cloned().unwrap_or_default(),
            "videoEvents": optional["rawResult"]["videoEvents"].clone().as_array().cloned().unwrap_or_default(), "chatEvents": [], "communityClipsEvents": [], "keywordEvents": [],
            "powderGameId": live.clone().unwrap_or(Value::Null), "transcript": [], "bookmarkEvents": bookmarks});
        if let Ok(ms) = super::analyzer::suggested_moments(&params, vm.duration_ms as f64) {
            optional["cartMomentsById"] = Value::Object(ms.into_iter().map(|m| (m["id"].as_str().unwrap_or("").to_string(), m)).collect());
        }
    }
    projects::get().create(json!({"videoPath": path, "source": "recorder", "durationAtCreation": vm.duration_ms}), Some(optional))?;
    super::sounds::play("record-saved");
    Ok(())
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    h.set(B, "status", json!("IS_READY"));
    h.set(B, "liveAnalysisGameCode", Value::Null);
    h.set(B, "startDate", Value::Null);
    h.handle(B, "startRecording", |_, _| {
        start()?;
        Ok(Value::Null)
    });
    h.handle(B, "stopRecording", |_, a| {
        stop(a.first().and_then(|o| o["from"].as_str()).unwrap_or("app"))?;
        Ok(Value::Null)
    });
    h.handle(B, "setLiveAnalysisGameCode", |h, a| {
        h.set(B, "liveAnalysisGameCode", a.into_iter().next().unwrap_or(Value::Null));
        Ok(Value::Null)
    });
    // initializeShortcuts
    crate::hotkeys::on("TOGGLE_RECORD_SESSION", || {
        let rec = host().get(B, "status") == Some(json!("IS_RECORDING"));
        let r = if rec { stop("shortcut") } else { start() };
        if let Err(e) = r {
            crate::log(&format!("shortcut: {e:#}"));
        }
    });
    crate::hotkeys::on("MANUAL_BOOKMARK", bookmark);
    let res = crate::hotkeys::reload();
    let mut sc = super::settings::read()["shortcuts"].clone();
    for (ty, ok) in res {
        sc[&ty]["registered"] = json!(ok);
    }
    h.set("settings", "shortcuts", sc);
    let _ = bail_unused();
}

fn bail_unused() -> Result<()> {
    if false {
        bail!("")
    }
    Ok(())
}
