//! Edit a game's events after Add Game: a scan only sees the events that happened in that one
//! video, so people remove wrong ones, fix the on-screen words and add events the video missed.
//!
//! A game pack (engine\games\<ID>) keeps detection in game_postprocess.lua:
//!   ocrConfig.crops  - screen areas that are read
//!   DETECTORS        - one line per (event, area, words)
//!   EVENT_MERGE      - per event: seconds within which repeats count as one
//! and the event list Powder shows in events.json. The editor works on DETECTORS lines and keeps
//! everything else in the file as it is; saving reinstalls the game.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

const ANYWHERE: &str = "Anywhere";

fn pack_dir(id: &str) -> Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("unknown game");
    }
    let d = super::community::games_dir().join(id);
    if !d.join("game_postprocess.lua").exists() {
        bail!("{} isn't a game added with Rewind (no game_postprocess.lua)", d.display());
    }
    Ok(d)
}

/// The text between `local NAME = {` and the matching `\n}` at the start of a line.
fn block(src: &str, name: &str) -> Option<(usize, usize)> {
    let head = format!("local {name} = {{");
    let s = src.find(&head)? + head.len();
    let e = s + src[s..].find("\n}")?;
    Some((s, e))
}

fn quoted(s: &str) -> Vec<String> {
    s.split('"').skip(1).step_by(2).map(|x| x.to_string()).collect()
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(&format!("{key} = "))? + key.len() + 3;
    let rest = &line[i..];
    if let Some(r) = rest.strip_prefix('"') {
        return r.split('"').next();
    }
    Some(rest.split([',', '}']).next()?.trim())
}

#[derive(Clone)]
struct Det {
    event: String,
    crop: String,
    words: Vec<String>,
    score: String,
    visible: String,
    offset: String,
    exclude: Vec<String>,
}

fn parse_detectors(src: &str) -> Vec<Det> {
    let Some((s, e)) = block(src, "DETECTORS") else { return vec![] };
    src[s..e]
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{') && l.contains("event ="))
        .filter_map(|l| {
            let m = l.find("match = {")?;
            let me = m + l[m..].find('}')?;
            Some(Det {
                event: field(l, "event")?.to_string(),
                crop: field(l, "crop")?.to_string(),
                words: quoted(&l[m + 9..me]),
                score: field(&l[me..], "score").unwrap_or("80").to_string(),
                visible: field(&l[me..], "visible").unwrap_or("0.3").to_string(),
                offset: field(&l[me..], "offset").unwrap_or("0").to_string(),
                exclude: l[me..].find("exclude = {").and_then(|x| { let a = me + x + 11; l[a..].find('}').map(|b| quoted(&l[a..a + b])) }).unwrap_or_default(),
            })
        })
        .collect()
}

fn crop_names(src: &str) -> Vec<String> {
    let Some((s, e)) = block(src, "ocrConfig") else { return vec![] };
    src[s..e].lines().filter_map(|l| field(l.trim(), "cropName").map(String::from)).collect()
}

/// A crop that already reads the whole screen (Add Game often makes one), reused for "Anywhere".
fn full_screen_crop(src: &str) -> Option<String> {
    let (s, e) = block(src, "ocrConfig")?;
    let mut name = None;
    for l in src[s..e].lines() {
        let l = l.trim();
        if let Some(n) = field(l, "cropName") {
            name = Some(n.to_string());
        }
        if let Some(i) = l.find("cropCoords = {") {
            let nums: Vec<f32> = l[i + 14..].split(['}', ',']).filter_map(|x| x.trim().parse().ok()).collect();
            if nums.len() >= 4 && nums[0] <= 0.01 && nums[1] <= 0.01 && nums[2] >= 0.99 && nums[3] >= 0.99 {
                return name;
            }
        }
    }
    None
}

/// For the editor: the game's detectors, the screen areas and the event names it shows.
pub fn load(id: &str) -> Value {
    match load_inner(id) {
        Ok(v) => v,
        Err(e) => json!({"error": format!("{e:#}")}),
    }
}

fn load_inner(id: &str) -> Result<Value> {
    let dir = pack_dir(id)?;
    let src = std::fs::read_to_string(dir.join("game_postprocess.lua"))?;
    let events: Value = serde_json::from_slice(&std::fs::read(dir.join("events.json")).unwrap_or_default()).unwrap_or(json!({}));
    let display = |ev: &str| {
        events["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == ev)).and_then(|e| e["displayName"].as_str().map(String::from)).unwrap_or_else(|| ev.to_string())
    };
    let name = std::fs::read(dir.join("game.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["name"].as_str().map(String::from)).unwrap_or_else(|| id.to_string());
    let rows: Vec<Value> = parse_detectors(&src).into_iter().map(|d| json!({"event": d.event, "displayName": display(&d.event), "area": d.crop, "words": d.words})).collect();
    let used: Vec<String> = parse_detectors(&src).into_iter().flat_map(|d| d.words).collect();
    let suggestions: Vec<Value> = std::fs::read(dir.join("suggestions.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s["word"].as_str().map(|w| !used.iter().any(|u| u == w)).unwrap_or(false))
        .collect();
    let mut areas = crop_names(&src);
    let full = full_screen_crop(&src);
    if full.is_none() && !areas.iter().any(|a| a == ANYWHERE) {
        areas.push(ANYWHERE.into());
    }
    Ok(json!({"id": id, "name": name, "rows": rows, "areas": areas, "suggestions": suggestions, "fullScreen": full, "edited": dir.join(".original").exists(), "folder": dir.display().to_string()}))
}

fn event_name(display: &str) -> String {
    let mut out = String::new();
    for (i, w) in display.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).enumerate() {
        let w = w.to_lowercase();
        if i == 0 {
            out += &w;
        } else {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                out.push(f.to_ascii_uppercase());
                out.extend(c);
            }
        }
    }
    out
}

/// Save the edited rows: rewrite DETECTORS / EVENT_MERGE / events.json, then reinstall the game.
pub fn save(id: &str, rows: &Value) -> Value {
    match save_inner(id, rows) {
        Ok(n) => json!({"ok": true, "events": n}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

fn save_inner(id: &str, rows: &Value) -> Result<usize> {
    let dir = pack_dir(id)?;
    let lua_p = dir.join("game_postprocess.lua");
    let mut src = std::fs::read_to_string(&lua_p)?;
    let old = parse_detectors(&src);
    let rows = rows.as_array().context("no rows")?;
    // keep a backup of the version before the first edit
    let bak = dir.join(".original");
    if !bak.exists() {
        std::fs::create_dir_all(&bak)?;
        for f in ["game_postprocess.lua", "events.json"] {
            let _ = std::fs::copy(dir.join(f), bak.join(f));
        }
    }

    let mut dets: Vec<(Det, String)> = Vec::new(); // (detector, display name)
    for r in rows {
        let display = r["displayName"].as_str().unwrap_or("").trim().to_string();
        let words: Vec<String> = r["words"].as_array().map(|a| a.iter().filter_map(|w| w.as_str()).map(|w| w.trim().replace('"', "").to_uppercase()).filter(|w| !w.is_empty()).collect()).unwrap_or_default();
        if display.is_empty() || words.is_empty() {
            continue;
        }
        let mut ev = r["event"].as_str().unwrap_or("").to_string();
        if ev.is_empty() {
            ev = event_name(&display);
        }
        if ev.is_empty() {
            bail!("\"{display}\" needs letters or numbers in its name");
        }
        let mut area = r["area"].as_str().unwrap_or(ANYWHERE).to_string();
        if area == ANYWHERE {
            if let Some(f) = full_screen_crop(&src) {
                area = f;
            }
        }
        // keep the tuning of an unchanged detector; new ones get safe defaults
        let prev = old.iter().find(|d| d.event == ev && d.crop == area);
        let longest = words.iter().map(|w| w.chars().count()).max().unwrap_or(4);
        let d = Det {
            event: ev,
            crop: area,
            words,
            score: prev.map(|d| d.score.clone()).unwrap_or_else(|| if longest <= 4 { "85".into() } else { "78".into() }),
            visible: prev.map(|d| d.visible.clone()).unwrap_or_else(|| "0.3".into()),
            offset: prev.map(|d| d.offset.clone()).unwrap_or_else(|| "0".into()),
            exclude: prev.map(|d| d.exclude.clone()).unwrap_or_default(),
        };
        dets.push((d, display));
    }
    if dets.is_empty() {
        bail!("keep at least one event");
    }

    // a full-screen area when something is set to "Anywhere"
    if dets.iter().any(|(d, _)| d.crop == ANYWHERE) && !crop_names(&src).iter().any(|c| c == ANYWHERE) {
        let (s, e) = block(&src, "ocrConfig").context("ocrConfig not found in game_postprocess.lua")?;
        let crops_end = s + src[s..e].rfind("\n    }").context("crops list not found")?;
        let entry = "\n        {\n            cropName = \"Anywhere\",\n            debug = false,\n            cropCoords = { 0.000, 0.000, 1.000, 1.000 },\n            detectorDilateDiameter = 5,\n            detectorMinimumArea = 40,\n            detectorMargin = 8,\n            recogniserStretchVertical = false,\n            restrictedCharacters = \"\"\n        },";
        let before = src[..crops_end].trim_end_matches([' ', '\n']).to_string();
        let sep = if before.ends_with(',') || before.ends_with('{') { "" } else { "," };
        src = format!("{before}{sep}{entry}{}", &src[crops_end..]);
    }

    // DETECTORS
    let all_words: Vec<Vec<String>> = dets.iter().flat_map(|(d, _)| d.words.iter().map(|w| tokens(w))).collect();
    let lines: String = dets
        .iter()
        .map(|(d, _)| {
            let q: Vec<String> = d.words.iter().map(|w| format!("\"{w}\"")).collect();
            let longest = d.words.iter().map(|w| w.chars().count()).max().unwrap_or(4);
            // other events' longer phrases that contain this one's word ("DOUBLE KILL" for KILL)
            let own: Vec<Vec<String>> = d.words.iter().map(|w| tokens(w)).collect();
            let mut ex: Vec<String> = d.exclude.iter().filter(|x| !own.contains(&tokens(x))).cloned().collect();
            for c in &all_words {
                if own.contains(c) { continue; }
                if own.iter().any(|wt| !wt.is_empty() && c.len() > wt.len() && c.windows(wt.len()).any(|w| w == wt.as_slice())) {
                    let j = c.join(" ");
                    if !ex.contains(&j) { ex.push(j); }
                }
            }
            let exq: Vec<String> = ex.iter().map(|w| format!("\"{}\"", w.replace('"', ""))).collect();
            format!("    {{ event = \"{}\", crop = \"{}\", match = {{ {} }}, exclude = {{ {} }}, score = {}, visible = {}, offset = {}, priority = {} }},\n", d.event, d.crop, q.join(", "), exq.join(", "), d.score, d.visible, d.offset, longest)
        })
        .collect();
    let (s, e) = block(&src, "DETECTORS").context("DETECTORS not found in game_postprocess.lua")?;
    src = format!("{}\n{}{}", &src[..s], lines.trim_end_matches('\n'), &src[e..]);

    // EVENT_MERGE: keep existing values, add new events
    let mut names: Vec<String> = Vec::new();
    for (d, _) in &dets {
        if !names.contains(&d.event) {
            names.push(d.event.clone());
        }
    }
    if let Some((s, e)) = block(&src, "EVENT_MERGE") {
        let existing: Vec<(String, String)> = src[s..e].lines().filter_map(|l| l.trim().split_once(" = ").map(|(k, v)| (k.trim().to_string(), v.trim().trim_end_matches(',').to_string()))).collect();
        let body: String = names
            .iter()
            .map(|n| {
                let v = existing.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap_or_else(|| {
                    let l = n.to_lowercase();
                    if ["victory", "defeat", "win", "lose"].iter().any(|k| l.contains(k)) { "10.0".into() } else { "2.0".into() }
                });
                format!("    {n} = {v},\n")
            })
            .collect();
        src = format!("{}\n{}{}", &src[..s], body.trim_end_matches('\n'), &src[e..]);
    }
    let src = upgrade_matcher(&src);
    std::fs::write(&lua_p, src)?;

    // events.json: keep the existing entries (icons, scores, montage settings), add new ones
    let ev_p = dir.join("events.json");
    let mut evj: Value = serde_json::from_slice(&std::fs::read(&ev_p).unwrap_or_default()).unwrap_or(json!({"name": id}));
    let old_events = evj["events"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for n in &names {
        let display = dets.iter().find(|(d, _)| &d.event == n).map(|(_, dn)| dn.clone()).unwrap_or_else(|| n.clone());
        let mut e = old_events.iter().find(|e| e["name"] == n.as_str()).cloned().unwrap_or_else(|| {
            let l = n.to_lowercase();
            let end = ["victory", "defeat", "win", "lose"].iter().any(|k| l.contains(k));
            json!({"name": n, "icon": if end { "overall-victory" } else { "generic-event" }, "default": true, "eventScore": 10,
                   "automontage": {"primary": !end, "secondary": end, "offsetBefore": 2, "offsetAfter": 2, "effectTypes": []}})
        });
        let words: Vec<String> = dets.iter().filter(|(d, _)| &d.event == n).flat_map(|(d, _)| d.words.clone()).collect();
        e["displayName"] = json!(display);
        if e["automontage"]["custom"] != true {
            e["automontage"] = clip_policy(n, &display);
        }
        e["instruction"] = json!(format!("* {display} *"));
        e["tooltip"] = json!(format!("When {} appears on screen.", words.join(" / ")));
        out.push(e);
    }
    evj["events"] = json!(out);
    std::fs::write(&ev_p, serde_json::to_string_pretty(&evj)?)?;

    // remember what the person changed, so improving the game with another video keeps it
    let baseline: Vec<String> = std::fs::read_to_string(bak.join("game_postprocess.lua")).map(|s| parse_detectors(&s).into_iter().map(|d| d.event).collect()).unwrap_or_default();
    let rows_json: Vec<Value> = dets.iter().map(|(d, dn)| json!({"event": d.event, "displayName": dn, "area": d.crop, "words": d.words})).collect();
    let _ = std::fs::write(dir.join("my-edits.json"), serde_json::to_vec_pretty(&json!({"baseline": baseline, "rows": rows_json}))?);

    reinstall(id)?;
    Ok(names.len())
}

/// After a game is rebuilt from more videos: apply the person's earlier edits on top of the new
/// automatic version (events they removed stay removed, their rows replace the automatic ones).
pub fn reapply(id: &str) -> Result<bool> {
    let dir = pack_dir(id)?;
    let Ok(b) = std::fs::read(dir.join("my-edits.json")) else { return Ok(false) };
    let edits: Value = serde_json::from_slice(&b)?;
    let rows = edits["rows"].as_array().cloned().unwrap_or_default();
    let baseline: Vec<String> = edits["baseline"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mine: Vec<String> = rows.iter().filter_map(|r| r["event"].as_str().map(String::from)).collect();
    let removed: Vec<&String> = baseline.iter().filter(|e| !mine.contains(e)).collect();
    let src = std::fs::read_to_string(dir.join("game_postprocess.lua"))?;
    let events: Value = serde_json::from_slice(&std::fs::read(dir.join("events.json")).unwrap_or_default()).unwrap_or(json!({}));
    let display = |ev: &str| events["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == ev)).and_then(|e| e["displayName"].as_str().map(String::from)).unwrap_or_else(|| ev.to_string());
    let mut out = rows.clone();
    for d in parse_detectors(&src) {
        if removed.contains(&&d.event) || mine.contains(&d.event) {
            continue;
        }
        out.push(json!({"event": d.event, "displayName": display(&d.event), "area": d.crop, "words": d.words}));
    }
    // the rebuilt automatic version becomes the new "undo" point
    let bak = dir.join(".original");
    let _ = std::fs::remove_dir_all(&bak);
    save_inner(id, &Value::Array(out))?;
    Ok(true)
}

/// Put back the version Add Game made.
pub fn reset(id: &str) -> Value {
    let r = (|| -> Result<()> {
        let dir = pack_dir(id)?;
        let bak = dir.join(".original");
        if !bak.exists() {
            bail!("there are no edits to undo");
        }
        for f in ["game_postprocess.lua", "events.json"] {
            std::fs::copy(bak.join(f), dir.join(f))?;
        }
        reinstall(id)
    })();
    match r {
        Ok(()) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

fn reinstall(id: &str) -> Result<()> {
    let eng = super::customgame::engine().context("the game engine is missing")?;
    let root = super::ctx().dist.root.clone();
    let mut c = std::process::Command::new(&eng);
    c.current_dir(eng.parent().unwrap()).args(["powder", "install", id, "--powder"]).arg(&root);
    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
    let out = c.output()?;
    if !out.status.success() {
        bail!("reinstalling the game failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let ctx = super::ctx();
    super::games::refresh(&ctx.host, ctx);
    Ok(())
}

/// Games that can be edited: everything in the engine's games folder.
pub fn list() -> Value {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(super::community::games_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.join("game_postprocess.lua").exists() {
                continue;
            }
            let id = e.file_name().to_string_lossy().to_string();
            let name = std::fs::read(p.join("game.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["name"].as_str().map(String::from)).unwrap_or_else(|| id.clone());
            v.push(json!({"id": id, "name": name}));
        }
    }
    v.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!(v)
}

/// Open the game's folder (events.json + game_postprocess.lua) in Explorer for hand editing.
/// Model Creation's training pictures (engine\\datasets, next to engine\\games)
pub fn datasets_dir() -> PathBuf {
    let g = super::community::games_dir();
    g.parent().map(|p| p.to_path_buf()).unwrap_or(g).join("datasets")
}

pub fn open_folder(id: &str) -> Value {
    match pack_dir(id) {
        Ok(d) => {
            super::core::open_path(&d.display().to_string());
            json!({"ok": true})
        }
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

/// Save the Add Game log (plus the engine's report) as a text file people can attach to a bug report.
pub fn export_log(id: Option<&str>, log: &[String]) -> Value {
    let r = (|| -> Result<PathBuf> {
        let out_dir = dirs::download_dir().or_else(dirs::desktop_dir).unwrap_or_else(std::env::temp_dir);
        let stamp = super::core::now_iso().replace([':', '.'], "-");
        let p = out_dir.join(format!("Rewind-add-game-log-{}-{}.txt", id.unwrap_or("game"), &stamp[..stamp.len().min(19)]));
        let mut s = format!("Rewind {} add-game log\nWindows: {}\nTime: {}\n\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, super::core::now_iso());
        s += &log.join("\n");
        if let Some(id) = id {
            let rep = super::community::games_dir().join("_work").join(id).join("add-game-report.json");
            if let Ok(r) = std::fs::read_to_string(rep) {
                s += "\n\n--- add-game-report.json ---\n";
                s += &r;
            }
            if let Ok(l) = std::fs::read_to_string(super::community::games_dir().join(id).join("game_postprocess.lua")) {
                if let Some((a, b)) = block(&l, "DETECTORS") {
                    s += "\n\n--- detectors ---";
                    s += &l[a..b];
                }
            }
        }
        std::fs::write(&p, s)?;
        super::core::reveal(&p.display().to_string());
        Ok(p)
    })();
    match r {
        Ok(p) => json!({"ok": true, "path": p.display().to_string()}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

/// How an event is used in auto-montages: kills, medals, streaks and the like are the highlights
/// (clip from ~7 s before the first to ~7 s after the last; events this close together join into
/// one clip). Wins/assists only fill in, deaths are never highlights.
pub fn clip_policy(name: &str, display: &str) -> serde_json::Value {
    let l = format!("{} {}", name, display).to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| l.contains(k));
    let (primary, secondary, before, after) = if has(&["victory", "defeat", "win", "lose", "result"]) {
        (false, true, 3, 5)
    } else if has(&["death", "killed by", "killedby", "eliminated by", "downed", "respawn", "spectat"]) {
        (false, false, 2, 2)
    } else if has(&["assist", "level", "xp", "others", "objective", "capture"]) {
        (false, true, 5, 5)
    } else {
        (true, false, 7, 7)
    };
    serde_json::json!({"primary": primary, "secondary": secondary, "offsetBefore": before, "offsetAfter": after, "effectTypes": []})
}

fn tokens(s: &str) -> Vec<String> {
    s.to_uppercase().split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit())).map(String::from).collect()
}

/// Games made before whole-word matching: switch their detector loop to it ("KILL" no longer
/// fires on "KILL CONFIRMED", "KILLS" or a player called MrsKiller).
fn upgrade_matcher(src: &str) -> String {
    const OLD_IF: &str = "paddle_ocr.checkValues(text, d.match, d.score)";
    const OLD_FUT: &str = "paddle_ocr.checkFuture(frameIndex, need - 1, cropName, d.match, d.score)";
    if src.contains("local function rwMatch") || !src.contains(OLD_IF) {
        return src.to_string();
    }
    let mut s = src.replace(OLD_IF, "rwMatch(text, d)").replace(OLD_FUT, "rwMatchFuture(cues, frameIndex, need - 1, cropName, d)");
    let lua = include_str!("rwmatch.lua");
    let at = s.find("local function frames(").or_else(|| s.find("local get_fps")).unwrap_or(0);
    s.insert_str(at, &format!("{lua}\n"));
    s
}
