//! Port of src/main/features/auto-edits: one click -> YouTube (silence removed), Shorts and X edits of a
//! recording, previewed in the editor and exported through the montage maker.
use super::{automontage, montage, projects};
use crate::host::Host;
use crate::native::{Accel, Job};
use crate::server::Ctx;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

const B: &str = "auto-edits";

fn host() -> &'static Host {
    &super::ctx().host
}

fn running() -> &'static Mutex<HashMap<String, (Vec<Arc<Job>>, Arc<AtomicBool>)>> {
    static R: OnceLock<Mutex<HashMap<String, (Vec<Arc<Job>>, Arc<AtomicBool>)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_task(id: &str, t: Option<Value>) {
    let mut all = host().get(B, "autoEditsTasksByExtractionProjectId").unwrap_or(json!({}));
    match t {
        Some(t) => all[id] = t,
        None => {
            if let Some(o) = all.as_object_mut() {
                o.remove(id);
            }
        }
    }
    host().set(B, "autoEditsTasksByExtractionProjectId", all);
}
fn patch(id: &str, p: Value) {
    let all = host().get(B, "autoEditsTasksByExtractionProjectId").unwrap_or(json!({}));
    if let Some(cur) = all.get(id).cloned() {
        set_task(id, Some(projects::merge(cur, p)));
    }
}

/// config.ts generateConfig
fn configs() -> Vec<(&'static str, Value)> {
    let m = super::music::list();
    let t = |i: usize| (m.get(i).map(|x| x["path"].clone()).unwrap_or(Value::Null), m.get(i).map(|x| x["title"].clone()).unwrap_or(json!("")));
    let am = |i: usize, bal: &str, order: &str, x: bool| {
        let (p, ti) = t(i);
        json!({"type": "AUTO_MONTAGE", "title": "Banger Reel", "isVertical": true, "musicsPaths": [p], "musicTitle": ti, "audioBalance": bal, "sortOrder": order, "shareOnX": x})
    };
    vec![
        ("YOUTUBE", json!({"type": "SILENCE_REMOVAL", "title": "", "silenceRemovalValue": 0.25})),
        ("YOUTUBE_SHORTS", am(1, "balanced", "chronological", false)),
        ("YOUTUBE_SHORTS", am(2, "gameFocused", "bestStardAndFinished", false)),
        ("YOUTUBE_SHORTS", am(3, "musicFocused", "randomized", false)),
        ("TWITTER", am(1, "balanced", "chronological", true)),
        ("TWITTER", am(2, "gameFocused", "bestStardAndFinished", true)),
        ("TWITTER", am(3, "musicFocused", "randomized", true)),
    ]
}

fn run_native(id: &str, input: Value, total: usize, with_progress: bool) -> Result<Value> {
    let pid = id.to_string();
    let job = Job::start("auto_montage_maker", vec![], input, "auto_montage_progress", "auto_montage_finished", Accel::from_settings(), move |p| {
        if with_progress {
            let cur = host().get(B, "autoEditsTasksByExtractionProjectId").and_then(|a| a[&pid]["progress"].as_f64()).unwrap_or(0.0);
            patch(&pid, json!({"progress": cur + p["progress"].as_f64().unwrap_or(0.0) / total as f64}));
        }
    })?;
    if let Some(r) = running().lock().get_mut(id) {
        r.0.push(job.clone());
    }
    job.wait()
}

fn generate(id: &str) -> Result<Value> {
    let p = projects::get().read(id)?.ok_or_else(|| anyhow::anyhow!("Extraction project {id} not found"))?;
    let cfgs = configs();
    let total = cfgs.len();
    let mut out = serde_json::Map::new();
    for (ty, c) in cfgs {
        let entry = out.entry(ty.to_string()).or_insert(json!([]));
        let d = if c["type"] == "SILENCE_REMOVAL" {
            let loud = &p["rawResult"]["loudnessData"];
            if loud.is_null() {
                continue;
            }
            let input = json!({"outputPath": automontage::output_path(), "videoPath": p["videoPath"], "threshold": c["silenceRemovalValue"], "loudnessData": loud});
            let mut d = run_native(id, input.clone(), total, false)?;
            // lay the kept parts back to back for the preview stream
            let mut last = 0.0;
            let clips: Vec<Value> = d["clips"].as_array().cloned().unwrap_or_default().into_iter().map(|mut cl| {
                let b = cl["inputTimeRange"]["beginInSec"].as_f64().unwrap_or(0.0);
                let e = cl["inputTimeRange"]["endInSec"].as_f64().unwrap_or(0.0);
                let dur = if e > 0.0 { e - b } else { 0.0 };
                cl["montageTimeRange"] = json!({"beginInSec": last, "endInSec": last + dur});
                last += dur;
                cl
            }).collect();
            if clips.is_empty() {
                continue;
            }
            d["clips"] = json!(clips);
            let d = automontage::with_export_params(d, 30.0, 1080.0, false, false, &[p.clone()]);
            automontage::for_player(&d, false, &input, "", None)
        } else {
            let musics: Vec<String> = c["musicsPaths"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect();
            let input = automontage::montage_input(&[p.clone()], &musics, None, None, None, c["audioBalance"].as_str().unwrap_or("balanced"), c["sortOrder"].as_str().unwrap_or("chronological"), c["shareOnX"] == true);
            let d = run_native(id, input.clone(), total, true)?;
            if d["clips"].as_array().map(|c| c.is_empty()).unwrap_or(true) {
                continue;
            }
            let d = automontage::with_export_params(d, 30.0, 1080.0, false, c["isVertical"] == true, &[p.clone()]);
            automontage::for_player(&d, c["isVertical"] == true, &input, c["musicTitle"].as_str().unwrap_or(""), c["title"].as_str())
        };
        entry.as_array_mut().unwrap().push(d);
    }
    Ok(Value::Object(out))
}

fn start(id: String) -> Result<Value> {
    set_task(&id, Some(json!({"progress": 0, "etaSeconds": 0, "status": "GENERATING", "result": null, "paused": false})));
    running().lock().insert(id.clone(), (vec![], Arc::new(AtomicBool::new(false))));
    let r = generate(&id);
    let stopped = running().lock().remove(&id).map(|r| r.1.load(Ordering::SeqCst)).unwrap_or(false);
    match r {
        Ok(v) => {
            patch(&id, json!({"progress": 1, "result": v.clone(), "status": "DONE"}));
            Ok(v)
        }
        Err(_) if stopped => Ok(Value::Null),
        Err(e) => {
            patch(&id, json!({"status": "FAILED"}));
            bail!("2 Failed to generate auto montage description record: {e:#}")
        }
    }
}

fn stop(id: &str) {
    if let Some((jobs, flag)) = running().lock().get(id) {
        flag.store(true, Ordering::SeqCst);
        for j in jobs {
            j.stop();
        }
    }
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    h.set(B, "autoEditsTasksByExtractionProjectId", json!({}));
    h.set(B, "exportAutoEditProcess", json!({"status": "READY", "progress": 0}));
    h.handle(B, "createGenerateAutoEditsTaskByExtractionProjectId", |_, a| start(a.first().and_then(|v| v.as_str()).unwrap_or("").to_string()));
    h.handle(B, "cancelGenerateAutoEditsTaskByExtractionProjectId", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        stop(id);
        set_task(id, None);
        Ok(Value::Null)
    });
    h.handle(B, "tryTogglePauseResumeAutoEditsByExtractionProjectId", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let st = host().get(B, "autoEditsTasksByExtractionProjectId").map(|t| t[&id]["status"].clone()).unwrap_or(Value::Null);
        if st == "IS_PAUSED" {
            patch(&id, json!({"status": "GENERATING"}));
            std::thread::spawn(move || {
                let _ = start(id);
            });
        } else {
            stop(&id);
            patch(&id, json!({"status": "IS_PAUSED"}));
        }
        Ok(Value::Null)
    });
    h.handle(B, "exportAutoEdit", |h, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        if h.get(B, "exportAutoEditProcess").map(|e| e["status"] == "EXPORTING").unwrap_or(false) {
            bail!("Export already in progress");
        }
        let mut d = o["montageDescription"].clone();
        let vertical = d["format"] == "vertical";
        match (o["exportOutro"] == true).then(|| montage::outro_path(vertical)).flatten() {
            Some(p) => d["outro"] = json!(p),
            None => {
                if let Some(m) = d.as_object_mut() {
                    m.remove("outro");
                }
            }
        }
        h.set(B, "exportAutoEditProcess", json!({"status": "EXPORTING", "progress": 0}));
        let r = montage::launch(&super::ctx().host, &d, |p| host().set(B, "exportAutoEditProcess", json!({"status": "EXPORTING", "progress": p})));
        h.set(B, "exportAutoEditProcess", json!({"status": "READY", "progress": 0}));
        let out = r.map_err(|e| anyhow::anyhow!("Failed to export auto edit: {e:#}"))?;
        if let Some(p) = out.as_str() {
            super::core::open_path(p);
        }
        Ok(out)
    });
    h.handle(B, "cancelExportAutoEdit", |h, _| {
        if h.get(B, "exportAutoEditProcess").map(|e| e["status"] == "EXPORTING").unwrap_or(false) {
            montage::stop();
        }
        Ok(Value::Null)
    });
}
