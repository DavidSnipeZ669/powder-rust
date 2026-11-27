//! One-click "Add Custom Game Support": title + video in, finished game pack out.
//!
//!   1. discover all on-screen text (discover.rs)
//!   2. decide which text means which event - built-in vocabulary (Powder's official events +
//!      common game phrases), optional phrases mined from the game's own files, and a
//!      "highlight-like text" heuristic for anything unknown
//!   3. generate crops / OCR settings / Lua / events.json (discover::build_game)
//!   4. test-run the pack on the same video and report what it found
//!   5. optionally fetch Steam id + artwork and install into the original Powder app

use crate::discover::{self, Discovery, Group, Rule};
use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Short Powder-style id from a title: "Modern Warfare 4" -> "MW4", "Space Engineers" -> "SPACEENG",
/// "Delta Force" -> "DELTAFORCE" (kept unique against `taken`).
pub fn make_id(title: &str, taken: &[String]) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_uppercase())
        .filter(|w| !["THE", "OF", "A", "AN", "AND"].contains(&w.as_str()))
        .collect();
    let mut id: String = if words.is_empty() {
        "CUSTOM".into()
    } else if words.len() >= 3 {
        words.iter().map(|w| if w.chars().all(|c| c.is_ascii_digit()) { w.clone() } else { w.chars().next().unwrap().to_string() }).collect()
    } else {
        let joined: String = words.concat();
        if joined.len() <= 10 { joined } else {
            words.iter().map(|w| if w.chars().all(|c| c.is_ascii_digit()) { w.clone() } else { w.chars().take(5).collect() }).collect()
        }
    };
    id = id.chars().filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit()).take(10).collect();
    if id.is_empty() || id.chars().next().unwrap().is_ascii_digit() {
        id = format!("G{id}");
    }
    let base = id.clone();
    let mut n = 2;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&id)) {
        id = format!("{base}{n}");
        n += 1;
    }
    id
}

/// Multi-word phrases and single words that mean an event. Checked against the full OCR texts.
const PHRASES: &[(&str, &str)] = &[
    ("SQUAD WIPE", "squadWipe"), ("SQUAD WIPED", "squadWipe"), ("TEAM WIPE", "squadWipe"), ("TEAM ELIMINATED", "squadWipe"),
    ("DOUBLE KILL", "doubleKill"), ("TRIPLE KILL", "tripleKill"), ("QUADRA KILL", "quadraKill"), ("QUAD KILL", "quadraKill"),
    ("PENTA KILL", "pentaKill"), ("MULTI KILL", "multiKill"), ("KILLING SPREE", "killingSpree"),
    ("MISSION SUCCESS", "victory"), ("MISSION COMPLETE", "victory"), ("MATCH WON", "victory"), ("ROUND WON", "roundVictory"),
    ("WINNER WINNER", "victory"), ("YOU WIN", "victory"), ("YOU WON", "victory"), ("GAME WON", "victory"),
    ("MISSION FAILED", "defeat"), ("MATCH LOST", "defeat"), ("ROUND LOST", "roundDefeat"), ("YOU LOSE", "defeat"), ("GAME OVER", "defeat"),
    ("YOU DIED", "death"), ("YOU WERE KILLED", "death"), ("KILLED BY", "death"), ("ELIMINATED BY", "death"), ("YOU HAVE BEEN", "death"),
    ("HEAD SHOT", "headshot"), ("LONG SHOT", "longshot"), ("KNOCKED DOWN", "knockDown"), ("ENEMY DOWN", "kill"),
    ("BOSS DEFEATED", "bossKill"), ("ENEMY FELLED", "bossKill"), ("QUEST COMPLETE", "questComplete"), ("LEVEL UP", "levelUp"),
    ("EXTRACTION SUCCESSFUL", "victory"), ("EXTRACTED", "victory"), ("FIRST BLOOD", "firstBlood"), ("ACE", "ace"),
    ("REVIVED", "revive"), ("REVIVE", "revive"), ("OBJECTIVE CAPTURED", "objective"), ("POINT CAPTURED", "objective"),
    ("BOMB PLANTED", "bombPlanted"), ("BOMB DEFUSED", "bombDefused"), ("GOAL", "goal"), ("TOUCHDOWN", "goal"),
    // shooter medals (Call of Duty, Battlefield and others use the same names)
    ("HEADSHOT", "headshot"), ("HEADSHOT KILL", "headshot"), ("QUICK SCOPE", "quickscope"), ("QUICKSCOPE", "quickscope"),
    ("NO SCOPE", "noscope"), ("NOSCOPE", "noscope"), ("LONGSHOT", "longshot"), ("POINT BLANK", "pointBlank"),
    ("ONE SHOT ONE KILL", "oneShotOneKill"), ("JUMPSHOT", "jumpshot"), ("HIPFIRE", "hipfire"), ("COLLATERAL", "collateral"),
    ("KINGSLAYER", "kingslayer"), ("REVENGE", "revenge"), ("PAYBACK", "revenge"), ("AVENGER", "avenger"), ("SAVIOR", "savior"),
    ("BUZZKILL", "buzzkill"), ("BACKSTABBER", "backstab"), ("BACKSTAB", "backstab"), ("MELEE KILL", "meleeKill"),
    ("BLOODTHIRSTY", "bloodthirsty"), ("MERCILESS", "killStreak"), ("RUTHLESS", "killStreak"), ("RELENTLESS", "killStreak"),
    ("FURY KILL", "furyKill"), ("FRENZY KILL", "frenzyKill"), ("SUPER KILL", "superKill"), ("RAMPAGE", "rampage"),
    ("COMEBACK", "comeback"), ("AFTERLIFE", "afterlife"), ("WALLBANG", "wallbang"),
];

fn phrase_event(text: &str) -> Option<&'static str> {
    phrase_match(text).map(|(_, e)| e)
}

/// Finds the longest known phrase in `text`. OCR often drops spaces ("MISSIONSUCCESS"),
/// so multi-word phrases are also matched with spaces removed.
fn phrase_match(text: &str) -> Option<(&'static str, &'static str)> {
    let words: Vec<String> = text.to_uppercase().split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(String::from).collect();
    let t = format!(" {} ", words.join(" "));
    let squashed = words.concat();
    let mut best: Option<(&str, &str)> = None;
    for (p, e) in PHRASES {
        let hit = t.contains(&format!(" {p} ")) || (p.contains(' ') && squashed.contains(&p.replace(' ', "")));
        if hit && best.map(|b| p.len() > b.0.len()).unwrap_or(true) {
            best = Some((p, e));
        }
    }
    best
}

/// Mine likely event phrases from a game's install folder (localisation / string tables).
/// Best effort: reads text-ish files and Unreal .locres, keeps short UPPER/Title-case phrases that
/// contain event-ish words. Returns phrase -> suggested event name.
pub fn mine_game_files(dir: &Path, limit_files: usize) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let key = regex::Regex::new(r"(?i)\b(kill|killed|elimin|victory|defeat|headshot|knock|assist|win|won|lost|mission|squad|wipe|extract|revive|downed|destroyed|double|triple|streak|spree|ace|mvp|clutch|boss|slain)\w*").unwrap();
    let phrase = regex::Regex::new(r"[A-Za-z][A-Za-z !'-]{2,28}[A-Za-z!]").unwrap();
    let mut seen = 0usize;
    for e in walkdir::WalkDir::new(dir).max_depth(8).into_iter().flatten() {
        if seen >= limit_files { break; }
        let p = e.path();
        let name = p.to_string_lossy().to_lowercase();
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
        let loc_like = ["loc", "lang", "string", "text", "i18n", "l10n", "en-us", "en_us", "english", "/en/", "\\en\\"].iter().any(|k| name.contains(k));
        if !matches!(ext.as_str(), "txt" | "json" | "csv" | "ini" | "xml" | "locres" | "lang" | "loc" | "po" | "yaml" | "yml" | "strings") { continue; }
        if !loc_like && ext != "locres" { continue; }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() || meta.len() > 64 * 1024 * 1024 { continue; }
        let Ok(bytes) = std::fs::read(p) else { continue };
        seen += 1;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if bytes.len() > 2 && bytes.iter().skip(1).step_by(2).take(200).filter(|b| **b == 0).count() > 150 {
            let u16s: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            text = String::from_utf16_lossy(&u16s);
        }
        for m in phrase.find_iter(&text) {
            let s = m.as_str().trim();
            if s.split_whitespace().count() > 4 || !key.is_match(s) { continue; }
            let up = s.to_uppercase();
            let ev = phrase_event(&up)
                .map(|x| x.to_string())
                .or_else(|| up.split_whitespace().find_map(|w| discover::suggest_event(w).map(|x| x.to_string())));
            if let Some(ev) = ev {
                out.entry(up).or_insert(ev);
            }
            if out.len() > 5000 { return out; }
        }
    }
    out
}

fn camel(s: &str) -> String {
    let mut out = String::new();
    for (i, w) in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).enumerate() {
        let w = w.to_lowercase();
        if i == 0 { out += &w; } else { let mut c = w.chars(); out += &(c.next().unwrap().to_uppercase().collect::<String>() + c.as_str()); }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub group: usize,
    pub word: String,
    pub event: String,
    pub reason: String,
    pub count: usize,
}

/// Decide events for discovered text groups automatically.
pub fn classify(d: &Discovery, vocab: &BTreeMap<String, String>, max_custom: usize) -> Vec<Decision> {
    let n = d.frame_times.len().max(1);
    let mut out = Vec::new();
    let mut custom = 0usize;
    // typical text height, to spot "big" announcement text
    let mut hs: Vec<f32> = d.detections.iter().map(|x| x.bbox[3] - x.bbox[1]).collect();
    hs.sort_by(|a, b| a.total_cmp(b));
    let median_h = hs.get(hs.len() / 2).copied().unwrap_or(0.02);
    let mut decided_dets: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // pass 1: known vocabulary; pass 2: unknown highlight-like text
    let mut order: Vec<&Group> = d.groups.iter().collect();
    order.sort_by_key(|g| {
        let known = g.suggestion.is_some() || g.examples.iter().any(|t| phrase_event(t).is_some());
        (!known, std::cmp::Reverse(g.count))
    });
    for g in order {
        if g.is_static { continue; }
        let ex: Vec<&String> = g.examples.iter().collect();
        // 1) phrase in the full text (DOUBLE KILL, MISSION FAILED ...). A word group collects
        //    every text the word appeared in ("KILL" is in "KILL", "DOUBLE KILL", "KILL CONFIRMED"),
        //    so a phrase only decides when most of those texts contain it.
        let by_phrase = {
            let hits: Vec<&str> = ex.iter().filter_map(|t| phrase_event(t)).collect();
            hits.iter().copied().max_by_key(|e| hits.iter().filter(|x| *x == e).count()).filter(|e| hits.iter().filter(|x| *x == e).count() * 2 > ex.len())
        };
        // 2) phrase mined from the game files
        let by_vocab = ex.iter().find_map(|t| {
            let up = t.to_uppercase();
            vocab.iter().find(|(p, _)| up.contains(p.as_str()) && p.split_whitespace().any(|w| w == g.word)).map(|(_, e)| e.clone())
        });
        // 3) single keyword (recomputed: scans saved by older versions used looser matching)
        let by_word = crate::discover::suggest_event(&g.word).map(String::from);
        // one sighting is too little to make an event from (a word in a menu or on a wall)
        if g.count < 2 {
            continue;
        }
        let (event, reason) = if let Some(e) = by_phrase {
            (e.to_string(), format!("phrase in \"{}\"", ex[0]))
        } else if let Some(e) = by_vocab {
            (e, "phrase found in the game's files".to_string())
        } else if let Some(e) = by_word {
            (e, format!("keyword {}", g.word))
        } else {
            // 4) unknown but highlight-like: transient, repeated, centred, big, a real word
            let h = g.detections.iter().map(|i| d.detections[*i].bbox[3] - d.detections[*i].bbox[1]).sum::<f32>() / g.detections.len() as f32;
            let (cx, cy) = ((g.bbox[0] + g.bbox[2]) / 2.0, (g.bbox[1] + g.bbox[3]) / 2.0);
            let central = (cx - 0.5).abs() < 0.25 && (cy - 0.5).abs() < 0.35;
            let frac = g.count as f32 / n as f32;
            let consistent = g.examples.len() <= 4; // the same message, not names/chat
            // announcements are written in capitals; player names / chat usually are not
            let caps = g.word.chars().any(|c| c.is_ascii_uppercase()) && g.examples.iter().all(|t| {
                t.split(|c: char| !c.is_alphabetic()).filter(|w| w.to_uppercase().contains(&g.word) || g.word.contains(&w.to_uppercase()) && w.len() >= 3).all(|w| w.chars().all(|c| !c.is_lowercase()))
            });
            // words that only show up inside an already-detected message (e.g. the name after
            // "ELIMINATED") are part of that event, not a new one
            let inside = g.detections.iter().filter(|i| decided_dets.contains(i)).count() as f32 >= 0.6 * g.detections.len() as f32;
            let contains_known = out.iter().any(|x: &Decision| g.word.contains(&x.word) && g.word != x.word);
            // OCR variants of an already-recognised message show up at the same moments
            // (e.g. "WCTTORY" next to "VICTORY"): give them that event instead of a new one
            let real_word = g.count >= 2 && g.word.len() >= 4 && g.word.chars().all(|c| c.is_ascii_alphabetic()) && caps;
            let iou = |a: [f32; 4], b: [f32; 4]| {
                let ix = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
                let iy = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
                let inter = ix * iy;
                inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter).max(1e-6)
            };
            let same_moment = out.iter().filter(|x: &&Decision| real_word && !x.reason.starts_with("highlight-like")).find_map(|x| {
                let other = &d.groups[x.group];
                if iou(g.bbox, other.bbox) < 0.2 {
                    return None;
                }
                // a misread looks like the word (WCTTORY ~ VICTORY); different words that pop up
                // together (a medal stack: QUICK SCOPE under DOUBLE KILL) are separate events
                if crate::discover::similarity(&g.word, &x.word) < 0.6 {
                    return None;
                }
                let near = g.frames.iter().filter(|f| other.frames.iter().any(|o| (**f as i64 - *o as i64).abs() <= 3)).count();
                (near as f32 >= 0.5 * g.frames.len().max(1) as f32).then(|| (x.event.clone(), x.word.clone()))
            });
            // text that moves around (writing on in-game objects, player names over heads and
            // in scoreboards) is not a HUD announcement
            let centers: Vec<(f32, f32)> = g.detections.iter().map(|i| { let b = d.detections[*i].bbox; ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0) }).collect();
            let med = |mut v: Vec<f32>| { v.sort_by(|a, b| a.total_cmp(b)); v[v.len() / 2] };
            let (mx, my) = (med(centers.iter().map(|c| c.0).collect()), med(centers.iter().map(|c| c.1).collect()));
            let steady = centers.iter().filter(|c| (c.0 - mx).abs() < 0.05 && (c.1 - my).abs() < 0.05).count() as f32 >= 0.6 * centers.len() as f32;
            if let Some((ev, w)) = same_moment {
                (ev, format!("misread of \"{w}\" (same moments)"))
            } else if custom < max_custom && steady && g.count >= 2 && frac < 0.3 && central && h >= median_h * 0.9 && g.word.len() >= 4 && consistent && caps && !inside && !contains_known {
                custom += 1;
                (camel(&g.word), format!("highlight-like text \"{}\" (central, capitals, appears briefly {}x)", g.word, g.count))
            } else {
                continue;
            }
        };
        decided_dets.extend(g.detections.iter().copied());
        out.push(Decision { group: g.id, word: g.word.clone(), event, reason, count: g.count });
    }
    // when a phrase decided the event, drop the less specific words of the same occurrences
    // (e.g. KILL inside "DOUBLE KILL" must not also become a plain kill rule)
    let mut keep = vec![true; out.len()];
    for i in 0..out.len() {
        for j in 0..out.len() {
            if i == j || !keep[i] || !keep[j] || out[i].event == out[j].event { continue; }
            let gi = &d.groups[out[i].group];
            let gj = &d.groups[out[j].group];
            let shared = gi.detections.iter().filter(|x| gj.detections.contains(x)).count();
            // gj fully inside gi's occurrences and gi more specific -> drop gj's separate rule only if gj adds nothing
            if shared > 0 && shared == gj.detections.len() && out[i].reason.starts_with("phrase") && !out[j].reason.starts_with("phrase") {
                keep[j] = false;
            }
        }
    }
    out.into_iter().zip(keep).filter(|(_, k)| *k).map(|(d, _)| d).collect()
}

pub fn rules_from(decisions: &[Decision], d: &Discovery) -> Vec<Rule> {
    let mut by_event: BTreeMap<String, Vec<&Decision>> = BTreeMap::new();
    for x in decisions {
        by_event.entry(x.event.clone()).or_default().push(x);
    }
    let mut rules = Vec::new();
    for (ev, xs) in by_event {
        // split groups of the same event that are far apart on screen into separate rules
        let mut words: Vec<String> = Vec::new();
        for x in &xs {
            // prefer the multi-word phrase when the text is a known phrase
            let g: &Group = &d.groups[x.group];
            let phrase = g.examples.iter().find_map(|t| {
                let up = t.to_uppercase();
                PHRASES.iter().filter(|(p, e)| *e == ev && up.contains(p)).map(|(p, _)| p.to_string()).max_by_key(|p| p.len())
            });
            let phrase = phrase.or_else(|| g.examples.iter().find_map(|t| phrase_match(t).filter(|(_, e)| *e == ev).map(|(p, _)| p.to_string())));
            let w = phrase.unwrap_or_else(|| x.word.clone());
            if !words.contains(&w) { words.push(w); }
        }
        // custom (unknown) events get a readable display name from the clearest example text
        let display_name = if xs.iter().all(|x| x.reason.starts_with("highlight")) {
            let g = &d.groups[xs[0].group];
            // the example whose letters equal the group's (most frequent) word; prefer one with spaces
            let squash = |t: &str| t.to_uppercase().chars().filter(|c| c.is_alphabetic()).collect::<String>();
            let best = g
                .examples
                .iter()
                .filter(|t| squash(t).contains(&g.word))
                .max_by_key(|t| t.matches(' ').count())
                .cloned()
                .unwrap_or_else(|| g.word.clone());
            Some(best.split_whitespace().map(|w| { let l = w.to_lowercase(); let mut c = l.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" "))
        } else {
            None
        };
        rules.push(Rule { event: ev, display_name, groups: xs.iter().map(|x| x.group).collect(), words });
    }
    rules
}

#[derive(Debug, Clone, Default)]
pub struct SteamInfo {
    pub app_id: String,
    pub name: String,
    pub description: Option<String>,
    pub website: Option<String>,
    pub free: bool,
}

/// Accepts a store link (https://store.steampowered.com/app/2507950/Delta_Force/), a
/// steam://, SteamDB link or a bare app id. Returns the app id.
pub fn parse_steam(s: &str) -> Option<String> {
    let s = s.trim();
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Some(s.to_string());
    }
    regex::Regex::new(r"(?:/app/|/apps/|steam://(?:run|rungameid|store)/)(\d+)").unwrap().captures(s).map(|c| c[1].to_string())
}

/// Full details for an app id from the public store API (name, description, website, free).
pub fn steam_details(app_id: &str) -> Option<SteamInfo> {
    let url = format!("https://store.steampowered.com/api/appdetails?l=english&appids={app_id}");
    let v: serde_json::Value = ureq::get(&url).timeout(std::time::Duration::from_secs(8)).call().ok()?.into_json().ok()?;
    let d = &v[app_id]["data"];
    if d.is_null() {
        return None;
    }
    Some(SteamInfo {
        app_id: app_id.to_string(),
        name: d["name"].as_str()?.to_string(),
        description: d["short_description"].as_str().map(|x| x.to_string()),
        website: d["website"].as_str().filter(|w| w.starts_with("http")).map(|x| x.to_string()),
        free: d["is_free"].as_bool().unwrap_or(false),
    })
}

/// Look the title up on the Steam store (optional; needs internet).
pub fn steam_lookup(title: &str) -> Option<SteamInfo> {
    let url = format!("https://store.steampowered.com/api/storesearch/?cc=us&l=en&term={}", urlencode(title));
    let v: serde_json::Value = ureq::get(&url).timeout(std::time::Duration::from_secs(8)).call().ok()?.into_json().ok()?;
    let item = v["items"].as_array()?.first()?;
    let id = item["id"].as_u64()?.to_string();
    steam_details(&id).or(Some(SteamInfo { app_id: id, name: item["name"].as_str()?.to_string(), ..Default::default() }))
}

pub fn steam_artwork(app_id: &str, assets: &Path) -> Result<()> {
    std::fs::create_dir_all(assets)?;
    for (url, file) in [
        (format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/header.jpg"), "banner.jpg"),
        (format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{app_id}/library_600x900.jpg"), "cover.jpg"),
    ] {
        if let Ok(r) = ureq::get(&url).timeout(std::time::Duration::from_secs(10)).call() {
            let mut buf = Vec::new();
            use std::io::Read;
            r.into_reader().take(20_000_000).read_to_end(&mut buf)?;
            if image::load_from_memory(&buf).is_ok() {
                std::fs::write(assets.join(file), &buf)?;
            }
        }
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AddGameReport {
    pub id: String,
    pub title: String,
    pub dir: String,
    pub decisions: Vec<Decision>,
    pub test_events: Vec<crate::analyze::OutputEvent>,
    pub steam_app_id: Option<String>,
    pub installed_into_powder: Option<String>,
    pub warnings: Vec<String>,
    /// on-screen words that looked like announcements but aren't known events: not detected,
    /// offered in Rewind's event editor instead (game names, player names and HUD labels end up
    /// here too, so they shouldn't become events by themselves)
    #[serde(default)]
    pub suggestions: Vec<serde_json::Value>,
}

pub struct AddGameOptions {
    /// may be empty when `steam` is given (the store name is used)
    pub title: String,
    /// Steam store link or app id (optional)
    pub steam: Option<String>,
    pub video: PathBuf,
    pub games_dir: PathBuf,
    pub game_files: Option<PathBuf>,
    pub ffmpeg: PathBuf,
    pub ocr_models: PathBuf,
    pub lua_libs: Vec<PathBuf>,
    pub fps: crate::video::FpsSettings,
    pub scan_fps_default: f64,
    pub online: bool,
    pub install_powder: Option<PathBuf>,
    pub work_dir: PathBuf,
    pub overwrite: bool,
    pub reuse_scan: bool,
}

pub fn add_game(o: &AddGameOptions, progress: Option<discover::SharedProgress>) -> Result<AddGameReport> {
    let say = |m: &str| {
        if let Some(p) = &progress {
            let mut p = p.lock().unwrap();
            p.message = m.to_string();
            p.state = if m == "done" { "done".into() } else if m.starts_with("scanning") { "scanning".into() } else { "working".into() };
        } else {
            eprintln!("[add-game] {m}");
        }
    };
    // Steam: explicit link/id first, otherwise search by title (both optional, need internet)
    let mut steam: Option<SteamInfo> = None;
    if let Some(sv) = &o.steam {
        let id = parse_steam(sv).ok_or_else(|| anyhow::anyhow!("'{sv}' is not a Steam store link or app id"))?;
        say(&format!("reading Steam store page for app {id}"));
        steam = steam_details(&id).or(Some(SteamInfo { app_id: id, ..Default::default() }));
    } else if o.online && !o.title.is_empty() {
        say("looking up the game on Steam");
        steam = steam_lookup(&o.title);
    }
    let title = if !o.title.trim().is_empty() {
        o.title.trim().to_string()
    } else {
        steam.as_ref().map(|s| s.name.clone()).filter(|n| !n.is_empty()).ok_or_else(|| anyhow::anyhow!("give a title (or a Steam link we can read the name from)"))?
    };
    let taken: Vec<String> = crate::game::list_games(&[o.games_dir.clone()]).into_iter().map(|g| g.id).collect();
    let id = make_id(&title, &if o.overwrite { vec![] } else { taken });
    let mut rep = AddGameReport { id: id.clone(), title: title.clone(), ..Default::default() };

    let info = crate::video::probe(&o.ffmpeg, &o.video)?;
    let mut scan_fps = o.fps.clone();
    if scan_fps.sample.is_none() {
        scan_fps.sample = Some(o.scan_fps_default);
    }
    let (eff, why) = scan_fps.effective(&info, o.scan_fps_default);
    say(&format!("scanning video for text ({why})"));
    let dopt = discover::DiscoverOptions { ffmpeg: o.ffmpeg.clone(), ocr_models: o.ocr_models.clone(), fps: eff, start: None, end: None, out_dir: o.work_dir.join(&id) };
    // reuse an earlier scan of the same video at the same rate (re-running after tweaks is instant)
    let cached = o.work_dir.join(&id).join("discovery.json");
    let d = match discover::load(&cached) {
        Ok(c) if c.video == o.video.display().to_string() && (c.fps - eff).abs() < 1e-6 && o.reuse_scan => {
            say("reusing previous scan of this video");
            let mut c = c;
            c.groups = discover::group(&c.detections, c.frame_times.len());
            c
        }
        _ => discover::discover(&o.video, &dopt, progress.clone())?,
    };

    let vocab = match &o.game_files {
        Some(dir) => {
            say("looking for event names in the game's files");
            let v = mine_game_files(dir, 20000);
            if v.is_empty() { rep.warnings.push("no event-like phrases found in the game files".into()); }
            v
        }
        None => BTreeMap::new(),
    };
    say("deciding which text means which event");
    let all = classify(&d, &vocab, 6);
    if all.is_empty() {
        bail!("no event-like text was found in this video. Use a clip that shows kills / wins on screen, or point --game-files at the install folder, or pick text manually in the studio.");
    }
    // unknown words become suggestions, not events (unless nothing else was found)
    let (known, custom): (Vec<Decision>, Vec<Decision>) = all.into_iter().partition(|x| !x.reason.starts_with("highlight-like"));
    rep.suggestions = custom.iter().map(|x| serde_json::json!({"word": x.word, "count": x.count})).collect();
    let decisions = if known.is_empty() { custom } else { known };
    let rules = rules_from(&decisions, &d);
    rep.decisions = decisions;
    let dir = discover::build_game(&d, &rules, &o.games_dir, &id, &title, true)?;
    rep.dir = dir.display().to_string();
    let _ = std::fs::write(dir.join("suggestions.json"), serde_json::to_string_pretty(&rep.suggestions)?);

    // metadata + artwork
    let meta_p = dir.join("game.json");
    let mut meta: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&meta_p)?)?;
    match &steam {
        Some(s) => {
            meta["steamId"] = serde_json::json!(s.app_id);
            if let Some(d) = &s.description { meta["description"] = serde_json::json!(d); }
            if let Some(w) = &s.website { meta["websiteUrl"] = serde_json::json!(w); }
            meta["free2play"] = serde_json::json!(s.free);
            if o.online {
                say("downloading artwork from Steam");
                let _ = steam_artwork(&s.app_id, &dir.join("assets"));
            }
            rep.steam_app_id = Some(s.app_id.clone());
        }
        None if o.online => rep.warnings.push("game not found on Steam (or offline) - placeholder artwork will be used; pass --steam <store link> to fix".into()),
        None => {}
    }
    std::fs::write(&meta_p, serde_json::to_string_pretty(&meta)?)?;

    // test run with the analysis rate the user picked (default: the script's own rate)
    say("test-running the new game on your video");
    let game = crate::game::GamePack::load(&dir)?;
    let aopt = crate::analyze::AnalyzeOptions {
        ffmpeg: o.ffmpeg.clone(),
        lua_libs: o.lua_libs.clone(),
        ocr_models: o.ocr_models.clone(),
        start: None,
        end: None,
        fps: o.fps.clone(),
        fps_native_requested: false,
        skip_unchanged: true,
        max_height: Some(1440),
        debug_dir: None,
        debug_all: false,
        quiet: progress.is_some(),
        workers: None,
        progress: None,
    };
    let a = crate::analyze::analyze(&game, &o.video, &aopt)?;
    std::fs::write(o.work_dir.join(&id).join("test.events.json"), serde_json::to_string_pretty(&a)?)?;
    rep.test_events = a.events;
    rep.warnings.extend(a.warnings);

    if let Some(p) = &o.install_powder {
        say("installing into Powder");
        let pd = crate::patcher::powder_dir(Some(p.clone()))?;
        crate::patcher::install(&game, &pd, &o.lua_libs)?;
        rep.installed_into_powder = Some(pd.display().to_string());
    }
    std::fs::write(o.work_dir.join(&id).join("add-game-report.json"), serde_json::to_string_pretty(&rep)?)?;
    say("done");
    Ok(rep)
}
