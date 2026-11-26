//! Port of src/main/features/bookmark: the Bookmarks page (clips saved from recordings), clip export
//! (16:9 or vertical) through the native montage maker.
use super::projects;
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::sync::Arc;

const B: &str = "bookmark";
const DEFAULT_CROPS: &str = r#"[{"in":[0.34197530864,0,0.31604938271,1],"out":[0,0,1,1],"locked":false,"cover":true}]"#;

fn host() -> &'static Host {
    &super::ctx().host
}

fn info(p: &Value) -> Value {
    json!({
        "title": p["title"], "id": p["id"], "videoUrl": p["videoUrl"].as_str().unwrap_or(""), "videoPath": p["videoPath"].as_str().unwrap_or(""),
        "createdAt": p["createdAt"], "updatedAt": p["updatedAt"], "durationSec": p["durationSec"],
        "width": p["videoMetadata"]["width"], "height": p["videoMetadata"]["height"], "framerate": p["videoMetadata"]["framerate"],
        "missingVideoFile": p["missingVideoFile"], "source": p["source"],
    })
}

/// helpers/captions.ts sanitizeTranscript
fn sanitize(transcript: &Value, duration: f64, start: f64) -> Vec<Value> {
    let lines: Vec<Value> = transcript
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            let t = c["text"].as_str().unwrap_or("").trim_start();
            !(t.starts_with('*') || t.starts_with('[') || t.starts_with('('))
        })
        .map(|mut c| {
            let f = c["offsets"]["from"].as_f64().unwrap_or(0.0);
            let t = c["offsets"]["to"].as_f64().unwrap_or(0.0);
            let from = if f - start < 0.0 { start } else { f };
            let to = if t - start > duration { start + duration } else { t };
            c["offsets"] = json!({"from": from, "to": to});
            c["position"] = Value::Null;
            c["scale"] = json!(1);
            c
        })
        .collect();
    let n = lines.len();
    let mut out = vec![];
    for i in 0..n {
        let mut c = lines[i].clone();
        if i + 1 < n {
            let next_from = lines[i + 1]["offsets"]["from"].as_f64().unwrap_or(0.0);
            if c["offsets"]["to"].as_f64().unwrap_or(0.0) > next_from {
                c["offsets"]["to"] = json!(next_from);
            }
        }
        let text = c["text"].as_str().unwrap_or("").to_string();
        let text = text.strip_prefix('-').unwrap_or(&text).trim_start().to_string();
        c["text"] = json!(text);
        let (f, t) = (c["offsets"]["from"].as_f64().unwrap_or(0.0), c["offsets"]["to"].as_f64().unwrap_or(0.0));
        if f - start < duration && t > f {
            c["id"] = json!(super::montage::uuid());
            out.push(c);
        }
    }
    out
}

fn as_bookmark(moment: &Value, info: &Value, game: &Value, project_template: &Value) -> Value {
    let templates = host().get("vertical-format", "templatesById").unwrap_or(json!({}));
    let gid = game["powderGameId"].as_str().unwrap_or("");
    let vt = [&moment["verticalTemplate"], project_template, &templates[gid]["crops"], &templates["default"]["crops"]]
        .into_iter()
        .find(|v| !v.is_null() && *v != &Value::Bool(false))
        .cloned()
        .unwrap_or_else(|| serde_json::from_str(DEFAULT_CROPS).unwrap());
    let s = moment["customStartTimeMs"].as_f64().unwrap_or(0.0);
    let e = moment["customEndTimeMs"].as_f64().unwrap_or(0.0);
    let mut b = moment.clone();
    b["verticalTemplate"] = vt;
    b["transcript"] = Value::Array(sanitize(&moment["transcript"], e - s, s));
    b["extractionProject"] = info.clone();
    b["game"] = game.clone();
    b
}

fn section(p: &Value) -> Value {
    let i = info(p);
    let list: Vec<Value> = p["cartMomentsById"].as_object().map(|m| m.values().map(|mo| as_bookmark(mo, &i, &p["game"], &p["verticalTemplate"])).collect()).unwrap_or_default();
    json!({"orderIndex": 0, "game": p["game"], "extractionProject": i, "bookmarkList": list})
}

fn sections(a: &Value) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = projects::get().all().iter().map(|p| (p["id"].as_str().unwrap_or("").to_string(), section(p))).collect();
    if a["removeEmpty"] == true {
        v.retain(|(_, s)| s["bookmarkList"].as_array().map(|l| !l.is_empty()).unwrap_or(false));
    }
    if let Some(kws) = a["keywords"].as_array().filter(|k| !k.is_empty()) {
        let kws: Vec<String> = kws.iter().filter_map(|k| k.as_str().map(|s| s.to_lowercase())).collect();
        v.retain(|(_, s)| {
            s["bookmarkList"].as_array().unwrap().iter().any(|b| {
                b["transcript"].as_array().map(|t| t.iter().any(|l| kws.iter().all(|k| l["text"].as_str().unwrap_or("").to_lowercase().contains(k)))).unwrap_or(false)
            })
        });
    }
    if let Some(g) = a["powderGameId"].as_str().filter(|g| !g.is_empty()) {
        v.retain(|(_, s)| {
            let id = s["game"]["powderGameId"].as_str();
            if g == "no-game" { id.is_none() || id == Some("<< no game >>") } else { id == Some(g) }
        });
    }
    if let Some(evs) = a["events"].as_array() {
        let checked: Vec<&Value> = evs.iter().filter(|e| e["checked"] == true).collect();
        if !checked.is_empty() {
            for (_, s) in v.iter_mut() {
                let l: Vec<Value> = s["bookmarkList"].as_array().unwrap().iter().filter(|b| b["events"].as_array().map(|es| es.iter().any(|e| checked.iter().any(|c| c["id"] == e["eventName"]))).unwrap_or(false)).cloned().collect();
                s["bookmarkList"] = json!(l);
            }
        }
    }
    if let Some(max) = a["maximumDuration"].as_f64().filter(|m| *m > 0.0) {
        for (_, s) in v.iter_mut() {
            let l: Vec<Value> = s["bookmarkList"].as_array().unwrap().iter().filter(|b| (b["customEndTimeMs"].as_f64().unwrap_or(0.0) - b["customStartTimeMs"].as_f64().unwrap_or(0.0)) / 1000.0 <= max).cloned().collect();
            s["bookmarkList"] = json!(l);
        }
    }
    if a["orderBy"] == "DATE" {
        v.sort_by(|x, y| y.1["extractionProject"]["updatedAt"].as_str().unwrap_or("").cmp(x.1["extractionProject"]["updatedAt"].as_str().unwrap_or("")));
        for (i, (_, s)) in v.iter_mut().enumerate() {
            s["orderIndex"] = json!(i);
        }
    }
    v
}

fn get_bookmark(pid: &str, bid: &str) -> Result<Value> {
    let p = projects::get().get_by_id(pid).with_context(|| format!("Missing extraction project: {pid}"))?;
    let m = p["cartMomentsById"].get(bid).cloned().with_context(|| format!("Bookmark with id {bid} not found in extraction project with id {pid}"))?;
    Ok(as_bookmark(&m, &info(&p), &p["game"], &p["verticalTemplate"]))
}

fn set_export(status: &str, progress: f64, bookmarks: Value) {
    host().set(B, "exportProcess", json!({"progress": progress, "status": status, "bookmarks": bookmarks}));
}

fn export_one(b: &Value, out: &str, overlays: &Value, vertical: bool, open: bool, height: f64, fps: f64, outro: bool) -> Result<()> {
    let h = host();
    let ep = h.get(B, "exportProcess").unwrap_or(json!({"status": "READY"}));
    if ep["status"] != "READY" && !ep["bookmarks"].as_array().map(|l| l.iter().any(|x| x["id"] == b["id"] && x["extractionProject"]["videoPath"] == b["extractionProject"]["videoPath"])).unwrap_or(false) {
        bail!("Export already in progress");
    }
    if super::montage::is_processing(h) {
        bail!("MontageMaker: montage already running");
    }
    if ep["status"] == "READY" {
        set_export("EXPORTING", 0.0, json!([b]));
    }
    let info = &b["extractionProject"];
    let (w, hh, f) = (info["width"].as_f64(), info["height"].as_f64(), info["framerate"].as_f64());
    let (Some(w), Some(hh), Some(f)) = (w, hh, f) else { bail!("Bookmark recording width, height or framerate is missing") };
    let s = b["customStartTimeMs"].as_f64().unwrap_or(0.0) / 1000.0;
    let e = b["customEndTimeMs"].as_f64().unwrap_or(0.0) / 1000.0;
    let ov: Vec<Value> = overlays
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|o| {
            let data = o["data"].as_str().unwrap_or("");
            json!({"image": {"type": "Base64ImageSpec", "data": data.get(22..).unwrap_or("")},
                   "timeRange": {"beginInSec": o["from"].as_f64().unwrap_or(0.0) / 1000.0, "endInSec": o["to"].as_f64().unwrap_or(0.0) / 1000.0},
                   "sourceRectNormalised": {"x":0,"y":0,"w":1,"h":1}, "destRectNormalised": {"x":0,"y":0,"w":1,"h":1}})
        })
        .collect();
    let desc = json!({
        "outputWidth": if height == -1.0 { w } else { w / hh * height },
        "outputHeight": if height == -1.0 { hh } else { height },
        "outputFramerate": if fps == -1.0 { f } else { fps },
        "outputPath": out,
        "sections": [{"path": info["videoPath"], "inputTimeRange": {"beginInSec": s, "endInSec": e}, "montageTimeRange": {"beginInSec": 0, "endInSec": e - s},
                      "transition": {"type": ""}, "verticalTemplate": if vertical { b["verticalTemplate"].clone() } else { json!(false) }}],
        "outro": outro, "overlays": ov,
    });
    let cur = h.get(B, "exportProcess").unwrap_or(json!({}));
    let base = cur["progress"].as_f64().unwrap_or(0.0);
    let list = cur["bookmarks"].clone();
    let n = list.as_array().map(|l| l.len().max(1)).unwrap_or(1) as f64;
    if n == 1.0 {
        super::montage::set_state(h, json!({"videosIds": [info["id"]]}));
    }
    let l2 = list.clone();
    let r = super::montage::launch(h, &desc, move |p| set_export("EXPORTING", base + p.clamp(0.0, 1.0) / n, l2.clone()));
    let single = h.get(B, "exportProcess").map(|e| e["bookmarks"].as_array().map(|l| l.len()).unwrap_or(0) == 1).unwrap_or(true);
    if single {
        set_export("READY", 0.0, json!([]));
        super::montage::set_state(h, json!({"videosIds": []}));
    }
    match r {
        Ok(_) => {
            if open {
                super::core::open_path(out);
            }
            Ok(())
        }
        Err(e) if crate::native::is_abort(&e) => {
            set_export("READY", 0.0, json!([]));
            Ok(())
        }
        Err(e) => {
            set_export("READY", 0.0, json!([]));
            Err(e)
        }
    }
}

fn update_moments(pid: &str, f: impl FnOnce(&mut Map<String, Value>)) -> Result<()> {
    let p = projects::get().get_by_id(pid).unwrap_or(json!({}));
    let mut m = p["cartMomentsById"].as_object().cloned().unwrap_or_default();
    f(&mut m);
    projects::get().update(pid, json!({"cartMomentsById": m}), false)
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    set_export("READY", 0.0, json!([]));
    h.handle(B, "getBookmarksSectionGroupedByProjectId", |_, a| {
        Ok(Value::Object(sections(&a.into_iter().next().unwrap_or(json!({}))).into_iter().collect()))
    });
    h.handle(B, "getBookmark", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        get_bookmark(o["extractionProjectId"].as_str().unwrap_or(""), o["bookmarkId"].as_str().unwrap_or(""))
    });
    h.handle(B, "getBookmarks", |_, a| {
        let l = a.into_iter().next().unwrap_or(json!([]));
        let v: Result<Vec<Value>> = l.as_array().cloned().unwrap_or_default().iter().map(|o| get_bookmark(o["extractionProjectId"].as_str().unwrap_or(""), o["bookmarkId"].as_str().unwrap_or(""))).collect();
        Ok(Value::Array(v?))
    });
    h.handle(B, "getGameListOfBookmarksSections", |_, a| {
        let mut out: Vec<Value> = vec![];
        for (_, s) in sections(&a.into_iter().next().unwrap_or(json!({}))) {
            let g = if s["game"].is_null() { json!({"powderGameId": "no-game", "name": "Universal game support", "iconUrl": "", "coverUrl": "", "bannerUrl": ""}) } else { s["game"].clone() };
            if !out.iter().any(|x| x["powderGameId"] == g["powderGameId"]) {
                out.push(g);
            }
        }
        Ok(Value::Array(out))
    });
    h.handle(B, "setVerticalTemplateToBookmark", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let bid = o["bookmarkId"].as_str().unwrap_or("").to_string();
        update_moments(o["extractionProjectId"].as_str().unwrap_or(""), |m| {
            let mut c = m.get(&bid).cloned().unwrap_or(json!({}));
            c["verticalTemplate"] = o["verticalTemplate"].clone();
            m.insert(bid, c);
        })?;
        Ok(Value::Null)
    });
    h.handle(B, "setVerticalTemplateToAllBookmarksOfAExtractionProject", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let pid = o["extractionProjectId"].as_str().unwrap_or("").to_string();
        let p = projects::get().get_by_id(&pid).unwrap_or(json!({}));
        let mut m = p["cartMomentsById"].as_object().cloned().unwrap_or_default();
        for v in m.values_mut() {
            v["verticalTemplate"] = o["verticalTemplate"].clone();
        }
        projects::get().update(&pid, json!({"verticalTemplate": o["verticalTemplate"], "cartMomentsById": m}), false)?;
        Ok(Value::Null)
    });
    h.handle(B, "setBookmarkTranscript", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let bid = o["bookmarkId"].as_str().unwrap_or("").to_string();
        update_moments(o["extractionProjectId"].as_str().unwrap_or(""), |m| {
            let mut c = m.get(&bid).cloned().unwrap_or(json!({}));
            c["transcript"] = o["transcript"].clone();
            m.insert(bid, c);
        })?;
        Ok(Value::Null)
    });
    h.handle(B, "removeBookmark", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let ids: Vec<Value> = o["bookmarkIdList"].as_array().cloned().unwrap_or_default();
        update_moments(o["extractionProjectId"].as_str().unwrap_or(""), |m| m.retain(|k, _| !ids.iter().any(|i| i == k.as_str())))?;
        Ok(Value::Null)
    });
    h.handle(B, "removeBookmarks", |_, a| {
        for o in a.into_iter().next().and_then(|v| v.as_array().cloned()).unwrap_or_default() {
            let ids: Vec<Value> = o["bookmarkIdList"].as_array().cloned().unwrap_or_default();
            update_moments(o["extractionProjectId"].as_str().unwrap_or(""), |m| m.retain(|k, _| !ids.iter().any(|i| i == k.as_str())))?;
        }
        Ok(Value::Null)
    });
    h.handle(B, "exportBookmark", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        export_one(&o["bookmark"], o["outputFolderPath"].as_str().unwrap_or(""), &o["overlays"], o["isVertical"] == true, o["openPath"] != false,
                   o["exportHeight"].as_f64().unwrap_or(-1.0), o["exportFramerate"].as_f64().unwrap_or(-1.0), o["exportOutro"] == true)?;
        Ok(Value::Null)
    });
    h.handle(B, "exportBookmarks", |h, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        if h.get(B, "exportProcess").map(|e| e["status"] != "READY").unwrap_or(false) {
            bail!("Export already in progress");
        }
        let items = o["bookmarksWithOverlay"].as_array().cloned().unwrap_or_default();
        set_export("EXPORTING", 0.0, Value::Array(items.iter().map(|x| x["bookmark"].clone()).collect()));
        let mut ids: Vec<Value> = vec![];
        for x in &items {
            let id = x["bookmark"]["extractionProject"]["id"].clone();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        super::montage::set_state(h, json!({"videosIds": ids}));
        let folder = o["outputFolderPath"].as_str().unwrap_or("").to_string();
        let vertical = o["isVertical"] == true;
        let mut res = Ok(());
        for x in &items {
            let b = &x["bookmark"];
            let name = format!("powder-clip-{}-{}-{}", b["customStartTimeMs"].as_f64().unwrap_or(0.0), b["customEndTimeMs"].as_f64().unwrap_or(0.0), if vertical { "vertical" } else { "horizontal" });
            let path = std::path::Path::new(&folder).join(format!("{name}.mp4")).display().to_string();
            if let Err(e) = export_one(b, &path, &x["overlays"], vertical, false, o["exportHeight"].as_f64().unwrap_or(-1.0), o["exportFramerate"].as_f64().unwrap_or(-1.0), o["exportOutro"] == true) {
                res = Err(e);
                break;
            }
            if h.get(B, "exportProcess").map(|e| e["status"] == "READY").unwrap_or(false) {
                break; // cancelled
            }
        }
        if res.is_ok() {
            super::core::open_path(&folder);
        }
        super::montage::set_state(h, json!({"videosIds": []}));
        set_export("READY", 0.0, json!([]));
        res.map(|_| Value::Null)
    });
    h.handle(B, "cancelExportBookmark", |_, _| {
        super::montage::stop();
        set_export("READY", 0.0, json!([]));
        Ok(Value::Null)
    });
}
