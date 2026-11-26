//! Port of src/main/features/video-editor: editor projects (Feature Storage/Video editor projects/*.json),
//! assets (recordings, bookmarks, local files, overlays, music), auto-montage -> editor and export.
use super::{montage, projects};
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const B: &str = "video-editor";

fn host() -> &'static Host {
    &super::ctx().host
}
fn folder() -> PathBuf {
    super::user_data().join("Feature Storage").join("Video editor projects")
}
fn file(id: &str) -> PathBuf {
    folder().join(format!("{id}.json"))
}
fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("Powder").join("VideoEditor")
}
fn read(id: &str) -> Result<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(file(id)).with_context(|| format!("project {id}"))?)?)
}
fn write(id: &str, v: &Value) -> Result<()> {
    super::core::write_json(&file(id), v)
}
fn exists_nonempty(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
}
fn short_id() -> String {
    montage::uuid()[..8].to_string()
}

fn initial_state() -> Value {
    json!({
        "currentTab": "CLIPS", "montageSections": [], "overlays": [],
        "captionsGlobalStyle": {"case": "default", "hAlign": "center", "vAlign": null, "style": "style1"},
        "videoAssetsById": {}, "format": "horizontal", "displayCaptions": false, "captionsInit": false,
        "localOverlayAssets": [], "effects": [], "musics": [], "musicAssets": []
    })
}

fn is_bookmark(a: &Value) -> bool {
    a["extractionProjectId"].is_string() && a["type"] == "bookmark"
}
fn is_norm_bookmark(a: &Value) -> bool {
    is_bookmark(a) && a.get("videoUrl").is_none() && a.get("path").is_none() && a.get("title").is_none()
}
fn is_norm_bookmark_deleted(a: &Value) -> bool {
    is_bookmark(a) && a.get("videoUrl").is_none() && a["path"].is_string() && a["title"].is_string()
}
fn is_denormalized_state(s: &Value) -> bool {
    s["videoAssetsById"].as_object().map(|m| m.values().any(|a| a["videoUrl"].is_string())).unwrap_or(false)
}

fn without(mut v: Value, keys: &[&str]) -> Value {
    if let Some(o) = v.as_object_mut() {
        for k in keys {
            o.remove(*k);
        }
    }
    v
}

fn normalize_assets(m: &Value) -> Value {
    Value::Object(
        m.as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, a)| {
                let n = if is_bookmark(&a) && a["videoUrl"].is_string() {
                    if a["existingExtractionProject"] == true {
                        without(a, &["videoUrl", "path", "title", "filename", "missingVideoFile", "existingExtractionProject"])
                    } else {
                        without(a, &["videoUrl", "filename", "missingVideoFile", "existingExtractionProject"])
                    }
                } else {
                    without(a, &["videoUrl", "title", "filename", "missingVideoFile", "posterUrl"])
                };
                (id, n)
            })
            .collect(),
    )
}

fn denormalize_assets(m: &Value) -> Value {
    let ctx = super::ctx();
    Value::Object(
        m.as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, a)| {
                let mut a = a;
                if is_norm_bookmark_deleted(&a) {
                    let p = a["path"].as_str().unwrap_or("").to_string();
                    a["missingVideoFile"] = json!(!exists_nonempty(&p));
                    a["videoUrl"] = json!(super::library::video_url(ctx, &p));
                    a["fileName"] = json!(Path::new(&p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
                    a["existingExtractionProject"] = json!(false);
                } else if is_norm_bookmark(&a) {
                    let pid = a["extractionProjectId"].as_str().unwrap_or("").to_string();
                    match projects::get().read(&pid).ok().flatten() {
                        Some(p) => {
                            a["videoUrl"] = p["videoUrl"].clone();
                            a["path"] = p["videoPath"].clone();
                            a["missingVideoFile"] = p["missingVideoFile"].clone();
                            a["missingExtractionProject"] = json!(false);
                            a["title"] = p["title"].clone();
                            a["fileName"] = json!(Path::new(p["videoPath"].as_str().unwrap_or("")).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default());
                            a["existingExtractionProject"] = json!(true);
                        }
                        None => {
                            for (k, v) in [("missingExtractionProject", json!(true)), ("missingVideoFile", json!(true)), ("videoUrl", Value::Null), ("path", json!("")), ("title", json!("")), ("fileName", json!("")), ("existingExtractionProject", json!(false))] {
                                a[k] = v;
                            }
                        }
                    }
                } else if let Some(p) = a["path"].as_str().map(String::from) {
                    let f = Path::new(&p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    a["missingVideoFile"] = json!(!exists_nonempty(&p));
                    a["videoUrl"] = json!(super::library::video_url(ctx, &p));
                    a["posterUrl"] = json!(super::library::poster_url(ctx, &p));
                    a["title"] = json!(f);
                    a["fileName"] = json!(f);
                }
                (id, a)
            })
            .collect(),
    )
}

fn mark_missing(list: &Value, key: &str) -> Value {
    Value::Array(
        list.as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut a| {
                if !Path::new(a[key].as_str().unwrap_or("")).exists() {
                    a["assetIsMissing"] = json!(true);
                }
                a
            })
            .collect(),
    )
}

/// sanitizeVideoEditorProjectById: upgrades old project files like the original
fn sanitize(mut p: Value) -> Value {
    let mut s = p["state"].clone();
    let mut changed = false;
    let fix_vt = |v: &Value| if v["crops"].is_array() { v["crops"].clone() } else if v.is_null() { json!([]) } else { v.clone() };
    if let Some(secs) = s["montageSections"].as_array_mut() {
        for sec in secs.iter_mut() {
            if sec["verticalTemplate"]["crops"].is_array() {
                sec["verticalTemplate"] = fix_vt(&sec["verticalTemplate"]);
                changed = true;
            }
            if sec.get("captions").is_none() {
                sec["captions"] = json!([]);
                changed = true;
            }
        }
    }
    if s.get("captionsInit").map(|v| v.is_null() || v == false).unwrap_or(true) && !s["displayCaptions"].is_null() {
        s["captionsInit"] = s["displayCaptions"].clone();
    }
    if let Some(m) = s["videoAssetsById"].as_object_mut() {
        for a in m.values_mut() {
            if a["verticalTemplate"]["crops"].is_array() {
                a["verticalTemplate"] = fix_vt(&a["verticalTemplate"]);
                changed = true;
            }
            if a.get("transcript").is_none() {
                a["transcript"] = json!([]);
                changed = true;
            }
            if a["assetStartTimeSec"].is_null() {
                a["assetStartTimeSec"] = json!(0);
                changed = true;
            }
            if a["assetEndTimeSec"].as_f64().unwrap_or(0.0) == 0.0 {
                a["assetEndTimeSec"] = a["durationSec"].clone();
                changed = true;
            }
            if a["framerate"].is_null() {
                let fr = if is_norm_bookmark(a) {
                    projects::get().get_by_id(a["extractionProjectId"].as_str().unwrap_or("")).and_then(|p| p["videoMetadata"]["framerate"].as_f64())
                } else {
                    a["path"].as_str().and_then(|p| crate::media::metadata(Path::new(p)).ok()).map(|m| m.framerate)
                };
                a["framerate"] = json!(fr.filter(|f| *f > 0.0).unwrap_or(60.0));
                changed = true;
            }
            if a["videoUrl"].is_string() {
                let keep_bookmark = is_bookmark(a);
                *a = if keep_bookmark { without(a.clone(), &["videoUrl", "path", "title"]) } else { without(a.clone(), &["videoUrl", "posterUrl", "title"]) };
                changed = true;
            }
        }
    }
    for (k, d) in [("localOverlayAssets", json!([])), ("musicAssets", json!([]))] {
        if s[k].is_null() {
            s[k] = d;
            changed = true;
        }
    }
    if s.get("localVideoAssets").is_some() {
        s = without(s, &["localVideoAssets"]);
        changed = true;
    }
    p["state"] = s;
    if changed {
        if let Some(id) = p["projectId"].as_str() {
            let _ = write(id, &p);
        }
    }
    p
}

fn get_project(id: &str) -> Result<Value> {
    let p = sanitize(read(id)?);
    let mut s = p["state"].clone();
    s["videoAssetsById"] = denormalize_assets(&s["videoAssetsById"]);
    s["localOverlayAssets"] = mark_missing(&s["localOverlayAssets"], "path");
    s["musicAssets"] = mark_missing(&s["musicAssets"], "realPath");
    let mv = s["videoAssetsById"].as_object().map(|m| m.values().any(|a| a["missingVideoFile"] == true)).unwrap_or(false);
    let mo = s["localOverlayAssets"].as_array().map(|l| l.iter().any(|a| a["assetIsMissing"] == true)).unwrap_or(false);
    let ma = s["musicAssets"].as_array().map(|l| l.iter().any(|a| a["assetIsMissing"] == true)).unwrap_or(false);
    let mut out = p;
    out["state"] = s;
    out["missingVideoFiles"] = json!(mv);
    out["missingOverlayAssets"] = json!(mo);
    out["missingAudioAssets"] = json!(ma);
    Ok(out)
}

fn list() -> Vec<Value> {
    let _ = std::fs::create_dir_all(folder());
    let mut v: Vec<Value> = std::fs::read_dir(folder())
        .map(|rd| rd.flatten().filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").and_then(|id| read(id).ok())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| {
        let d = |x: &Value| x["updatedAt"].as_str().or(x["createdAt"].as_str()).unwrap_or("").to_string();
        d(b).cmp(&d(a))
    });
    v
}

fn refresh() {
    let l = list();
    host().set(B, "videoEditorProjectsCount", json!(l.len()));
    host().set(B, "videoEditorProjectList", Value::Array(l));
}

fn update(id: &str, state: Value, silent: bool) -> Result<()> {
    let mut p = read(id)?;
    let mut st = state;
    if is_denormalized_state(&st) {
        st["videoAssetsById"] = normalize_assets(&st["videoAssetsById"]);
    }
    if !silent {
        p["updatedAt"] = json!(super::core::now_iso());
    }
    p["state"] = projects::merge(p["state"].clone(), st);
    write(id, &p)
}

pub fn import_videos(paths: &[String]) -> Result<Vec<Value>> {
    let default = host().get("vertical-format", "templatesById").map(|t| t["default"]["crops"].clone()).filter(|c| c.is_array()).unwrap_or(json!([]));
    let ctx = super::ctx();
    paths
        .iter()
        .map(|p| {
            let m = crate::media::metadata(Path::new(p)).context("Can't read video metadata")?;
            let f = Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            Ok(json!({
                "id": short_id(), "type": "local", "title": f, "filename": f, "durationSec": m.duration_ms as f64 / 1000.0, "path": p,
                "videoUrl": super::library::video_url(ctx, p), "posterUrl": super::library::poster_url(ctx, p),
                "width": m.width, "height": m.height, "createdAt": super::automontage_iso(m.created_at),
                "events": [], "verticalTemplate": default, "transcript": [], "assetStartTimeSec": 0, "assetEndTimeSec": m.duration_ms as f64 / 1000.0,
                "framerate": m.framerate, "missingVideoFile": false,
            }))
        })
        .collect()
}

fn import_overlays(paths: &[String]) -> Result<Vec<Value>> {
    let ctx = super::ctx();
    paths
        .iter()
        .map(|p| {
            let sz = imagesize::size(p).context("Error importing file, could not get image size")?;
            let ty = imagesize::image_type(&std::fs::read(p)?).map(|t| format!("{t:?}").to_lowercase()).unwrap_or_else(|_| "image".into());
            Ok(json!({
                "id": short_id(), "path": p, "imageUrl": crate::protocols::url(&ctx.token, "powder-local-overlay", &format!("/{p}")),
                "fileName": Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                "createdAt": super::core::now_iso(), "width": sz.width, "height": sz.height, "type": ty, "assetIsMissing": false,
            }))
        })
        .collect()
}

fn event_def(game: &str, name: &str) -> Value {
    if let Some(e) = super::cues::model(game).and_then(|m| m["events"].as_array().cloned()).and_then(|l| l.into_iter().find(|e| e["name"] == name)) {
        return json!({"name": name, "displayName": e["displayName"], "icon": e["icon"]});
    }
    if let Some(e) = super::cues::audio_events().into_iter().chain(super::cues::other_events()).find(|e| e["name"] == name) {
        return json!({"name": name, "displayName": e["displayName"], "icon": e["icon"]});
    }
    json!({"name": name, "displayName": name, "icon": name})
}

fn state_from_automontage(cfg: &Value) -> Result<Value> {
    let mut paths: Vec<String> = vec![];
    for c in cfg["clips"].as_array().cloned().unwrap_or_default() {
        if let Some(p) = c["path"].as_str() {
            if !paths.iter().any(|x| x == p) {
                paths.push(p.to_string());
            }
        }
    }
    let videos = cfg["videos"].as_array().cloned().unwrap_or_default();
    let assets: Vec<Value> = import_videos(&paths)?
        .into_iter()
        .map(|mut a| {
            if let Some(vc) = videos.iter().find(|v| v["path"] == a["path"]) {
                let game = vc["gameId"].as_str().unwrap_or("");
                let mut evs = vec![];
                for (_, list) in vc["events"].as_object().cloned().unwrap_or_default() {
                    for e in list.as_array().cloned().unwrap_or_default() {
                        let d = event_def(game, e["eventName"].as_str().unwrap_or(""));
                        evs.push(json!({"id": d["icon"], "displayName": d["displayName"], "eventName": e["eventName"], "iconName": d["icon"], "score": e["score"],
                                        "startTimeMs": e["startTimeMs"], "suggestedEndContextMs": e["suggestedEndContextMs"], "suggestedStartContextMs": e["suggestedStartContextMs"], "keyword": e["keyword"]}));
                    }
                }
                a["events"] = json!(evs);
            }
            a
        })
        .collect();
    let by_id: Map<String, Value> = assets.iter().map(|a| (a["id"].as_str().unwrap_or("").to_string(), a.clone())).collect();
    let crops = cfg["croppings"].as_array().cloned().unwrap_or_default();
    let transitions = cfg["transitions"].as_array().cloned().unwrap_or_default();
    let sections: Result<Vec<Value>> = cfg["clips"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let tr = c.get("inputTimeRange").filter(|v| v.is_object()).context("Missing inputTimeRange in clip")?;
            let aid = assets.iter().find(|a| a["path"] == c["path"]).map(|a| a["id"].clone()).context("Missing video asset id")?;
            Ok(json!({"id": montage::uuid(), "videoAsset": aid, "assetStartTimeSec": tr["beginInSec"], "assetEndTimeSec": tr["endInSec"], "captions": [], "initialCaptions": [],
                      "playbackSpeed": c["playbackSpeed"].as_f64().unwrap_or(1.0), "transition": transitions.get(i).cloned().unwrap_or(Value::Null),
                      "verticalTemplate": crops.get(i).map(|c| c["crops"].clone()).unwrap_or(json!([]))}))
        })
        .collect();
    // effects: packed into rows without overlap
    let mut rows: Vec<Vec<Value>> = vec![];
    for fx in cfg["visualEffects"].as_array().cloned().unwrap_or_default() {
        let s = fx["beginInSec"].as_f64().unwrap_or(0.0);
        let e = s + fx["durationInSec"].as_f64().unwrap_or(0.0);
        let item = json!({"effectName": fx["effectName"], "name": fx["effectName"], "assetStartTimeSec": s, "assetEndTimeSec": e, "id": montage::uuid()});
        match rows.iter_mut().find(|r| r.iter().all(|x| e <= x["assetStartTimeSec"].as_f64().unwrap_or(0.0) || s >= x["assetEndTimeSec"].as_f64().unwrap_or(0.0))) {
            Some(r) => {
                r.push(item);
                r.sort_by(|a, b| a["assetStartTimeSec"].as_f64().unwrap_or(0.0).total_cmp(&b["assetStartTimeSec"].as_f64().unwrap_or(0.0)));
            }
            None => rows.insert(0, vec![item]),
        }
    }
    let musics: Vec<Value> = cfg["musics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|m| {
            let b = m["montageTimeRange"]["beginInSec"].as_f64().unwrap_or(0.0);
            let e = m["montageTimeRange"]["endInSec"].as_f64().unwrap_or(0.0);
            json!([{"id": montage::uuid(), "assetStartTimeSec": b, "assetEndTimeSec": e, "audioGain": m["audioGain"].as_f64().unwrap_or(1.0), "musicDurationSec": m["durationSec"],
                    "inputTimeRange": m["inputTimeRange"], "musicData": {"title": m["title"], "artist": "", "label": "", "durationSec": e - b, "path": m["path"], "powderPath": ""}, "assetIsMissing": false}])
        })
        .collect();
    let mut st = initial_state();
    st["format"] = cfg.get("format").cloned().filter(|v| !v.is_null()).unwrap_or(json!("horizontal"));
    st["montageSections"] = json!(sections?);
    st["videoAssetsById"] = Value::Object(by_id);
    st["effects"] = json!(rows);
    st["musics"] = json!(musics);
    Ok(st)
}

fn export(proj: &Value, height: f64, fps: f64, outro: bool) -> Result<Value> {
    let h = host();
    if montage::is_processing(h) {
        bail!("MontageMaker: montage already running");
    }
    let secs = proj["montageSections"].as_array().cloned().unwrap_or_default();
    let vertical = proj["format"] == "vertical";
    let out = proj["outputPath"].as_str().context("Missing output path")?.to_string();
    let temp = proj["isTemp"] == true;
    let (mut hw, mut hh, mut hf) = (0.0f64, 0.0f64, 0.0f64);
    for s in &secs {
        let a = &s["videoAsset"];
        hw = hw.max(a["width"].as_f64().unwrap_or(0.0));
        hh = hh.max(a["height"].as_f64().unwrap_or(0.0));
        hf = hf.max(a["framerate"].as_f64().unwrap_or(0.0));
    }
    std::fs::create_dir_all(temp_dir())?;
    let mut sections: Vec<Value> = vec![];
    for s in &secs {
        let speed = s["playbackSpeed"].as_f64().unwrap_or(1.0);
        let prev = sections.last();
        let prev_tr = prev.map(|p| p["transition"]["durationInSec"].as_f64().unwrap_or(0.0) / speed).unwrap_or(0.0);
        let begin = prev.map(|p| p["montageTimeRange"]["endInSec"].as_f64().unwrap_or(0.0) - prev_tr).unwrap_or(0.0);
        let (ib, ie) = (s["assetStartTimeSec"].as_f64().unwrap_or(0.0), s["assetEndTimeSec"].as_f64().unwrap_or(0.0));
        sections.push(json!({
            "path": s["videoAsset"]["path"], "inputTimeRange": {"beginInSec": ib, "endInSec": ie},
            "montageTimeRange": {"beginInSec": begin, "endInSec": begin + (ie - ib) / speed},
            "transition": if s["transition"].is_null() { json!({"type": "NONE"}) } else { s["transition"].clone() },
            "verticalTemplate": if vertical { s["verticalTemplate"].clone() } else { json!(false) }, "playbackSpeed": speed,
        }));
    }
    let overlays: Vec<Value> = proj["overlays"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| {
            let d = o["data"].as_str().unwrap_or("");
            let b64 = d.starts_with("data:image/png");
            let (from, to) = (o["from"].as_f64().unwrap_or(0.0), o["to"].as_f64().unwrap_or(0.0));
            json!({
                "image": if b64 { json!({"type": "Base64ImageSpec", "data": d.get(22..).unwrap_or("")}) } else { json!({"type": "OnDiskImageSpec", "path": d}) },
                "timeRange": if b64 { json!({"beginInSec": from / 1000.0, "endInSec": to / 1000.0}) } else { json!({"beginInSec": from, "endInSec": to}) },
                "sourceRectNormalised": {"x":0,"y":0,"w":1,"h":1}, "destRectNormalised": o["destRectNormalized"],
            })
        })
        .collect();
    let desc = json!({
        "outputWidth": if height == -1.0 { hw } else { hw / hh * height }, "outputHeight": if height == -1.0 { hh } else { height },
        "outputFramerate": if fps == -1.0 { hf } else { fps },
        "outputPath": if temp { temp_dir().join("PowderVideoEditorTemp.mp4").display().to_string() } else { out.clone() },
        "sections": sections, "outro": outro, "overlays": overlays, "visualEffects": proj["effects"], "musics": proj["musics"],
    });
    let mut ids: Vec<Value> = vec![];
    for s in &secs {
        let id = s["videoAsset"]["id"].clone();
        if !id.is_null() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    montage::set_state(h, json!({"processing": true, "progress": 0, "videosIds": ids}));
    let r = montage::launch(h, &desc, |_| {});
    montage::set_state(h, json!({"videosIds": []}));
    match r {
        Ok(_) => {
            if !temp {
                super::core::reveal(&out);
            }
            Ok(json!(out))
        }
        Err(e) if crate::native::is_abort(&e) => Ok(Value::Null),
        Err(e) => Err(e),
    }
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    crate::protocols::register("powder-local-overlay", |rest| {
        let p = PathBuf::from(rest.trim_start_matches('/'));
        p.exists().then_some(p)
    });
    refresh();
    h.handle(B, "getVideoEditorProjectList", |_, _| Ok(Value::Array(list())));
    h.handle(B, "refreshVideoEditorProjectList", |_, _| {
        refresh();
        Ok(Value::Null)
    });
    h.handle(B, "createVideoEditorProject", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let id = montage::uuid();
        let mut st = o["state"].clone();
        if is_denormalized_state(&st) {
            st["videoAssetsById"] = normalize_assets(&st["videoAssetsById"]);
        }
        write(&id, &json!({"projectId": id, "name": o["name"], "createdAt": super::core::now_iso(), "state": st}))?;
        refresh();
        Ok(json!({"projectId": id}))
    });
    h.handle(B, "getVideoEditorProjectById", |_, a| get_project(a.first().and_then(|v| v.as_str()).unwrap_or("")));
    h.handle(B, "updateVideoEditorProjectById", |_, a| {
        update(a.first().and_then(|v| v.as_str()).unwrap_or(""), a.get(1).cloned().unwrap_or(json!({})), a.get(2).map(|o| o["silent"] == true).unwrap_or(false))?;
        Ok(Value::Null)
    });
    h.handle(B, "updateVideoEditorProjectNameById", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let mut p = read(id)?;
        p["updatedAt"] = json!(super::core::now_iso());
        p["name"] = a.get(1).cloned().unwrap_or(Value::Null);
        write(id, &p)?;
        refresh();
        Ok(Value::Null)
    });
    h.handle(B, "deleteVideoEditorProjectById", |_, a| {
        let _ = std::fs::remove_file(file(a.first().and_then(|v| v.as_str()).unwrap_or("")));
        refresh();
        Ok(Value::Null)
    });
    h.handle(B, "importLocalVideoAssets", |_, a| Ok(Value::Array(import_videos(&strs(a.first()))?)));
    h.handle(B, "importLocalOverlayAssets", |_, a| Ok(Value::Array(import_overlays(&strs(a.first()))?)));
    h.handle(B, "getVideoEditorStateFromAutoMontageConfig", |_, a| state_from_automontage(&a.into_iter().next().unwrap_or(json!({}))));
    h.handle(B, "getVideoEditorStateByAutoMontageId", |_, a| {
        // the id passed is the auto montage's video path (loadAutoMontageConfig works by file name)
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let name = Path::new(id).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let p = super::user_data().join("Feature Storage").join("Automontage Configs").join(format!("{name}.json"));
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(p).context("Missing auto montage config")?)?;
        state_from_automontage(&cfg)
    });
    h.handle(B, "dropVideoEditorAssetWithMissingFiles", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let p = get_project(id)?;
        let mut s = p["state"].clone();
        let missing: Vec<String> = s["videoAssetsById"].as_object().map(|m| m.iter().filter(|(_, v)| v["missingVideoFile"] == true).map(|(k, _)| k.clone()).collect()).unwrap_or_default();
        if let Some(m) = s["videoAssetsById"].as_object_mut() {
            m.retain(|k, _| !missing.contains(k));
        }
        if let Some(l) = s["montageSections"].as_array_mut() {
            l.retain(|x| !missing.iter().any(|m| x["videoAsset"] == m.as_str()));
        }
        for (assets, timeline, field, key) in [("localOverlayAssets", "overlays", "localOverlayAsset", "path"), ("musicAssets", "musics", "musicData", "realPath")] {
            let gone: Vec<String> = s[assets].as_array().map(|l| l.iter().filter(|a| a["assetIsMissing"] == true).filter_map(|a| a[key].as_str().map(String::from)).collect()).unwrap_or_default();
            if let Some(l) = s[assets].as_array_mut() {
                l.retain(|a| a["assetIsMissing"] != true);
            }
            let rows: Vec<Value> = s[timeline].as_array().cloned().unwrap_or_default().into_iter().filter_map(|row| {
                let r: Vec<Value> = row.as_array().cloned().unwrap_or_default().into_iter().filter(|it| !gone.iter().any(|g| it[field]["path"] == g.as_str())).collect();
                (!r.is_empty()).then(|| json!(r))
            }).collect();
            s[timeline] = json!(rows);
        }
        update(id, s, false)?;
        Ok(Value::Null)
    });
    h.handle(B, "relinkOverlayAsset", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let prev = a.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mw = a.get(3).and_then(|v| v.as_f64()).unwrap_or(1920.0);
        let new = import_overlays(&strs(a.get(2)))?.into_iter().next().context("no image")?;
        let p = get_project(&id)?;
        let mut s = p["state"].clone();
        if let Some(l) = s["localOverlayAssets"].as_array_mut() {
            for x in l.iter_mut() {
                if x["id"] == prev.as_str() {
                    *x = new.clone();
                }
            }
        }
        if let Some(rows) = s["overlays"].as_array_mut() {
            for row in rows.iter_mut() {
                for it in row.as_array_mut().into_iter().flatten() {
                    if it["localOverlayAsset"]["id"] == prev.as_str() {
                        let w = mw * 0.2;
                        let hgt = w * new["height"].as_f64().unwrap_or(1.0) / new["width"].as_f64().unwrap_or(1.0);
                        let pos = it["position"].clone();
                        it["localOverlayAsset"] = new.clone();
                        it["position"] = json!([pos[0], pos[1], w / 1920.0, hgt / 1080.0]);
                    }
                }
            }
        }
        update(&id, s, false)?;
        Ok(Value::Null)
    });
    h.handle(B, "relinkAudioAsset", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let prev = a.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let new = a.get(2).cloned().unwrap_or(json!({}));
        let Some(dur) = crate::media::duration_secs(Path::new(new["realPath"].as_str().unwrap_or(""))) else { return Ok(Value::Null) };
        let p = get_project(&id)?;
        let mut s = p["state"].clone();
        if let Some(l) = s["musicAssets"].as_array_mut() {
            for x in l.iter_mut() {
                if x["id"] == new["id"] {
                    *x = new.clone();
                }
            }
        }
        let rows: Vec<Value> = s["musics"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|row| {
            let r: Vec<Value> = row.as_array().cloned().unwrap_or_default().into_iter().filter_map(|mut it| {
                if it["musicData"]["path"] == prev.as_str() {
                    if it["musicDurationSec"].as_f64().unwrap_or(0.0).floor() != dur.floor() {
                        return None;
                    }
                    it["musicData"]["powderPath"] = new["fakePath"].clone();
                    it["musicData"]["path"] = new["realPath"].clone();
                    it["musicData"]["durationSec"] = json!(dur);
                }
                Some(it)
            }).collect();
            (!r.is_empty()).then(|| json!(r))
        }).collect();
        s["musics"] = json!(rows);
        update(&id, s, false)?;
        Ok(Value::Null)
    });
    h.handle(B, "exportMontage", |_, a| {
        let o = a.get(1).cloned().unwrap_or(json!({}));
        export(&a.first().cloned().unwrap_or(json!({})), o["exportHeight"].as_f64().unwrap_or(-1.0), o["exportFramerate"].as_f64().unwrap_or(-1.0), o["exportOutro"] == true)
    });
}
