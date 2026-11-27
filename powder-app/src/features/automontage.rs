//! Port of src/main/features/automontage: Auto Montages page (folder of generated montages), candidate
//! recordings, length estimate and generation (native auto_montage_maker -> montage_maker).
use super::{montage, projects};
use crate::host::Host;
use crate::native::{is_abort, Accel, Job};
use crate::server::Ctx;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

const B: &str = "automontage";

fn host() -> &'static Host {
    &super::ctx().host
}

fn folder() -> PathBuf {
    PathBuf::from(super::settings::read()["autoMontagesFolder"].as_str().unwrap_or("."))
}

fn config_path(video: &str) -> PathBuf {
    let name = Path::new(video).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    super::user_data().join("Feature Storage").join("Automontage Configs").join(format!("{name}.json"))
}

fn by_id() -> &'static Mutex<BTreeMap<String, Value>> {
    static M: OnceLock<Mutex<BTreeMap<String, Value>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn publish() {
    let mut v: Vec<Value> = by_id().lock().values().cloned().collect();
    v.sort_by(|a, b| b["createdAt"].as_str().unwrap_or("").cmp(a["createdAt"].as_str().unwrap_or("")));
    host().set(B, "normalizedAutoMontage", Value::Array(v));
}

pub fn iso_pub(ms: u128) -> String {
    iso(ms)
}

fn iso(ms: u128) -> String {
    let s = (ms / 1000) as i64;
    let (d, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    let z = d + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, ms % 1000)
}

fn register(path: &Path, unwatched: bool) -> Option<Value> {
    let meta = super::super::mp4meta::read(path)?;
    if meta["kind"] != "AUTO_MONTAGE" {
        return None;
    }
    let vm = crate::media::metadata(path).ok()?;
    let ctx = super::ctx();
    let p = path.display().to_string();
    let fname = path.file_name()?.to_string_lossy().to_string();
    let a = json!({
        "id": meta["id"], "unwatch": unwatched, "path": p, "filename": fname, "fileKey": p,
        "title": meta["title"].as_str().unwrap_or(&fname), "createdAt": iso(vm.created_at),
        "durationSec": (vm.duration_ms as f64 / 1000.0).round(), "durationMs": vm.duration_ms, "width": vm.width, "height": vm.height,
        "posterUrl": super::library::poster_url(ctx, &p), "videoUrl": super::library::video_url(ctx, &p), "powderMetadata": meta, "framerate": vm.framerate,
    });
    by_id().lock().insert(a["id"].as_str().unwrap_or(&p).to_string(), a.clone());
    Some(a)
}

fn scan() {
    by_id().lock().clear();
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("mp4")).unwrap_or(false) {
                register(&p, false);
            }
        }
    }
    publish();
}

fn events_for(game: &str, main: &mut Vec<Value>, secondary: &mut Vec<Value>) {
    let mut all: Vec<Value> = super::cues::model(game).and_then(|m| m["events"].as_array().cloned()).unwrap_or_default();
    all.extend(super::cues::audio_events());
    all.extend(super::cues::other_events());
    for e in all {
        let item = json!({"eventName": e["name"], "offsetBefore": e["automontage"]["offsetBefore"], "offsetAfter": e["automontage"]["offsetAfter"], "effectTypes": e["automontage"]["effectTypes"]});
        if e["automontage"]["primary"] == true && !main.iter().any(|m| m["eventName"] == e["name"]) {
            main.push(item.clone());
        }
        if e["automontage"]["secondary"] == true && !secondary.iter().any(|m| m["eventName"] == e["name"]) {
            secondary.push(item);
        }
    }
}

/// Keep every clip inside the video: the montage maker crashes ("native error signal") when a
/// clip runs past the video's end, which happens for events in the last few seconds (the clip
/// adds time after each event). Those events move earlier just enough to fit.
fn inside_video(evs: Value, dur_ms: f64, after_ms: f64) -> Value {
    if dur_ms <= 0.0 {
        return evs;
    }
    let end = dur_ms - 200.0;
    let evs: Vec<Value> = evs
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mut e| {
            let t = e["startTimeMs"].as_f64()?;
            if t > dur_ms {
                return None;
            }
            let t = t.min(end - after_ms).max(0.0);
            e["startTimeMs"] = json!(t);
            if let Some(x) = e["suggestedEndContextMs"].as_f64() {
                e["suggestedEndContextMs"] = json!(x.min(end).max(t));
            }
            if let Some(x) = e["suggestedStartContextMs"].as_f64() {
                e["suggestedStartContextMs"] = json!(x.min(t).max(0.0));
            }
            Some(e)
        })
        .collect();
    json!(evs)
}

/// Clip times around each highlight, in seconds.
const CLIP_BEFORE: f64 = 5.0;
const CLIP_AFTER: f64 = 4.0;
/// Events this close are the same moment: a kill and all its medals (kingslayer, revenge,
/// quickscope...) are read from the screen a moment apart, but are one clip.
const SAME_MOMENT: f64 = 2.0;
/// Events further apart than this are separate kills when counting a multi-kill's kills.
const KILL_APART: f64 = 0.6;
/// Longest time between two kills of one multi-kill.
const MULTI_GAP: f64 = 6.0;

/// How many kills a multi-kill event stands for (0 = not a multi-kill).
fn multi_kills(name: &str) -> usize {
    let l = name.to_lowercase();
    if l.contains("double") { 2 }
    else if l.contains("triple") { 3 }
    else if l.contains("quad") || l.contains("fury") { 4 }
    else if l.contains("penta") || l.contains("frenzy") { 5 }
    else if l.contains("hexa") || l.contains("super") { 6 }
    else if l.contains("ace") { 5 }
    else { 0 }
}
const TRANSITION: f64 = 0.5;

/// Replace the montage maker's clips with one clip per highlight; a multi-kill is one clip from
/// its first kill to its last. Best first until the montage is full, then in video order. Music, output
/// size and audio levels stay as the montage maker chose them.
fn per_event_clips(desc: &Value, input: &Value, ps: &[Value]) -> Value {
    let wanted: Vec<String> = input["mainEvents"].as_array().map(|a| a.iter().filter_map(|e| e["eventName"].as_str().map(String::from)).collect()).unwrap_or_default();
    let use_event = |n: &str| {
        let l = n.to_lowercase();
        if wanted.is_empty() { !(l.contains("death") || l.contains("assist")) } else { wanted.iter().any(|w| w == n) }
    };
    // extra weight for bigger moments
    let weight = |n: &str| {
        let l = n.to_lowercase();
        if ["quadra", "quad", "penta", "ace", "frenzy", "fury"].iter().any(|k| l.contains(k)) { 4.0 }
        else if l.contains("triple") || l.contains("streak") { 3.0 }
        else if l.contains("double") || l.contains("oneshot") || l.contains("quickscope") || l.contains("headshot") { 2.0 }
        else { 1.0 }
    };
    // the game's own clip times (ms before/after in the event editor) and importance
    let offs: std::collections::HashMap<String, (f64, f64)> = input["mainEvents"].as_array().cloned().unwrap_or_default().iter()
        .filter_map(|e| Some((e["eventName"].as_str()?.to_string(), (e["offsetBefore"].as_f64().unwrap_or(CLIP_BEFORE), e["offsetAfter"].as_f64().unwrap_or(CLIP_AFTER)))))
        .collect();
    let off = |n: &str| offs.get(n).copied().unwrap_or((CLIP_BEFORE, CLIP_AFTER));
    let gain = desc["clips"].as_array().and_then(|c| c.first()).and_then(|c| c["audioGain"].as_f64()).unwrap_or(1.0);
    // (video path, start, end, score)
    let mut groups: Vec<(String, f64, f64, f64)> = Vec::new();
    for v in input["videos"].as_array().cloned().unwrap_or_default() {
        let path = v["path"].as_str().unwrap_or("").to_string();
        let dur = ps.iter().find(|p| p["videoPath"] == v["path"]).and_then(|p| p["durationAtCreation"].as_f64()).map(|d| d / 1000.0).unwrap_or(f64::MAX);
        let scores: std::collections::HashMap<String, f64> = v["gameId"].as_str().and_then(super::cues::model).and_then(|m| m["events"].as_array().cloned()).unwrap_or_default().iter()
            .filter_map(|e| Some((e["name"].as_str()?.to_string(), e["eventScore"].as_f64()?)))
            .collect();
        // importance set in the event editor; games without one use bigger-moment weights
        let w_of = |n: &str| scores.get(n).copied().filter(|s| *s != 10.0 || scores.values().any(|x| *x != 10.0)).unwrap_or(weight(n) * 10.0);
        // (time, weight, kills in it: 0 = not a multi-kill, seconds before, seconds after)
        let mut evs: Vec<(f64, f64, usize, f64, f64)> = v["events"]["videoEvents"].as_array().cloned().unwrap_or_default().iter()
            .filter(|e| e["eventName"].as_str().map(|n| use_event(n)).unwrap_or(false))
            .filter_map(|e| {
                let n = e["eventName"].as_str().unwrap_or("");
                let (b, a) = off(n);
                let t = e["startTimeMs"].as_f64()? / 1000.0;
                // where the fight before it started (ammo counter / fighting model), if found
                let b = e["fightStartMs"].as_f64().map(|f| (t - f / 1000.0).max(0.0)).unwrap_or(b);
                Some((t, w_of(n), multi_kills(n), b, a))
            })
            .collect();
        evs.sort_by(|a, b| a.0.total_cmp(&b.0));
        // multi-kills: one clip from the first kill of it to the last (the multi-kill itself);
        // the kills and medals inside it belong to that clip
        let mut multi: Vec<(f64, f64, f64, f64, f64)> = Vec::new(); // (first, last, score, before, after)
        for (i, &(t, w, kills, _, after)) in evs.iter().enumerate() {
            if kills < 2 {
                continue;
            }
            let mut first = t;
            let mut before = evs[i].3;
            let mut moments = 1;
            let mut score = w;
            let mut j = i;
            while j > 0 && moments < kills {
                let (pt, pw, pk, pb, _) = evs[j - 1];
                if first - pt > MULTI_GAP {
                    break;
                }
                // separate kills can be well under a second apart, so counting them uses a
                // tighter window than grouping medals
                if pk == 0 && first - pt > KILL_APART {
                    moments += 1;
                }
                if pk == 0 || first - pt <= KILL_APART {
                    score += pw;
                }
                first = pt;
                before = pb;
                j -= 1;
            }
            multi.push((first, t, score, before, after));
        }
        // a triple kill contains its double kill: overlapping multi-kills become one clip
        multi.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64, f64, f64, f64)> = Vec::new();
        for m in multi {
            match merged.last_mut() {
                Some(l) if m.0 <= l.1 + SAME_MOMENT => {
                    if m.1 >= l.1 {
                        l.4 = m.4;
                    }
                    l.1 = l.1.max(m.1);
                    l.2 = l.2.max(m.2);
                }
                _ => merged.push(m),
            }
        }
        let mut push = |first: f64, last: f64, score: f64, before: f64, after: f64| {
            let a = (first - before).max(0.0);
            let b = (last + after).min(dur - 0.2);
            if b - a > 1.0 {
                groups.push((path.clone(), a, b, score));
            }
        };
        for &(f, l, sc, be, af) in &merged {
            push(f, l, sc, be, af);
        }
        // every other event: its own clip (events at the same moment, e.g. a kill and its medal,
        // are one clip)
        let mut i = 0;
        while i < evs.len() {
            let t = evs[i].0;
            if merged.iter().any(|m| t >= m.0 - SAME_MOMENT && t <= m.1 + SAME_MOMENT) {
                i += 1;
                continue;
            }
            let (mut score, mut before, mut after) = (evs[i].1, evs[i].3, evs[i].4);
            let mut j = i + 1;
            while j < evs.len() && evs[j].0 - t <= SAME_MOMENT {
                score += evs[j].1;
                before = before.max(evs[j].3);
                after = after.max(evs[j].4);
                j += 1;
            }
            push(t, evs[j - 1].0, score, before, after);
            i = j;
        }
    }
    if groups.is_empty() {
        return json!({"clips": []});
    }
    // how long the montage may be: what the montage maker made, else the music's length
    let made: f64 = desc["clips"].as_array().map(|c| c.iter().filter_map(|c| c["montageTimeRange"]["endInSec"].as_f64()).fold(0.0, f64::max)).unwrap_or(0.0);
    let music: f64 = desc["musics"].as_array().map(|m| m.iter().filter_map(|m| m["montageTimeRange"]["endInSec"].as_f64()).fold(0.0, f64::max)).unwrap_or(0.0);
    let mut budget = if made > 0.0 { made.max(music.min(made * 1.5)) } else if music > 0.0 { music } else { 60.0 };
    if let Some(m) = input["maximumMontageDurationSec"].as_f64() { budget = budget.min(m); }
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&x, &y| groups[y].3.total_cmp(&groups[x].3).then(groups[x].1.total_cmp(&groups[y].1)));
    let mut pick = Vec::new();
    let mut total = 0.0;
    for k in order {
        let len = groups[k].2 - groups[k].1;
        if total + len - if pick.is_empty() { 0.0 } else { TRANSITION } > budget && !pick.is_empty() { continue; }
        total += len - if pick.is_empty() { 0.0 } else { TRANSITION };
        pick.push(k);
    }
    pick.sort_by(|&x, &y| groups[x].0.cmp(&groups[y].0).then(groups[x].1.total_cmp(&groups[y].1)));
    // neighbouring highlights can overlap: start the later clip where the earlier one ends
    let mut spans: Vec<(String, f64, f64)> = Vec::new();
    for &k in &pick {
        let (path, mut a, b, _) = groups[k].clone();
        if let Some(prev) = spans.last() {
            if prev.0 == path && a < prev.2 {
                a = prev.2;
            }
        }
        if b - a >= 2.0 {
            spans.push((path, a, b));
        }
    }
    let mut at = 0.0;
    let mut clips = Vec::new();
    for (n, (path, a, b)) in spans.iter().enumerate() {
        if n > 0 { at -= TRANSITION; }
        clips.push(json!({"audioGain": gain, "inputTimeRange": {"beginInSec": a, "endInSec": b}, "montageTimeRange": {"beginInSec": at, "endInSec": at + (b - a)}, "path": path, "playbackSpeed": 1.0}));
        at += b - a;
    }
    let mut d = desc.clone();
    d["clips"] = json!(clips);
    d["transitions"] = json!((1..clips.len()).map(|_| json!({"durationInSec": TRANSITION, "type": "zoom_blur"})).collect::<Vec<_>>());
    d["visualEffects"] = json!([]);
    if let Some(ms) = d["musics"].as_array_mut() {
        for m in ms.iter_mut() {
            let b = m["inputTimeRange"]["beginInSec"].as_f64().unwrap_or(0.0);
            let e = m["inputTimeRange"]["endInSec"].as_f64().unwrap_or(b + at);
            let len = (e - b).min(at);
            m["inputTimeRange"]["endInSec"] = json!(b + len);
            m["montageTimeRange"] = json!({"beginInSec": 0.0, "endInSec": len});
        }
    }
    d
}

fn videos_and_events(ps: &[Value]) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let (mut main, mut sec, mut videos) = (vec![], vec![], vec![]);
    for p in ps {
        let (Some(path), raw) = (p["videoPath"].as_str(), &p["rawResult"]) else { continue };
        if raw.is_null() {
            continue;
        }
        let game = p["powderGameId"].as_str().unwrap_or("<< no game >>");
        events_for(game, &mut main, &mut sec);
        let arr = |k: &str| raw.get(k).cloned().filter(|v| v.is_array()).unwrap_or(json!([]));
        let dur_ms = p["durationAtCreation"].as_f64().or_else(|| p["endSec"].as_f64().map(|s| s * 1000.0)).unwrap_or(0.0);
        let after_ms = main.iter().chain(sec.iter()).filter_map(|e| e["offsetAfter"].as_f64()).fold(0.0, f64::max) * 1000.0;
        videos.push(json!({"path": path, "gameId": game, "events": {"videoEvents": inside_video(arr("videoEvents"), dur_ms, after_ms), "audioEvents": arr("audioEvents"), "chatEvents": arr("chatEvents"), "communityClipsEvents": arr("communityClipsEvents"), "keywordEvents": []}}));
    }
    (main, sec, videos)
}

fn stamp() -> String {
    // yyyy.MM.dd HH.mm.ss.SSS (local time is not available without a tz database; UTC is used)
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let i = iso(ms);
    format!("{}.{}.{} {}.{}.{}.{}", &i[0..4], &i[5..7], &i[8..10], &i[11..13], &i[14..16], &i[17..19], &i[20..23])
}

pub fn output_path() -> PathBuf {
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("PowderAutoMontage{}.mp4", stamp()))
}

/// generateAutoMontageInput. Optional values are left out (not null) - the native side reads them as numbers.
pub fn montage_input(ps: &[Value], musics: &[String], ms: Option<f64>, me: Option<f64>, max_dur: Option<f64>, balance: &str, order: &str, share_x: bool) -> Value {
    let (main, sec, videos) = videos_and_events(ps);
    let music: Vec<Value> = musics
        .iter()
        .map(|p| {
            let mut m = json!({"path": p});
            if let Some(b) = ms {
                m["beginInSec"] = json!(b);
            }
            if let Some(e) = me {
                m["endInSec"] = json!(e);
            }
            m
        })
        .collect();
    let mut o = json!({
        "outputPath": output_path(), "mainEvents": main, "secondaryEvents": sec, "videos": videos,
        "musics": music, "audioBalance": balance, "sortOrder": order, "shareOnX": share_x,
    });
    // custom montage length: the interface's value, else the user's own choice from the length box
    if let Some(d) = max_dur.or_else(custom_length).filter(|d| *d > 0.0) {
        o["maximumMontageDurationSec"] = json!(d);
    }
    o
}

/// Montage length chosen in the added "Montage length" box (seconds), stored in Feature Storage.
pub fn custom_length() -> Option<f64> {
    let p = super::user_data().join("Feature Storage").join("powder-rs-montage-length.json");
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["seconds"].as_f64())
}

#[allow(dead_code)] // kept for upcoming features
pub fn set_custom_length(sec: Option<f64>) {
    let p = super::user_data().join("Feature Storage").join("powder-rs-montage-length.json");
    let _ = super::core::write_json(&p, &json!({"seconds": sec}));
}

/// formatAutoMontageDescriptionWithExportParameters
pub fn with_export_params(mut d: Value, fps: f64, height: f64, outro: bool, vertical: bool, ps: &[Value]) -> Value {
    if fps != -1.0 {
        d["output_framerate"] = json!(fps);
    }
    let w = d["output_resolution"]["width"].as_f64().unwrap_or(1920.0);
    let h = d["output_resolution"]["height"].as_f64().unwrap_or(1080.0);
    if height != -1.0 {
        d["output_resolution"]["width"] = json!(w / h * height);
        d["output_resolution"]["height"] = json!(height);
    }
    // outro: reuse montage::to_raw's outro choice by building a tiny description
    d["outro"] = if outro { montage::outro_path(vertical).map(|p| json!(p)).unwrap_or(Value::Null) } else { Value::Null };
    if vertical {
        let ow = d["output_resolution"]["width"].as_f64().unwrap_or(1920.0);
        let oh = d["output_resolution"]["height"].as_f64().unwrap_or(1080.0);
        let templates = host().get("vertical-format", "templatesById").unwrap_or(json!({}));
        let default = templates["default"]["crops"].clone();
        let mut croppings = vec![];
        let clips: Vec<Value> = d["clips"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut c| {
                let p = ps.iter().find(|p| p["videoPath"] == c["path"]);
                let gid = p.and_then(|p| p["game"]["powderGameId"].as_str()).unwrap_or("");
                let t = p.map(|p| p["verticalTemplate"].clone()).filter(|t| t.is_array()).or_else(|| Some(templates[gid]["crops"].clone()).filter(|t| t.is_array())).or_else(|| Some(default.clone()).filter(|t| t.is_array())).unwrap_or_else(|| serde_json::from_str(r#"[{"in":[0.34197530864,0,0.31604938271,1],"out":[0,0,1,1],"locked":false,"cover":true}]"#).unwrap());
                let crop = montage::adapt_crops(ow, oh, &t);
                croppings.push(crop.clone());
                c["croppings"] = json!([crop]);
                c
            })
            .collect();
        d["output_resolution"]["width"] = json!(2.0 * (oh * 9.0 / 16.0 / 2.0).round());
        d["clips"] = json!(clips);
        d["croppings"] = json!(croppings);
    }
    if d["outro"].is_null() {
        if let Some(o) = d.as_object_mut() {
            o.remove("outro");
        }
    }
    d
}

/// formatAutoMontageConfigForPlayer
pub fn for_player(d: &Value, vertical: bool, input: &Value, music_title: &str, title: Option<&str>) -> Value {
    let mut o = d.clone();
    let m = d["musics"].get(0).cloned();
    o["musics"] = match m {
        Some(mut m) => {
            m["title"] = json!(music_title);
            json!([m])
        }
        None => json!([]),
    };
    o["format"] = json!(if vertical { "vertical" } else { "horizontal" });
    o["videos"] = input.get("videos").cloned().unwrap_or(json!([]));
    o["title"] = title.map(|t| json!(t)).unwrap_or(Value::Null);
    o
}

fn current() -> &'static Mutex<Option<Arc<Job>>> {
    static C: OnceLock<Mutex<Option<Arc<Job>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

fn set_proc_state(patch: Value) {
    let cur = host().get(B, "automontageProcessState").unwrap_or(json!({"processing": false, "progress": 0, "videosIds": []}));
    host().set(B, "automontageProcessState", projects::merge(cur, patch));
}

/// After a montage file is written: tag it, add it to the Auto Montages list.
pub fn add_generated(path: &Path, music_path: &str) -> Result<Value> {
    let vm = crate::media::metadata(path)?;
    let music_id = super::music::list().iter().find(|m| m["path"] == music_path).map(|m| m["id"].clone()).unwrap_or(Value::Null);
    let meta = json!({"id": montage::uuid(), "video": vm.to_json(), "kind": "AUTO_MONTAGE", "musicId": music_id});
    if let Err(e) = super::super::mp4meta::write(path, &meta) {
        crate::log(&format!("metadata block: {e:#}"));
    }
    register(path, true);
    publish();
    Ok(meta)
}

fn generate(o: &Value) -> Result<Value> {
    let h = host();
    if current().lock().is_some() || montage::is_processing(h) {
        bail!("MontageMaker: montage already running");
    }
    let ids: Vec<String> = o["extractionProjectsIds"].as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let ps: Vec<Value> = ids.iter().filter_map(|id| projects::get().get_by_id(id)).collect();
    if ps.is_empty() {
        bail!("Error: Missing recording video file");
    }
    montage::set_state(h, json!({"processing": true, "progress": 0, "videosIds": ids}));
    set_proc_state(json!({"processing": true, "progress": 0, "videosIds": ids}));
    let vertical = o["isVertical"] == true;
    let musics: Vec<String> = o["musicsPaths"].as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let loudness = ps[0]["rawResult"]["loudnessData"].clone();
    let r = (|| -> Result<Value> {
        let input = if !o["silenceRemovalInput"].is_null() && !loudness.is_null() {
            json!({"outputPath": output_path(), "videoPath": ps[0]["videoPath"], "threshold": o["silenceRemovalInput"], "loudnessData": loudness})
        } else {
            montage_input(&ps, &musics, o["musicStartSec"].as_f64(), o["musicEndSec"].as_f64(), o["maximumMontageDurationSec"].as_f64(), o["audioBalance"].as_str().unwrap_or("balanced"), o["sortOrder"].as_str().unwrap_or("chronological"), o["shareOnX"] == true)
        };
        let run = |input: &Value| -> Result<Value> {
            let job = Job::start("auto_montage_maker", vec![], input.clone(), "auto_montage_progress", "auto_montage_finished", Accel::from_settings(), |p| {
                set_proc_state(json!({"progress": p["progress"].as_f64().unwrap_or(0.0) / 2.0}));
            })?;
            *current().lock() = Some(job.clone());
            let desc = job.wait();
            *current().lock() = None;
            desc
        };
        let summary = |i: &Value| {
            let list = |k: &str| i[k].as_array().map(|a| a.iter().map(|e| format!("{}({}/{})", e["eventName"].as_str().unwrap_or("?"), e["offsetBefore"], e["offsetAfter"])).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            let n: usize = i["videos"].as_array().map(|v| v.iter().map(|v| v["events"]["videoEvents"].as_array().map(|a| a.len()).unwrap_or(0)).sum()).unwrap_or(0);
            format!("{n} video events; main: {}; secondary: {}", list("mainEvents"), list("secondaryEvents"))
        };
        crate::log(&format!("automontage input: {}", summary(&input)));
        let empty = |d: &Value| d["clips"].as_array().map(|c| c.is_empty()).unwrap_or(false);
        let mut desc = run(&input)?;
        if empty(&desc) && input["mainEvents"].is_array() {
            // nothing picked: try again with every event in the videos as a highlight and short
            // clip times, which is how Powder's own games are set up
            let mut names: Vec<String> = Vec::new();
            for v in input["videos"].as_array().cloned().unwrap_or_default() {
                for e in v["events"]["videoEvents"].as_array().cloned().unwrap_or_default() {
                    if let Some(n) = e["eventName"].as_str() {
                        if !names.iter().any(|x| x == n) && !n.to_lowercase().contains("death") {
                            names.push(n.to_string());
                        }
                    }
                }
            }
            let mut retry = input.clone();
            retry["mainEvents"] = json!(names.iter().map(|n| json!({"eventName": n, "offsetBefore": 2, "offsetAfter": 2, "effectTypes": []})).collect::<Vec<_>>());
            retry["secondaryEvents"] = json!([]);
            crate::log(&format!("automontage picked nothing, trying again: {}", summary(&retry)));
            desc = run(&retry)?;
        }
        // games added with Rewind: one short clip per highlight (Powder's picker chains every
        // nearby event into one long clip)
        if ps.iter().any(|p| p["powderGameId"].as_str().map(|g| !g.is_empty() && super::community::games_dir().join(g).join("game_postprocess.lua").exists()).unwrap_or(false)) {
            let own = per_event_clips(&desc, &input, &ps);
            if !empty(&own) {
                crate::log(&format!("automontage: {} clip(s), one per highlight", own["clips"].as_array().map(|c| c.len()).unwrap_or(0)));
                desc = own;
            }
        }
        // an empty montage crashes the montage maker ("native error signal"); say why instead
        if empty(&desc) {
            bail!("No highlights could be picked for this montage, even with every event as a highlight. Please send the log file (powder-rs.log) so this can be fixed.");
        }
        let desc = with_export_params(desc, o["exportFramerate"].as_f64().unwrap_or(-1.0), o["exportHeight"].as_f64().unwrap_or(-1.0), o["exportOutro"] == true, vertical, &ps);
        let out = input["outputPath"].as_str().context("Output path is not defined in AutoMontageProcessInput")?.to_string();
        let cfg = for_player(&desc, vertical, &input, o["musicTitle"].as_str().unwrap_or(""), None);
        let _ = super::core::write_json(&config_path(&out), &cfg);
        montage::set_state(h, json!({"processing": false}));
        montage::launch(h, &desc, |p| set_proc_state(json!({"progress": 0.5 + p * 0.5})))?;
        add_generated(Path::new(&out), musics.first().map(|s| s.as_str()).unwrap_or(""))?;
        Ok(json!(out))
    })();
    set_proc_state(json!({"processing": false, "progress": 0, "videosIds": []}));
    montage::set_state(h, json!({"processing": false, "progress": 0, "videosIds": []}));
    match r {
        Err(e) if is_abort(&e) => Ok(Value::Null),
        r => r,
    }
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    h.set(B, "normalizedAutoMontage", json!([]));
    h.set(B, "autoMontagesByIds", json!({}));
    set_proc_state(json!({"processing": false, "progress": 0, "videosIds": []}));
    std::thread::spawn(scan);
    h.handle(B, "loadAutoMontageConfig", |_, a| Ok(std::fs::read_to_string(config_path(a.first().and_then(|v| v.as_str()).unwrap_or(""))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)));
    h.handle(B, "markAutoMontageAsWatched", |_, a| {
        if let Some(m) = by_id().lock().get_mut(a.first().and_then(|v| v.as_str()).unwrap_or("")) {
            m["unwatch"] = json!(false);
        }
        publish();
        Ok(Value::Null)
    });
    h.handle(B, "renameAutoMontage", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let id = o["id"].as_str().unwrap_or("").to_string();
        let m = by_id().lock().get(&id).cloned();
        if let Some(mut m) = m {
            let mut meta = m["powderMetadata"].clone();
            meta["title"] = o["newTitle"].clone();
            super::super::mp4meta::write(Path::new(m["path"].as_str().unwrap_or("")), &meta)?;
            m["title"] = o["newTitle"].clone();
            m["powderMetadata"] = meta;
            by_id().lock().insert(id, m);
            publish();
        }
        Ok(Value::Null)
    });
    h.handle(B, "revealInFolderById", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let m = by_id().lock().get(id).cloned().with_context(|| format!("Auto montage with id {id} not found"))?;
        super::core::reveal(m["path"].as_str().unwrap_or(""));
        Ok(Value::Null)
    });
    h.handle(B, "deleteAutoMontageById", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let m = by_id().lock().remove(&id);
        if let Some(m) = m {
            crate::gpu::trash(Path::new(m["path"].as_str().unwrap_or("")))?;
        }
        publish();
        Ok(Value::Null)
    });
    h.handle(B, "getAutomontageCandidates", |_, _| {
        let mut v: Vec<Value> = projects::get()
            .all()
            .into_iter()
            .filter(|p| p["automontageEventsCount"].as_u64().unwrap_or(0) > 0 && p["missingVideoFile"] != true)
            .map(|p| json!({"id": p["id"], "title": p["title"], "posterUrl": p["posterUrl"].as_str().unwrap_or(""), "source": p["source"], "createdAt": p["createdAt"], "automontageEventsCount": p["automontageEventsCount"]}))
            .collect();
        v.sort_by(|a, b| b["createdAt"].as_str().unwrap_or("").cmp(a["createdAt"].as_str().unwrap_or("")));
        Ok(Value::Array(v))
    });
    h.handle(B, "estimateAutoMontageLength", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let ps: Vec<Value> = o["extractionProjectsIds"].as_array().cloned().unwrap_or_default().iter().filter_map(|id| projects::get().get_by_id(id.as_str().unwrap_or(""))).collect();
        if ps.is_empty() {
            bail!("Error: Missing recording video file");
        }
        let (main, sec, videos) = videos_and_events(&ps);
        let input = json!({"mainEvents": main, "secondaryEvents": sec, "videos": videos, "musics": [], "shareOnX": false});
        // AutoMontageEstimateProcess: `estimate_duration` instead of `start`
        let (tx, rx) = std::sync::mpsc::channel();
        let tx = Mutex::new(tx);
        let p = crate::native::NativeProcess::start("auto_montage_maker", vec![], Accel::from_settings(), Box::new(move |n, d| {
            if n == "estimation_finished" || n.ends_with("ERROR_JSON") || n == "connection-closed" {
                let _ = tx.lock().send((n.to_string(), d.clone()));
            }
        }))?;
        let _ = p.push("estimate_duration", Some(&input));
        let r = rx.recv();
        p.kill();
        match r {
            Ok((n, d)) if n == "estimation_finished" => Ok(d),
            Ok((n, d)) => bail!("{n} {d}"),
            Err(_) => bail!("estimate failed"),
        }
    });
    h.handle(B, "generateAutoMontage", |_, a| generate(&a.into_iter().next().unwrap_or(json!({}))));
    h.handle(B, "stopAutomontageCreation", |_, _| {
        if let Some(j) = current().lock().as_ref() {
            j.stop();
            return Ok(Value::Null);
        }
        montage::stop();
        Ok(Value::Null)
    });
    let _: Option<Map<String, Value>> = None;
}
