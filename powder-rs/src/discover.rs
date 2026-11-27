//! Auto mode: find every piece of on-screen text in a video, group repeats, and turn the
//! groups a user picks ("VICTORY" -> victory, "ELIMINATED" -> kill) into a game pack
//! (crop regions, OCR settings, match words, events.json, game_postprocess.lua) automatically.

use crate::ocr::Ocr;
use crate::video::{FrameReader, SampleOptions};
use anyhow::{bail, Context, Result};
use image::imageops;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Detector input used for full frames (16:9, multiple of 32). Larger = finds smaller text, slower.
pub const FULL_DET_W: u32 = 1280;
/// extra pass over each quarter of the screen every this many sampled frames (see discover)
const QUARTER_EVERY: usize = 6;
const QUARTERS: [[f32; 4]; 4] = [[0.0, 0.0, 0.55, 0.55], [0.45, 0.0, 1.0, 0.55], [0.0, 0.45, 0.55, 1.0], [0.45, 0.45, 1.0, 1.0]];
pub const FULL_DET_H: u32 = 736;
pub const EVENT_CATALOG: &str = include_str!("../templates/event_catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub frame: usize,
    /// normalised l,t,r,b
    pub bbox: [f32; 4],
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: usize,
    /// the keyword (normalised, upper case)
    pub word: String,
    pub count: usize,
    pub frames: Vec<usize>,
    /// indexes into `detections`
    pub detections: Vec<usize>,
    /// union of all occurrences
    pub bbox: [f32; 4],
    /// full texts this word appeared in (up to 8 examples)
    pub examples: Vec<String>,
    /// visible in most frames -> probably static HUD
    pub is_static: bool,
    pub score: f32,
    /// suggested event name from the catalog, if the word looks like one
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Discovery {
    pub video: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frame_times: Vec<f64>,
    pub detections: Vec<Detection>,
    pub groups: Vec<Group>,
    /// folder with frame JPEGs (frame_000000.jpg ...)
    pub frames_dir: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Progress {
    pub state: String,
    pub done: usize,
    pub total: usize,
    pub message: String,
}

pub type SharedProgress = Arc<Mutex<Progress>>;

fn words_of(text: &str) -> Vec<String> {
    text.to_uppercase()
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_string())
        .collect()
}

/// Suggest a catalog event for a word ("ELIMINATED" -> kill).
pub fn suggest_event(word: &str) -> Option<&'static str> {
    let w = word.to_uppercase();
    let table: &[(&[&str], &str)] = &[
        (&["VICTORY", "WIN", "WINNER", "WON", "CHAMPION", "CHAMPIONS", "SUCCESS", "EXTRACTED", "VICTOIRE"], "victory"),
        (&["DEFEAT", "LOSE", "LOST", "FAILED", "DEFEATED"], "defeat"),
        (&["HEADSHOT"], "headshot"),
        (&["DOUBLE"], "doubleKill"),
        (&["TRIPLE"], "tripleKill"),
        (&["QUADRA", "QUAD"], "quadraKill"),
        (&["PENTA", "PENTAKILL"], "pentaKill"),
        (&["ASSIST", "ASSISTED"], "assist"),
        (&["KNOCKED", "KNOCK", "KNOCKDOWN", "DOWNED"], "knockDown"),
        (&["KILL", "KILLED", "ELIMINATED", "ELIMINATION", "ELIM", "TAKEDOWN", "SLAIN", "DESTROYED"], "kill"),
        (&["DIED", "DEAD", "WASTED", "SPECTATING", "RESPAWN", "RESPAWNING"], "death"),
        (&["ACE"], "ace"),
        (&["MVP"], "mvp"),
    ];
    for (words, ev) in table {
        if words.iter().any(|x| w == *x) {
            return Some(ev);
        }
    }
    // plurals are scoreboard / stats labels (KILLS, ELIMINATIONS, ASSISTS), not announcements
    if let Some(sing) = w.strip_suffix('S') {
        if table.iter().any(|(words, _)| words.iter().any(|x| *x == sing)) {
            return None;
        }
    }
    // OCR often misreads a letter or two on busy backgrounds ("VCTORY", "WCTTORY", "ELIMINATFD"):
    // accept close matches for words of 5+ letters
    if w.len() >= 5 && w.chars().all(|c| c.is_ascii_alphabetic()) {
        let mut best: Option<(f32, &'static str)> = None;
        for (words, ev) in table {
            for x in words.iter().filter(|x| x.len() >= 5) {
                let sim = similarity(&w, x);
                // close OCR misreads only (SKILLED ~ KILLED); 0.7 let UNLOCKED pass as KNOCKED
                if sim >= 0.8 && w.chars().next() == x.chars().next() && best.map(|b| sim > b.0).unwrap_or(true) {
                    best = Some((sim, ev));
                }
            }
        }
        return best.map(|b| b.1);
    }
    None
}


fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn center(b: &[f32; 4]) -> (f32, f32) {
    ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)
}

/// Group detections by keyword, then split each keyword by screen position.
pub fn group(dets: &[Detection], n_frames: usize) -> Vec<Group> {
    let mut by_word: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in dets.iter().enumerate() {
        let mut seen = Vec::new();
        for w in words_of(&d.text) {
            if !seen.contains(&w) {
                by_word.entry(w.clone()).or_default().push(i);
                seen.push(w);
            }
        }
    }
    let mut groups = Vec::new();
    for (word, idxs) in by_word {
        // greedy position clustering
        let mut clusters: Vec<(f32, f32, Vec<usize>)> = Vec::new();
        for i in idxs {
            let (cx, cy) = center(&dets[i].bbox);
            match clusters.iter_mut().find(|c| (c.0 - cx).abs() < 0.10 && (c.1 - cy).abs() < 0.07) {
                Some(c) => {
                    let n = c.2.len() as f32;
                    c.0 = (c.0 * n + cx) / (n + 1.0);
                    c.1 = (c.1 * n + cy) / (n + 1.0);
                    c.2.push(i);
                }
                None => clusters.push((cx, cy, vec![i])),
            }
        }
        for (cx, cy, members) in clusters {
            let mut frames: Vec<usize> = members.iter().map(|&i| dets[i].frame).collect();
            frames.sort();
            frames.dedup();
            let bbox = members.iter().fold(dets[members[0]].bbox, |a, &i| union(a, dets[i].bbox));
            let mut examples: Vec<String> = Vec::new();
            for &i in &members {
                let t = dets[i].text.clone();
                if !examples.contains(&t) && examples.len() < 8 {
                    examples.push(t);
                }
            }
            let frac = frames.len() as f32 / n_frames.max(1) as f32;
            let is_static = frac > 0.5 && n_frames >= 6;
            let suggestion = suggest_event(&word).map(|s| s.to_string());
            // ranking: transient, repeated, central, large, event-like words first
            let h = members.iter().map(|&i| dets[i].bbox[3] - dets[i].bbox[1]).sum::<f32>() / members.len() as f32;
            let centrality = 1.0 - ((cx - 0.5).abs() + (cy - 0.5).abs()).min(1.0);
            let mut score = (frames.len() as f32).ln_1p() + centrality + h * 20.0;
            if suggestion.is_some() { score += 3.0; }
            if is_static { score -= 4.0; }
            if frames.len() < 2 { score -= 1.0; }
            groups.push(Group { id: 0, word: word.clone(), count: frames.len(), frames, detections: members, bbox, examples, is_static, score, suggestion });
        }
    }
    // merge OCR misreads of the same text at the same place (DBJECTIVE -> OBJECTIVE)
    groups.sort_by(|a, b| b.count.cmp(&a.count));
    let mut merged: Vec<Group> = Vec::new();
    for g in groups {
        let target = merged.iter_mut().find(|m| {
            let ov = m.bbox[0] < g.bbox[2] && g.bbox[0] < m.bbox[2] && m.bbox[1] < g.bbox[3] && g.bbox[1] < m.bbox[3];
            ov && similarity(&m.word, &g.word) >= 0.75 && m.is_static == g.is_static
        });
        match target {
            Some(m) => {
                m.frames.extend(g.frames);
                m.frames.sort();
                m.frames.dedup();
                m.count = m.frames.len();
                m.detections.extend(g.detections);
                m.bbox = union(m.bbox, g.bbox);
                for e in g.examples {
                    if !m.examples.contains(&e) && m.examples.len() < 8 { m.examples.push(e); }
                }
                m.score = m.score.max(g.score);
            }
            None => merged.push(g),
        }
    }
    let mut groups = merged;
    groups.sort_by(|a, b| b.score.total_cmp(&a.score));
    for (i, g) in groups.iter_mut().enumerate() {
        g.id = i;
    }
    groups
}

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let c = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + c);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn similarity(a: &str, b: &str) -> f32 {
    let m = a.chars().count().max(b.chars().count());
    if m == 0 { return 1.0; }
    1.0 - levenshtein(a, b) as f32 / m as f32
}

pub struct DiscoverOptions {
    pub ffmpeg: PathBuf,
    pub ocr_models: PathBuf,
    pub fps: f64,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub out_dir: PathBuf,
}

pub fn discover(video: &Path, opt: &DiscoverOptions, progress: Option<SharedProgress>) -> Result<Discovery> {
    let set = |p: &Option<SharedProgress>, f: &dyn Fn(&mut Progress)| {
        if let Some(p) = p { f(&mut p.lock().unwrap()); }
    };
    set(&progress, &|p| { p.state = "loading".into(); p.message = "loading OCR models".into(); });
    let mut ocr = Ocr::load(&opt.ocr_models)?;
    let quiet = progress.is_some();
    if !quiet {
        eprintln!("[scan] text reading on {}", if crate::runtime::gpu() { "the GPU (DirectML)" } else { "the CPU" });
    }
    let info = crate::video::probe(&opt.ffmpeg, video)?;
    let frames_dir = opt.out_dir.join("frames");
    std::fs::create_dir_all(&frames_dir)?;
    let mut reader = FrameReader::open(&opt.ffmpeg, video, &info, &SampleOptions { fps: opt.fps, start: opt.start, end: opt.end, max_height: Some(1440) })?;
    let total = info.duration_sec.map(|d| ((opt.end.unwrap_or(d).min(d) - opt.start.unwrap_or(0.0)).max(0.0) * opt.fps) as usize).unwrap_or(0);
    set(&progress, &|p| { p.state = "scanning".into(); p.total = total; p.message = "reading all on-screen text".into(); });
    let full = crate::tools::adhoc_crop("full", [0.0, 0.0, 1.0, 1.0], 3, 40, 4, "");
    let mut d = Discovery { video: video.display().to_string(), width: info.width, height: info.height, fps: opt.fps, frames_dir: frames_dir.display().to_string(), ..Default::default() };
    let t0 = std::time::Instant::now();
    let mut last_pct = usize::MAX;
    while let Some((t, frame)) = reader.next_frame()? {
        let idx = d.frame_times.len();
        d.frame_times.push(t);
        let r = ocr.run_crop_sized(&frame, &full, FULL_DET_W, FULL_DET_H)?;
        let (w, h) = (frame.width() as f32, frame.height() as f32);
        let mut seen_here: Vec<String> = Vec::new();
        for b in r.boxes {
            let text = b.text.trim().to_string();
            if text.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
                continue;
            }
            seen_here.push(text.to_uppercase());
            d.detections.push(Detection { frame: idx, bbox: [b.x0 as f32 / w, b.y0 as f32 / h, b.x1 as f32 / w, b.y1 as f32 / h], text });
        }
        // Reading the whole screen at once misses very large, thin text (results screens like
        // VICTORY get merged with what's around them). Every few seconds, also read each quarter of
        // the screen at twice the detail; those screens stay up long enough to be caught.
        if idx % QUARTER_EVERY == 0 {
            for q in QUARTERS {
                let tile = crate::tools::adhoc_crop("quarter", q, 5, 80, 8, "");
                let (ox, oy, _, _) = crate::ocr::crop_rect(frame.width(), frame.height(), &q);
                let r = ocr.run_crop_sized(&frame, &tile, FULL_DET_W, FULL_DET_H)?;
                for b in r.boxes {
                    let text = b.text.trim().to_string();
                    if text.chars().filter(|c| c.is_alphanumeric()).count() < 3 || seen_here.contains(&text.to_uppercase()) {
                        continue;
                    }
                    seen_here.push(text.to_uppercase());
                    let bb = [(b.x0 + ox) as f32 / w, (b.y0 + oy) as f32 / h, (b.x1 + ox) as f32 / w, (b.y1 + oy) as f32 / h];
                    d.detections.push(Detection { frame: idx, bbox: bb, text });
                }
            }
        }
        let small = imageops::resize(&frame, 960, (960.0 * h / w) as u32, imageops::FilterType::Triangle);
        small.save(frames_dir.join(format!("frame_{idx:06}.jpg")))?;
        set(&progress, &|p| p.done = idx + 1);
        // progress for whoever runs us from the command line (Rewind shows these lines)
        if !quiet && total > 0 {
            let pct = (idx + 1) * 100 / total;
            if pct != last_pct && (pct % 5 == 0 || idx == 0) {
                last_pct = pct;
                let el = t0.elapsed().as_secs_f64();
                let left = if idx > 0 { el / (idx + 1) as f64 * (total.saturating_sub(idx + 1)) as f64 } else { 0.0 };
                eprintln!("[scan] {pct}% ({} of {total} frames, {:.0}s so far, about {:.0} min left)", idx + 1, el, (left / 60.0).ceil());
            }
        }
    }
    if d.frame_times.is_empty() {
        bail!("no frames decoded");
    }
    d.groups = group(&d.detections, d.frame_times.len());
    std::fs::write(opt.out_dir.join("discovery.json"), serde_json::to_string(&d)?)?;
    set(&progress, &|p| { p.state = "done".into(); p.message = format!("{} text groups found", d.groups.len()); });
    Ok(d)
}

// ---------------------------------------------------------------- building a game

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    /// event name (catalog name like "kill" or a custom camelCase name)
    pub event: String,
    /// optional display name for custom events
    #[serde(default)]
    pub display_name: Option<String>,
    /// discovery group ids that show this event
    pub groups: Vec<usize>,
    /// match words (defaults to the group words)
    #[serde(default)]
    pub words: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GeneratedCrop {
    pub name: String,
    pub coords: [f32; 4],
    pub min_area: i32,
    pub dilate: i32,
    pub margin: i32,
}

/// Group text boxes by where they are on screen; returns the clusters worth reading (at least
/// 3 boxes and 15% of all), largest first. Always returns at least the biggest one.
fn clusters(boxes: &[[f32; 4]]) -> Vec<Vec<[f32; 4]>> {
    let c = |b: &[f32; 4]| ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    let mut cl: Vec<((f32, f32), Vec<[f32; 4]>)> = Vec::new();
    for b in boxes {
        let (x, y) = c(b);
        match cl.iter_mut().find(|(m, _)| (m.0 - x).abs() < 0.07 && (m.1 - y).abs() < 0.07) {
            Some((m, v)) => {
                v.push(*b);
                let n = v.len() as f32;
                *m = (m.0 + (x - m.0) / n, m.1 + (y - m.1) / n);
            }
            None => cl.push(((x, y), vec![*b])),
        }
    }
    cl.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    let total = boxes.len().max(1);
    let keep: Vec<Vec<[f32; 4]>> = cl.iter().filter(|(_, v)| v.len() >= 3 && v.len() * 100 >= total * 15).map(|(_, v)| v.clone()).collect();
    if keep.is_empty() {
        cl.into_iter().take(1).map(|(_, v)| v).collect()
    } else {
        keep
    }
}

fn expand(b: [f32; 4]) -> [f32; 4] {
    let w = b[2] - b[0];
    let h = b[3] - b[1];
    let px = (w * 0.15).max(0.02);
    let py = (h * 0.6).max(0.015);
    [(b[0] - px).max(0.0), (b[1] - py).max(0.0), (b[2] + px).min(1.0), (b[3] + py).min(1.0)]
}

fn overlaps(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// "oneShotOneKill" -> "One Shot One Kill"
fn spaced_name(ev: &str) -> String {
    let mut out = String::new();
    for (i, ch) in ev.chars().enumerate() {
        if i > 0 && ch.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(if i == 0 { ch.to_ascii_uppercase() } else { ch });
    }
    out
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Smallest text area (in the 512x288 detector space Powder uses) seen inside a crop.
fn min_det_area(crop: &[f32; 4], boxes: &[[f32; 4]], fw: f32, fh: f32) -> f32 {
    let cw = (crop[2] - crop[0]) * fw;
    let ch = (crop[3] - crop[1]) * fh;
    let s = (crate::ocr::DET_W as f32 / cw).min(crate::ocr::DET_H as f32 / ch);
    boxes
        .iter()
        .map(|b| ((b[2] - b[0]) * fw * s) * ((b[3] - b[1]) * fh * s))
        .fold(f32::MAX, f32::min)
}

pub fn build_game(d: &Discovery, rules: &[Rule], games_dir: &Path, id: &str, name: &str, overwrite: bool) -> Result<PathBuf> {
    if rules.is_empty() {
        bail!("select at least one text and assign it an event");
    }
    if !crate::tools::valid_id(id) {
        bail!("game id must be 1-12 characters A-Z, 0-9 or _");
    }
    let dir = games_dir.join(id);
    if dir.exists() {
        if !overwrite {
            bail!("{} already exists (tick overwrite to replace it)", dir.display());
        }
    } else {
        crate::tools::new_game(games_dir, id, name, None, None)?;
    }
    let (fw, fh) = (d.width as f32, d.height as f32);

    // 1. region per rule
    struct R<'a> { rule: &'a Rule, region: [f32; 4], boxes: Vec<[f32; 4]>, words: Vec<String> }
    let mut rs: Vec<R> = Vec::new();
    for r in rules {
        let gs: Vec<&Group> = r.groups.iter().filter_map(|g| d.groups.get(*g)).collect();
        if gs.is_empty() {
            bail!("rule for '{}' has no text selected", r.event);
        }
        let boxes: Vec<[f32; 4]> = gs.iter().flat_map(|g| g.detections.iter().map(|i| d.detections[*i].bbox)).collect();
        let mut words: Vec<String> = if r.words.is_empty() { gs.iter().map(|g| g.word.clone()).collect() } else { r.words.iter().map(|w| w.to_uppercase()).collect() };
        words.dedup();
        // the places on screen where this text really shows up: one union of every occurrence
        // easily covers the whole screen (a word also seen in a scoreboard or a menu), so cluster
        // the occurrences and keep the places that hold a real share of them
        for cl in clusters(&boxes) {
            let region = cl.iter().fold(cl[0], |a, b| union(a, *b));
            rs.push(R { rule: r, region: expand(region), boxes: cl, words: words.clone() });
        }
    }
    // 2. merge overlapping regions into shared crops
    let mut crops: Vec<(GeneratedCrop, Vec<usize>, Vec<[f32; 4]>)> = Vec::new();
    for (i, r) in rs.iter().enumerate() {
        let area = |b: &[f32; 4]| (b[2] - b[0]) * (b[3] - b[1]);
        match crops.iter_mut().find(|c| overlaps(&c.0.coords, &r.region) && area(&union(c.0.coords, r.region)) < 0.2) {
            Some(c) => {
                c.0.coords = union(c.0.coords, r.region);
                c.1.push(i);
                c.2.extend(r.boxes.iter().cloned());
            }
            None => crops.push((
                GeneratedCrop { name: format!("{}Area", ucfirst(&r.rule.event)), coords: r.region, min_area: 0, dilate: 5, margin: 8 },
                vec![i],
                r.boxes.clone(),
            )),
        }
    }
    let mut used = Vec::new();
    for (k, c) in crops.iter_mut().enumerate() {
        if c.1.len() > 1 {
            c.0.name = format!("Region{}", k + 1);
        }
        if used.contains(&c.0.name) { c.0.name = format!("{}{}", c.0.name, k + 1); }
        used.push(c.0.name.clone());
        let a = min_det_area(&c.0.coords, &c.2, fw, fh);
        c.0.min_area = ((a * 0.35) as i32).clamp(20, 20000);
        let big = a > 4000.0;
        c.0.dilate = if big { 7 } else { 5 };
        c.0.margin = if big { 16 } else { 8 };
    }

    // 3. Lua
    let catalog: Value = serde_json::from_str(EVENT_CATALOG)?;
    let lookup = |ev: &str| catalog["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == ev)).cloned();
    let mut crop_lua = String::new();
    for (c, _, _) in &crops {
        crop_lua += &format!(
            "        {{\n            cropName = \"{}\",\n            debug = false,\n            cropCoords = {{ {:.3}, {:.3}, {:.3}, {:.3} }},\n            detectorDilateDiameter = {},\n            detectorMinimumArea = {},\n            detectorMargin = {},\n            recogniserStretchVertical = false,\n            restrictedCharacters = \"\"\n        }},\n",
            c.name, c.coords[0], c.coords[1], c.coords[2], c.coords[3], c.dilate, c.min_area, c.margin
        );
    }
    let mut det_lua = String::new();
    let mut specs: Vec<String> = Vec::new();
    let mut events = Vec::new();
    for (c, members, _) in &crops {
        for &i in members {
            let r = &rs[i];
            let ev = &r.rule.event;
            let lower = ev.to_lowercase();
            let is_end = ["victory", "defeat", "win", "lose", "result"].iter().any(|k| lower.contains(k));
            let offset = if lower.contains("kill") || lower.contains("headshot") || lower.contains("knock") || lower.contains("assist") { -0.5 } else { 0.0 };
            // how long the text was seen in a row during the scan, to choose a safe "visible" time
            let visible = if is_end { 0.5 } else { 0.3 };
            let longest = r.words.iter().map(|w| w.chars().count()).max().unwrap_or(4);
            let score = if longest <= 4 { 85 } else { 78 };
            let quoted: Vec<String> = r.words.iter().map(|w| format!("\"{}\"", w.replace('"', ""))).collect();
            det_lua += &format!(
                "    {{ event = \"{ev}\", crop = \"{}\", match = {{ {} }}, score = {score}, visible = {visible}, offset = {offset}, priority = {longest} }},\n",
                c.name,
                quoted.join(", ")
            );
            if !specs.iter().any(|s| s.starts_with(&format!("    {ev} = "))) {
                let merge = if is_end { 10.0 } else if lower.contains("double") || lower.contains("triple") || lower.contains("quadra") || lower.contains("penta") { 4.0 } else { 2.0 };
                specs.push(format!("    {ev} = {merge:.1},\n"));
                let mut e = lookup(ev).unwrap_or_else(|| {
                    json!({"name": ev, "icon": if is_end { "overall-victory" } else { "generic-event" }, "tooltip": format!("When {} appears on screen.", r.words.join(" / ")), "eventScore": 10,
                           "automontage": {"primary": !is_end, "secondary": is_end, "offsetBefore": 2, "offsetAfter": 2, "effectTypes": []}})
                });
                let dn = r.rule.display_name.clone().or_else(|| e["displayName"].as_str().map(String::from)).unwrap_or_else(|| spaced_name(ev));
                e["displayName"] = json!(dn);
                e["default"] = json!(true);
                e["instruction"] = json!(format!("* {dn} *"));
                events.push(e);
            }
        }
    }
    let lua_path = dir.join("game_postprocess.lua");
    let mut src = crate::tools::TEMPLATE_LUA.replace("{{ID}}", id).replace("{{NAME}}", name);
    let re_crops = regex::Regex::new(r"(?s)local ocrConfig = \{.*?\n\}\n").unwrap();
    src = re_crops.replace(&src, format!("local ocrConfig = {{\n    crops = {{\n{}    }}\n}}\n", crop_lua).as_str()).to_string();
    let re_det = regex::Regex::new(r"(?s)local DETECTORS = \{.*?\n\}\n").unwrap();
    src = re_det.replace(&src, format!("local DETECTORS = {{\n{det_lua}}}\n").as_str()).to_string();
    let re_spec = regex::Regex::new(r"(?s)local EVENT_MERGE = \{.*?\n\}\n").unwrap();
    src = re_spec.replace(&src, format!("local EVENT_MERGE = {{\n{}}}\n", specs.concat()).as_str()).to_string();
    src = src.replace("created with powder-rs new-game", "generated by powder-rs studio (Auto mode) - safe to edit by hand");
    std::fs::write(&lua_path, src)?;
    std::fs::write(dir.join("events.json"), serde_json::to_string_pretty(&json!({ "name": id, "events": events }))?)?;
    let meta_p = dir.join("game.json");
    let mut meta: Value = serde_json::from_str(&std::fs::read_to_string(&meta_p).unwrap_or("{}".into())).unwrap_or(json!({}));
    meta["name"] = json!(name);
    meta["calibratedResolution"] = json!(format!("{}x{}", d.width, d.height));
    std::fs::write(&meta_p, serde_json::to_string_pretty(&meta)?)?;
    Ok(dir)
}

/// Several scans (videos) of one game as one: frames are appended one after another.
pub fn combine(ds: Vec<Discovery>) -> Option<Discovery> {
    let mut it = ds.into_iter();
    let mut out = it.next()?;
    for d in it {
        let off = out.frame_times.len();
        let t_off = out.frame_times.last().copied().unwrap_or(0.0) + 60.0;
        out.frame_times.extend(d.frame_times.iter().map(|t| t + t_off));
        out.detections.extend(d.detections.into_iter().map(|mut x| {
            x.frame += off;
            x
        }));
    }
    out.groups = group(&out.detections, out.frame_times.len());
    Some(out)
}

/// Every saved scan of a game: `<work>/discovery.json` plus `<work>/videos/*/discovery.json`.
pub fn load_all(work: &Path) -> Vec<Discovery> {
    let mut v = Vec::new();
    if let Ok(d) = load(&work.join("discovery.json")) {
        v.push(d);
    }
    if let Ok(rd) = std::fs::read_dir(work.join("videos")) {
        let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for p in dirs {
            if let Ok(d) = load(&p.join("discovery.json")) {
                v.push(d);
            }
        }
    }
    v
}

pub fn load(path: &Path) -> Result<Discovery> {
    serde_json::from_str(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?).context("parsing discovery.json")
}
