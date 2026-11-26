//! The video library: recordings / downloads / clips found in the configured folders, with
//! per-video sidecar data (game, events, bookmarks) stored in the app data folder.
//! Powder's own recordings carry a JSON metadata block inside the MP4 (game id, events);
//! it is imported automatically.

use crate::settings::{data_dir, Settings};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LibEvent {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    pub time_sec: f64,
    /// "video" (game events), "bookmark", "audio", "chat", "keyword"
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Sidecar {
    pub game: Option<String>,
    pub events: Vec<LibEvent>,
    pub analyzed: bool,
    pub analyzed_at: Option<u64>,
    /// manual bookmarks (seconds) e.g. from the recorder hotkey
    pub bookmarks: Vec<f64>,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub title: Option<String>,
    /// "recorder" | "import" | "powder" | "download" | "clip" | "montage"
    pub source: Option<String>,
    pub imported_powder_metadata: bool,
    #[serde(default)]
    pub transcript: Vec<crate::audio::Line>,
    #[serde(default)]
    pub chat_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoEntry {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub modified: u64,
    pub sidecar: Sidecar,
}

pub fn video_id(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase().replace('\\', "/");
    hex::encode(&Sha256::digest(s.as_bytes())[..10])
}

fn sidecar_path(id: &str) -> PathBuf {
    let d = data_dir().join("library");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{id}.json"))
}

pub fn load_sidecar(p: &Path) -> Sidecar {
    std::fs::read_to_string(sidecar_path(&video_id(p))).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_sidecar(p: &Path, s: &Sidecar) -> Result<()> {
    std::fs::write(sidecar_path(&video_id(p)), serde_json::to_string_pretty(s)?)?;
    Ok(())
}

pub fn is_video(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(), Some("mp4" | "mkv" | "mov" | "webm" | "flv" | "avi" | "ts"))
}

/// Scan all library folders (2 levels deep), newest first.
pub fn scan(settings: &Settings) -> Vec<VideoEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut folders = settings.library_folders.clone();
    for extra in [&settings.recordings_folder, &settings.clips_folder, &settings.montages_folder] {
        if !folders.contains(extra) {
            folders.push(extra.clone());
        }
    }
    for f in folders {
        for e in walkdir::WalkDir::new(&f).max_depth(2).into_iter().flatten() {
            let p = e.path();
            if !p.is_file() || !is_video(p) {
                continue;
            }
            let id = video_id(p);
            if !seen.insert(id.clone()) {
                continue;
            }
            let md = e.metadata().ok();
            let mut sc = load_sidecar(p);
            if !sc.imported_powder_metadata {
                if let Some(meta) = read_powder_metadata(p) {
                    apply_powder_metadata(&mut sc, &meta);
                }
                sc.imported_powder_metadata = true;
                let _ = save_sidecar(p, &sc);
            }
            out.push(VideoEntry {
                id,
                name: p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                folder: f.display().to_string(),
                path: p.to_path_buf(),
                size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: md.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
                sidecar: sc,
            });
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

pub fn find(settings: &Settings, id: &str) -> Option<VideoEntry> {
    scan(settings).into_iter().find(|v| v.id == id)
}

/// Thumbnail JPEG (cached) at ~20% into the video or `time`.
pub fn thumbnail(ffmpeg: &Path, video: &Path, time: Option<f64>) -> Result<PathBuf> {
    let d = data_dir().join("thumbs");
    std::fs::create_dir_all(&d)?;
    let key = format!("{}_{}", video_id(video), time.map(|t| format!("{:.1}", t)).unwrap_or_default());
    let out = d.join(format!("{key}.jpg"));
    if out.exists() {
        return Ok(out);
    }
    let t = match time {
        Some(t) => t,
        None => crate::video::probe(ffmpeg, video).ok().and_then(|i| i.duration_sec).map(|d| (d * 0.2).min(60.0)).unwrap_or(1.0),
    };
    let st = Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-ss", &format!("{t:.2}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
        .arg(&out)
        .status()?;
    if !st.success() || !out.exists() {
        anyhow::bail!("thumbnail failed");
    }
    Ok(out)
}

/// Find Powder's embedded JSON metadata block (contains "powderGameId") near the file ends.
pub fn read_powder_metadata(p: &Path) -> Option<Value> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    const WIN: u64 = 8 * 1024 * 1024;
    let mut chunks = vec![(0u64, WIN.min(len))];
    if len > WIN {
        chunks.push((len - WIN, WIN));
    }
    for (off, n) in chunks {
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut buf = vec![0u8; n as usize];
        f.read_exact(&mut buf).ok()?;
        let key = b"\"powderGameId\"";
        if let Some(pos) = buf.windows(key.len()).position(|w| w == key) {
            // walk back to the outermost '{' that parses
            let mut starts: Vec<usize> = buf[..pos].iter().enumerate().rev().filter(|(_, b)| **b == b'{').map(|(i, _)| i).take(64).collect();
            starts.reverse();
            for s in starts {
                let mut depth = 0i32;
                let mut in_str = false;
                let mut esc = false;
                for (i, &b) in buf[s..].iter().enumerate() {
                    if in_str {
                        if esc { esc = false } else if b == b'\\' { esc = true } else if b == b'"' { in_str = false }
                        continue;
                    }
                    match b {
                        b'"' => in_str = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                if let Ok(v) = serde_json::from_slice::<Value>(&buf[s..s + i + 1]) {
                                    if v.to_string().contains("powderGameId") {
                                        return Some(v);
                                    }
                                }
                                break;
                            }
                        }
                        _ => {}
                    }
                    if i > 4 * 1024 * 1024 { break; }
                }
            }
        }
    }
    None
}

fn apply_powder_metadata(sc: &mut Sidecar, v: &Value) {
    // payload may be wrapped as {version, data:{...}}
    let d = if v.get("data").map(|d| d.is_object()).unwrap_or(false) { &v["data"] } else { v };
    let game = d["powderGameId"].as_str().or_else(|| d["game"]["powderGameId"].as_str()).or_else(|| d["recording"]["powderGameId"].as_str());
    if let Some(g) = game.filter(|g| !g.is_empty()) {
        sc.game = Some(g.to_string());
    }
    if let Some(evs) = d["events"].as_array() {
        for e in evs {
            let name = e["name"].as_str().or_else(|| e["eventName"].as_str()).unwrap_or("event").to_string();
            let t = e["startTimeMs"].as_f64().map(|x| x / 1000.0).or_else(|| e["timeSec"].as_f64()).or_else(|| e["time"].as_f64()).or_else(|| e["begin_frame_time"].as_f64());
            if let Some(t) = t {
                sc.events.push(LibEvent { display_name: name.clone(), name, time_sec: t, source: "video".into() });
            }
        }
        if !sc.events.is_empty() {
            sc.analyzed = true;
        }
    }
    sc.source.get_or_insert_with(|| "powder".into());
}

/// Import an analysis result into the sidecar (replacing previous game events, keeping bookmarks).
pub fn store_analysis(video: &Path, a: &crate::analyze::Analysis) -> Result<Sidecar> {
    let mut sc = load_sidecar(video);
    sc.game = Some(a.game.clone());
    sc.events.retain(|e| e.source != "video");
    for e in &a.events {
        sc.events.push(LibEvent { name: e.name.clone(), display_name: e.display_name.clone(), time_sec: e.time_sec, source: "video".into() });
    }
    sc.events.sort_by(|x, y| x.time_sec.total_cmp(&y.time_sec));
    sc.analyzed = true;
    sc.analyzed_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs());
    sc.width = Some(a.width);
    sc.height = Some(a.height);
    save_sidecar(video, &sc)?;
    Ok(sc)
}

pub fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_str(&std::fs::read_to_string(p)?).with_context(|| format!("parsing {}", p.display()))
}

/// Replace all events of one `source` ("audio", "chat", "keyword") on a video.
pub fn set_events(video: &Path, source: &str, name: &str, display: &str, items: &[(f64, String)]) -> Result<Sidecar> {
    let mut sc = load_sidecar(video);
    sc.events.retain(|e| e.source != source);
    for (t, label) in items {
        sc.events.push(LibEvent { name: name.into(), display_name: if label.is_empty() { display.into() } else { label.clone() }, time_sec: *t, source: source.into() });
    }
    sc.events.sort_by(|x, y| x.time_sec.total_cmp(&y.time_sec));
    save_sidecar(video, &sc)?;
    Ok(sc)
}
