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

/// Every screen area with its rectangle.
fn crops_with_coords(src: &str) -> Vec<(String, [f32; 4])> {
    let Some((s, e)) = block(src, "ocrConfig") else { return vec![] };
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for l in src[s..e].lines() {
        let l = l.trim();
        if let Some(n) = field(l, "cropName") {
            name = Some(n.to_string());
        }
        if let (Some(i), Some(n)) = (l.find("cropCoords = {"), name.as_ref()) {
            let v: Vec<f32> = l[i + 14..].split(['}', ',']).filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() >= 4 {
                out.push((n.clone(), [v[0], v[1], v[2], v[3]]));
            }
        }
    }
    out
}

fn same_rect(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01)
}

/// Add a screen area to the game's crops list.
fn add_crop(src: &str, name: &str, c: &[f32; 4]) -> Result<String> {
    let (s, e) = block(src, "ocrConfig").context("ocrConfig not found in game_postprocess.lua")?;
    let crops_end = s + src[s..e].rfind("\n    }").context("crops list not found")?;
    let entry = format!("\n        {{\n            cropName = \"{name}\",\n            debug = false,\n            cropCoords = {{ {:.3}, {:.3}, {:.3}, {:.3} }},\n            detectorDilateDiameter = 5,\n            detectorMinimumArea = 40,\n            detectorMargin = 8,\n            recogniserStretchVertical = false,\n            restrictedCharacters = \"\"\n        }},", c[0], c[1], c[2], c[3]);
    let before = src[..crops_end].trim_end_matches([' ', '\n']).to_string();
    let sep = if before.ends_with(',') || before.ends_with('{') { "" } else { "," };
    Ok(format!("{before}{sep}{entry}{}", &src[crops_end..]))
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
    let rects = crops_with_coords(&src);
    let rows: Vec<Value> = parse_detectors(&src)
        .into_iter()
        .map(|d| {
            let mut r = json!({"event": d.event, "displayName": display(&d.event), "area": d.crop, "words": d.words});
            event_settings(&events, &d.event, &mut r);
            if let Some((_, c)) = rects.iter().find(|(n, _)| *n == d.crop) {
                r["areaCoords"] = json!(c);
            }
            r
        })
        .collect();
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

/// Clip time before/after (ms) and importance of an event, from events.json.
fn event_settings(events: &Value, ev: &str, r: &mut Value) {
    let e = events["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == ev));
    let ms = |k: &str| e.and_then(|e| e["automontage"][k].as_f64()).map(|s| (s * 1000.0).round()).unwrap_or(1000.0);
    r["msBefore"] = json!(ms("offsetBefore"));
    r["msAfter"] = json!(ms("offsetAfter"));
    r["importance"] = json!(e.and_then(|e| e["eventScore"].as_f64()).unwrap_or(10.0));
}

/// Another game's events as a starting point. A game added with Rewind gives everything (words,
/// screen areas, clip times, importance); a game Powder supports gives its event names, clip
/// times and importance (its detection is Powder's own AI, so the words are just the names).
pub fn template(id: &str) -> Value {
    if pack_dir(id).is_ok() {
        return load(id);
    }
    let Some(m) = super::cues::model(id) else { return json!({"error": "unknown game"}) };
    // Powder's own text-reading games: their game_postprocess.lua has the words and screen areas
    let lua = super::games::ai_configs(&super::ctx()).map(|a| a.join("visual_cues").join(id).join("game_postprocess.lua")).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let (crops, found) = official_detectors(&lua);
    let name = super::games::find(id).and_then(|g| g["name"].as_str().map(String::from)).unwrap_or_else(|| id.to_string());
    let rows: Vec<Value> = m["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            let ev = e["name"].as_str()?;
            let dn = e["displayName"].as_str().unwrap_or(ev);
            let ms = |k: &str| e["automontage"][k].as_f64().filter(|s| *s > 0.0).map(|s| (s * 1000.0).round()).unwrap_or(1000.0);
            let base = json!({"event": ev, "displayName": dn, "msBefore": ms("offsetBefore"), "msAfter": ms("offsetAfter"), "importance": e["eventScore"].as_f64().unwrap_or(10.0)});
            let mine: Vec<&(String, String, Vec<String>)> = found.iter().filter(|f| f.0 == ev).collect();
            if mine.is_empty() {
                let mut r = base;
                r["words"] = json!([dn.to_uppercase()]);
                r["area"] = json!(ANYWHERE);
                return Some(vec![r]);
            }
            Some(
                mine.into_iter()
                    .map(|(_, crop, words)| {
                        let mut r = base.clone();
                        r["words"] = json!(words);
                        match crops.iter().find(|(n, _)| n == crop) {
                            Some((n, c)) => {
                                r["area"] = json!(n);
                                r["areaCoords"] = json!(c);
                            }
                            None => r["area"] = json!(ANYWHERE),
                        }
                        r
                    })
                    .collect(),
            )
        })
        .flatten()
        .collect();
    let words_known = !found.is_empty();
    json!({"id": id, "name": name, "rows": rows, "official": true, "wordsKnown": words_known})
}

/// The words and screen areas in one of Powder's own text-reading games (game_postprocess.lua
/// written by hand, e.g. CODBO6): crops, then (event, crop, words) for every detector list found.
fn official_detectors(src: &str) -> (Vec<(String, [f32; 4])>, Vec<(String, String, Vec<String>)>) {
    let strs = |s: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut cur: Option<(char, String)> = None;
        for c in s.chars() {
            match &mut cur {
                Some((q, buf)) if c == *q => {
                    out.push(std::mem::take(buf));
                    cur = None;
                }
                Some((_, buf)) => buf.push(c),
                None if c == '"' || c == '\'' => cur = Some((c, String::new())),
                None => {}
            }
        }
        out
    };
    // crops
    let mut crops: Vec<(String, [f32; 4])> = Vec::new();
    let mut name: Option<String> = None;
    for l in src.lines() {
        let t = l.trim();
        if t.starts_with("cropName") {
            name = strs(t).into_iter().next();
        }
        if let (Some(i), Some(n)) = (t.find("cropCoords"), name.as_ref()) {
            if let Some(b) = t[i..].find('{') {
                let v: Vec<f32> = t[i + b + 1..].split(['}', ',']).filter_map(|x| x.trim().parse().ok()).collect();
                if v.len() >= 4 {
                    crops.push((n.clone(), [v[0], v[1], v[2], v[3]]));
                    name = None;
                }
            }
        }
    }
    let lines: Vec<&str> = src.lines().collect();
    let crop_in = |l: &str| -> Option<String> { strs(l).into_iter().find(|w| crops.iter().any(|(n, _)| n == w)) };
    let mut found = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        let Some(rest) = t.strip_prefix("local ") else { continue };
        let Some((var, body)) = rest.split_once('=') else { continue };
        let var = var.trim();
        let body = body.trim();
        if !body.starts_with('{') || var.contains(' ') || var.is_empty() {
            continue;
        }
        // the whole table (may span lines)
        let mut text = String::new();
        let mut depth = 0i32;
        let mut end = i;
        'outer: for (k, l2) in lines.iter().enumerate().skip(i) {
            let part = if k == i { body } else { l2 };
            for c in part.chars() {
                if c == '{' { depth += 1 } else if c == '}' { depth -= 1 }
            }
            text.push_str(part);
            text.push('\n');
            end = k;
            if depth <= 0 {
                break 'outer;
            }
        }
        if text.contains("cropName") {
            continue;
        }
        // where the table is used: the crop named on that line or just after
        let usage = lines.iter().enumerate().skip(end + 1).find(|(_, u)| {
            let u = u.trim();
            u.contains(var) && !u.starts_with(&format!("local {var}"))
        });
        let Some((ui, _)) = usage else { continue };
        let crop = (ui..(ui + 5).min(lines.len())).find_map(|k| crop_in(lines[k]));
        let Some(crop) = crop else { continue };
        if text.contains("event") {
            for entry in text.split("event").skip(1) {
                let e = entry.trim_start().trim_start_matches('=').trim();
                let Some(ev) = strs(e).into_iter().next() else { continue };
                let words = e.find("match").map(|m| {
                    let a = &e[m..];
                    let b = a.find('{').map(|x| x + 1).unwrap_or(0);
                    let c = a.find('}').unwrap_or(a.len());
                    strs(&a[b.min(c)..c])
                });
                if let Some(w) = words.filter(|w| !w.is_empty()) {
                    found.push((ev, crop.clone(), w));
                }
            }
        } else {
            // a plain word list: the event is the first one the code returns after using it
            let words = strs(&text);
            let ev = (ui..(ui + 15).min(lines.len())).find_map(|k| {
                let t = lines[k].trim();
                t.strip_prefix("return ").and_then(|r| strs(r).into_iter().next())
            });
            if let (Some(ev), false) = (ev, words.is_empty()) {
                found.push((ev, crop, words));
            }
        }
    }
    (crops, found)
}

/// Add Game with a template: the template's events plus whatever the new game's scan found that
/// the template doesn't have.
pub fn apply_template(id: &str, template_id: &str) -> Result<usize> {
    let t = template(template_id);
    if let Some(e) = t["error"].as_str() {
        bail!("{e}");
    }
    let mut rows = t["rows"].as_array().cloned().unwrap_or_default();
    let names: Vec<String> = rows.iter().filter_map(|r| r["event"].as_str().map(String::from)).collect();
    for r in load(id)["rows"].as_array().cloned().unwrap_or_default() {
        if r["event"].as_str().map(|e| !names.iter().any(|n| n == e)).unwrap_or(false) {
            rows.push(r);
        }
    }
    save_inner(id, &Value::Array(rows))
}

/// The event a row stands for (new rows only have a display name).
fn event_of(r: &Value) -> String {
    match r["event"].as_str().filter(|e| !e.is_empty()) {
        Some(e) => e.to_string(),
        None => event_name(r["displayName"].as_str().unwrap_or("").trim()),
    }
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
        // a saved edit remembers where its area was: area names are made automatically, so after
        // the game is rebuilt from more videos the same name can mean another part of the screen
        let saved: Vec<f32> = r["areaCoords"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64().map(|v| v as f32)).collect()).unwrap_or_default();
        if saved.len() == 4 && area != ANYWHERE {
            let want = [saved[0], saved[1], saved[2], saved[3]];
            let crops = crops_with_coords(&src);
            if !crops.iter().any(|(n, c)| *n == area && same_rect(c, &want)) {
                if let Some((n, _)) = crops.iter().find(|(_, c)| same_rect(c, &want)) {
                    area = n.clone();
                } else {
                    let mut name = format!("My{area}");
                    let mut k = 2;
                    while crops.iter().any(|(n, _)| *n == name) {
                        name = format!("My{area}{k}");
                        k += 1;
                    }
                    src = add_crop(&src, &name, &want)?;
                    area = name;
                }
            }
        }
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
        // the person's own clip times and importance for this event (from the editor)
        if let Some(r) = rows.iter().find(|r| event_of(r) == *n && (r["msBefore"].is_number() || r["importance"].is_number())) {
            if let (Some(b), Some(a)) = (r["msBefore"].as_f64(), r["msAfter"].as_f64()) {
                e["automontage"]["offsetBefore"] = json!(b.max(0.0) / 1000.0);
                e["automontage"]["offsetAfter"] = json!(a.max(0.0) / 1000.0);
                e["automontage"]["custom"] = json!(true);
            }
            if let Some(i) = r["importance"].as_f64() {
                e["eventScore"] = json!(i.max(0.0));
            }
        }
        e["instruction"] = json!(format!("* {display} *"));
        e["tooltip"] = json!(format!("When {} appears on screen.", words.join(" / ")));
        out.push(e);
    }
    evj["events"] = json!(out);
    std::fs::write(&ev_p, serde_json::to_string_pretty(&evj)?)?;

    // remember what the person changed, so improving the game with another video keeps it
    let baseline: Vec<String> = std::fs::read_to_string(bak.join("game_postprocess.lua")).map(|s| parse_detectors(&s).into_iter().map(|d| d.event).collect()).unwrap_or_default();
    let rects = crops_with_coords(&std::fs::read_to_string(&lua_p).unwrap_or_default());
    let evj_now: Value = serde_json::from_slice(&std::fs::read(&ev_p).unwrap_or_default()).unwrap_or(json!({}));
    let rows_json: Vec<Value> = dets
        .iter()
        .map(|(d, dn)| {
            let mut r = json!({"event": d.event, "displayName": dn, "area": d.crop, "words": d.words});
            event_settings(&evj_now, &d.event, &mut r);
            if let Some((_, c)) = rects.iter().find(|(n, _)| *n == d.crop) {
                r["areaCoords"] = json!(c);
            }
            r
        })
        .collect();
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
        (false, true, 2, 2)
    } else {
        // Powder's own games use a few seconds; long ones chain every nearby event into one
        // over-long moment that the montage maker drops, leaving an empty montage
        (true, false, 3, 3)
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
