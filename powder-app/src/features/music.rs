//! Port of src/main/features/music: Powder's bundled royalty-free tracks (with covers and licence
//! certificates) and imported music, plus their file protocols.
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Webpack asset module id -> bundled file name ("bd35...b233.pow").
pub fn asset_file(main_js: &str, module_id: &str) -> Option<String> {
    let pat = format!(",{module_id}:(e,t,r)=>{{");
    let at = main_js.find(&pat).or_else(|| main_js.find(&format!("{{{module_id}:(e,t,r)=>{{")))?;
    let body = &main_js[at..(at + 400).min(main_js.len())];
    let s = body.find("r.p+\"")? + 5;
    let e = body[s..].find('"')?;
    Some(body[s..s + e].to_string())
}

/// The music list as in music_list.ts, with absolute file paths.
fn parse_list(main_js: &str, dir: &std::path::Path) -> Result<Vec<Value>> {
    let i = main_js.find("t.default=[{id:\"3eba43c7").context("music list not found in main.js")?;
    // module header: `let i=n(r(16928)),a=n(r(31984)),...;t.default=[`
    let head_start = main_js[..i].rfind("let ").context("music header")?;
    let head = &main_js[head_start + 4..i];
    let mut vars = std::collections::HashMap::new();
    for part in head.trim_end_matches(';').split("),") {
        if let Some((v, rest)) = part.split_once("=n(r(") {
            vars.insert(v.trim().to_string(), rest.trim_end_matches(')').to_string());
        }
    }
    let arr_start = i + "t.default=".len();
    let arr_end = crate::jslit::balanced(main_js, arr_start).context("music list end")?;
    let mut src = main_js[arr_start..=arr_end].to_string();
    let file = |v: &str| vars.get(v).and_then(|id| asset_file(main_js, id)).unwrap_or_default();
    // i.default.basename(s.default) -> "file"; s.default -> "full path"
    let re_base: Vec<(String, String)> = vars.keys().map(|v| (format!("i.default.basename({v}.default)"), format!("\"{}\"", file(v)))).collect();
    for (a, b) in re_base {
        src = src.replace(&a, &b);
    }
    for v in vars.keys() {
        let full = dir.join(file(v)).display().to_string().replace('\\', "\\\\");
        src = src.replace(&format!(":{v}.default"), &format!(":\"{full}\""));
    }
    let v = crate::jslit::to_json(&src)?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

static LIST: OnceLock<Vec<Value>> = OnceLock::new();

fn bundled() -> &'static [Value] {
    LIST.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// Bundled tracks plus the user's own: any audio file dropped into `<Rewind folder>\music` or
/// `%APPDATA%\powder-desktop\music` shows up in the music pickers (montages, auto-montage, auto-edits).
pub fn list() -> Vec<Value> {
    let mut v = bundled().to_vec();
    v.extend(user_tracks());
    v
}

pub fn music_dirs() -> Vec<PathBuf> {
    let mut d = Vec::new();
    if let Some(e) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("music"))) {
        d.push(e);
    }
    d.push(crate::setup::home().join("music"));
    d.push(super::user_data().join("music"));
    d
}

const AUDIO: &[&str] = &["mp3", "wav", "ogg", "m4a", "aac", "flac", "opus", "wma"];

fn user_tracks() -> Vec<Value> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    // durations are cached by path + modified time (probing with ffmpeg takes a moment per file)
    static CACHE: OnceLock<Mutex<HashMap<String, f64>>> = OnceLock::new();
    let cache_file = super::user_data().join("powder-rs-music-cache.json");
    let cache = CACHE.get_or_init(|| Mutex::new(std::fs::read(&cache_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()));
    let mut out = Vec::new();
    let mut changed = false;
    for dir in music_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|x| AUDIO.contains(&x.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)).collect();
        files.sort();
        for p in files {
            let mtime = p.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let key = format!("{}|{mtime}", p.display());
            let dur = {
                let mut c = cache.lock().unwrap();
                match c.get(&key) {
                    Some(d) => *d,
                    None => {
                        let d = crate::media::duration_secs(&p).unwrap_or(0.0);
                        c.insert(key.clone(), d);
                        changed = true;
                        d
                    }
                }
            };
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let (artist, title) = match stem.split_once(" - ") {
                Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
                None => ("Your music".to_string(), stem.clone()),
            };
            let id = format!("user-{:016x}", fxhash(&p.display().to_string()));
            let cover = user_cover(&p);
            out.push(json!({"id": id, "title": title, "artist": artist, "label": "", "durationSec": dur.round(), "fileName": p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), "path": p.display().to_string(), "cover": cover.display().to_string(), "certificate": "", "userTrack": true}));
        }
    }
    if changed {
        if let Ok(b) = serde_json::to_vec(&*cache.lock().unwrap()) {
            let _ = std::fs::write(&cache_file, b);
        }
    }
    out
}

fn fxhash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// `track.jpg`/`track.png` next to the audio file, else Rewind's icon.
fn user_cover(audio: &std::path::Path) -> PathBuf {
    for ext in ["jpg", "jpeg", "png", "webp"] {
        let c = audio.with_extension(ext);
        if c.exists() {
            return c;
        }
    }
    let def = super::user_data().join("powder-rs-music-cover.png");
    if !def.exists() {
        let _ = std::fs::create_dir_all(super::user_data());
        let _ = std::fs::write(&def, include_bytes!("../../assets/branding/icon-256.png"));
    }
    def
}

pub fn with_urls(ctx: &Ctx) -> Vec<Value> {
    list()
        .iter()
        .map(|m| {
            let f = m["fileName"].as_str().unwrap_or("");
            let mut m = m.clone();
            m["powderCover"] = json!(crate::protocols::url(&ctx.token, "powder-music-cover", &format!("/{f}")));
            m["powderPath"] = json!(crate::protocols::url(&ctx.token, "powder-music", &format!("/{f}")));
            m
        })
        .collect()
}

fn by_file(name: &str) -> Option<Value> {
    let n = std::path::Path::new(name).file_name()?.to_string_lossy().to_string();
    list().into_iter().find(|m| m["fileName"] == n.as_str())
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    let main_js = String::from_utf8(ctx.dist.read("dist/main/main.js").unwrap_or_default()).unwrap_or_default();
    // bundled assets live next to main.js, unpacked
    let dir = [ctx.dist.native.join("main"), ctx.dist.root.join("resources").join("app.asar.unpacked").join("dist").join("main"), ctx.dist.root.join("resources").join("app-unpacked").join("dist").join("main")]
        .into_iter()
        .find(|d| d.is_dir())
        .unwrap_or_else(|| ctx.dist.native.join("main"));
    match parse_list(&main_js, &dir) {
        Ok(l) => {
            for d in music_dirs() {
                let _ = std::fs::create_dir_all(d);
            }
            crate::log(&format!("{} bundled music tracks", l.len()));
            let _ = LIST.set(l);
        }
        Err(e) => crate::log(&format!("music: {e:#}")),
    }
    crate::protocols::register("powder-imported-music", |rest| {
        let p = PathBuf::from(rest.trim_start_matches('/'));
        p.exists().then_some(p)
    });
    crate::protocols::register("powder-music", |rest| by_file(rest).and_then(|m| m["path"].as_str().map(PathBuf::from)).filter(|p| p.exists()));
    crate::protocols::register("powder-music-cover", |rest| by_file(rest).and_then(|m| m["cover"].as_str().map(PathBuf::from)).filter(|p| p.exists()));
    let c2 = ctx.clone();
    h.handle("music", "getMusicList", move |_, _| Ok(Value::Array(with_urls(&c2))));
    let c3 = ctx.clone();
    h.handle("music", "protocolizeImportedMusicPath", move |_, a| Ok(json!(crate::protocols::url(&c3.token, "powder-imported-music", &format!("/{}", a.first().and_then(|v| v.as_str()).unwrap_or(""))))));
    h.handle("music", "exportMusicCertificate", |_, a| {
        let o = a.into_iter().next().unwrap_or(json!({}));
        let m = list().into_iter().find(|m| m["id"] == o["musicId"]).context("Export music certificate failed: Music not found")?;
        if m["certificate"].as_str().unwrap_or("").is_empty() {
            return Ok(Value::Null); // your own track: no licence certificate to export
        }
        let out = PathBuf::from(o["outputFolderPath"].as_str().unwrap_or("."));
        std::fs::create_dir_all(&out)?;
        std::fs::copy(m["certificate"].as_str().unwrap_or(""), out.join(format!("{} - {} - certificate.pdf", m["title"].as_str().unwrap_or(""), m["artist"].as_str().unwrap_or(""))))?;
        Ok(Value::Null)
    });
}
