//! Port of src/main/features/video-analyzer (bridge "new-video-analyzer"): runs Powder's native
//! analyzer_v2 (in-game events for all supported games, laughter/scream/loudness, transcripts),
//! events_aggregator (suggested moments) and keyword_search.
use crate::host::Host;
use crate::native::{is_abort, Accel, Job};
use crate::server::Ctx;
use anyhow::{bail, Result};
use md5::Digest;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const B: &str = "new-video-analyzer";

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static J: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_task(h: &Host, id: &str, task: Option<Value>) {
    let mut all = h.get(B, "analyzerTasksByExtractionProjectId").unwrap_or(json!({}));
    match task {
        Some(t) => all[id] = t,
        None => {
            if let Some(o) = all.as_object_mut() {
                o.remove(id);
            }
        }
    }
    h.set(B, "analyzerTasksByExtractionProjectId", all);
}

fn patch_task(h: &Host, id: &str, patch: Value) {
    let all = h.get(B, "analyzerTasksByExtractionProjectId").unwrap_or(json!({}));
    let Some(cur) = all.get(id).cloned() else { return };
    set_task(h, id, Some(super::projects::merge(cur, patch)));
}

/// Run analyzer_v2 and wait. `id` (project) enables progress/pause/cancel tracking.
pub fn analyze(h: &Host, id: Option<&str>, input: Value) -> Result<Value> {
    crate::log(&format!("Analyzer input: {input}"));
    let _ = h;
    let hp: &'static Host = &super::ctx().host;
    let pid = id.map(String::from);
    let job = Job::start("analyzer_v2", vec![], input, "analyzer_progress", "analyzer_finished", Accel::from_settings(), move |p| {
        if let Some(id) = &pid {
            patch_task(hp, id, json!({"progress": p["progress"], "etaSeconds": p["etaSeconds"]}));
        }
    })?;
    if let Some(id) = id {
        jobs().lock().insert(id.to_string(), job.clone());
    }
    let r = job.wait();
    if let Some(id) = id {
        jobs().lock().remove(id);
    }
    r
}

fn trim_transcript(lines: &[Value], start: f64, end: f64) -> Vec<Value> {
    lines
        .iter()
        .filter(|l| l["offsets"]["from"].as_f64().unwrap_or(0.0) < end && l["offsets"]["to"].as_f64().unwrap_or(0.0) >= start)
        .cloned()
        .collect()
}

pub fn suggested_moments(params: &Value, duration_ms: f64) -> Result<Vec<Value>> {
    let arr = |k: &str| params.get(k).cloned().filter(|v| v.is_array()).unwrap_or(json!([]));
    let input = json!({
        "eventWhitelist": arr("eventWhitelist"), "keywords": arr("keywords"), "inputDurationMs": duration_ms,
        "audioEvents": arr("audioEvents"), "videoEvents": arr("videoEvents"), "chatEvents": arr("chatEvents"),
        "communityClipsEvents": arr("communityClipsEvents"), "keywordEvents": arr("keywordEvents"), "bookmarkEvents": arr("bookmarkEvents"),
    });
    let job = Job::start("events_aggregator", vec![], input, "events_aggregator_progress", "events_aggregator_finished", Accel::from_settings(), |_| {})?;
    let out = job.wait()?;
    let transcript = params["transcript"].as_array().cloned().unwrap_or_default();
    Ok(out["suggestedMoments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|m| {
            let hash = hex::encode(md5::Md5::digest(m.to_string().as_bytes()));
            let s = m["startTimeMs"].as_f64().unwrap_or(0.0);
            let e = m["endTimeMs"].as_f64().unwrap_or(0.0);
            let mut o = json!({"id": hash, "customStartTimeMs": s, "customEndTimeMs": e});
            if let (Some(oo), Some(mm)) = (o.as_object_mut(), m.as_object()) {
                for (k, v) in mm {
                    oo.insert(k.clone(), v.clone());
                }
                oo.insert("startTimeMs".into(), json!(s.max(0.0)));
                oo.insert("endTimeMs".into(), json!(e.min(duration_ms)));
                oo.insert("transcript".into(), Value::Array(trim_transcript(&transcript, s, e)));
            }
            o
        })
        .collect())
}

pub fn keyword_events(transcript: &Value, keywords: &Value) -> Result<Value> {
    let job = Job::start("keyword_search", vec![], json!({"queries": keywords, "transcript": transcript}), "progress", "finished", Accel::from_settings(), |_| {})?;
    Ok(job.wait()?)
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    let last = super::user_data().join("Feature Storage").join("lastGameAnalyzed.json");
    let lg = std::fs::read_to_string(&last).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| v["powderGameId"].clone()).unwrap_or(Value::Null);
    h.set(B, "lastGameAnalyzed", lg);
    h.set(B, "analyzerTasksByExtractionProjectId", json!({}));

    h.handle(B, "createAnalyzerTaskByExtractionProjectId", move |h, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let o = a.get(1).cloned().unwrap_or(json!({}));
        let p = super::projects::get().get_by_id(&id);
        let Some(video) = p.as_ref().and_then(|p| p["videoPath"].as_str().map(String::from)) else { bail!("extractionProject path not found") };
        let p = p.unwrap();
        set_task(h, &id, Some(json!({"progress": 0, "etaSeconds": 0, "status": "PROCESSING", "result": {}, "paused": false})));
        crate::gpu::block_sleep(&id, true);
        let game = o["powderGameId"].clone();
        h.set(B, "lastGameAnalyzed", game.clone());
        let _ = super::core::write_json(&last, &json!({"powderGameId": game, "date": super::core::now_iso()}));
        let dur_ms = p["durationSec"].as_f64().unwrap_or(0.0) * 1000.0;
        let clips = &p["onlineCues"]["communityClips"];
        let chats = &p["onlineCues"]["twitchChats"];
        let mut input = json!({
            "input": {"path": video, "startSec": o["startSec"], "endSec": o["endSec"], "isLive": false},
            "video": {"gameCode": if game.is_string() { game.clone() } else { Value::Null }, "debugAI": false},
        });
        if !clips.is_null() {
            input["communityClips"] = json!({"clips": clips, "inputDurationMs": dur_ms});
        }
        if o["twitchEventEnabled"] == true && !chats.is_null() {
            input["twitchChats"] = json!({"chat": chats, "inputDurationMs": dur_ms});
        }
        if o["audioModelsEnabled"] == true {
            input["audio"] = json!({"runAudioAnalysis": true});
        }
        if let Some(lang) = o["analyzedLanguage"].as_str().filter(|s| !s.is_empty()) {
            input["transcript"] = json!({"prompt": "", "language": lang, "useGladia": false});
        }
        // drop nulls the native side doesn't expect
        if let Some(i) = input["input"].as_object_mut() {
            i.retain(|_, v| !v.is_null());
        }
        let r = analyze(h, Some(&id), input);
        crate::gpu::block_sleep(&id, false);
        match r {
            Ok(mut result) => {
                if let Some(g) = game.as_str() {
                    match super::customgame::add_model_events(g, &video, o["startSec"].as_f64(), o["endSec"].as_f64(), &mut result) {
                        Ok(n) if n > 0 => crate::log(&format!("visual model added {n} event(s)")),
                        Ok(_) => {}
                        Err(e) => crate::log(&format!("visual model: {e:#}")),
                    }
                }
                patch_task(h, &id, json!({"progress": 1, "result": result.clone(), "status": "DONE"}));
                Ok(result)
            }
            Err(e) if is_abort(&e) => {
                set_task(h, &id, None);
                bail!("CANCEL")
            }
            Err(e) => {
                crate::log(&format!("analysis failed: {e:#}"));
                patch_task(h, &id, json!({"status": "FAILED"}));
                bail!("GLOBAL")
            }
        }
    });
    h.handle(B, "cancelAnalyzerTaskByExtractionProjectId", |_, a| {
        if let Some(j) = jobs().lock().get(a.first().and_then(|v| v.as_str()).unwrap_or("")) {
            j.stop();
        }
        Ok(Value::Null)
    });
    h.handle(B, "tryTogglePauseResumeAnalysisByExtractionProjectId", |h, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let j = jobs().lock().get(&id).cloned();
        if let Some(j) = j {
            let paused = j.toggle_pause();
            patch_task(h, &id, json!({"paused": paused}));
        }
        Ok(Value::Null)
    });
    h.handle(B, "generateTranscriptForVideoEditor", |h, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let mut input = json!({"input": {"path": o["filePath"], "startSec": o["startTimeSec"], "endSec": o["endTimeSec"], "isLive": false},
                               "transcript": {"prompt": "", "language": o["analyzedLanguage"], "useGladia": false}});
        if let Some(i) = input["input"].as_object_mut() {
            i.retain(|_, v| !v.is_null());
        }
        analyze(h, None, input)
    });
    h.handle(B, "createSuggestedMoments", |_, a| {
        let params = a.first().cloned().unwrap_or(json!({}));
        let dur = a.get(1).and_then(|o| o["videoDurationMs"].as_f64()).unwrap_or(0.0);
        Ok(Value::Array(suggested_moments(&params, dur)?))
    });
    h.handle(B, "generateKeywordEvents", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        keyword_events(&o["transcript"], &o["keywords"])
    });
}
