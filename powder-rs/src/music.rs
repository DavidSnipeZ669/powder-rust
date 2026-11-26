//! Music for montages: Powder's bundled licensed tracks (renamed .pow audio files inside the
//! Powder install) plus any mp3/ogg/wav/flac/m4a in the user's music folders.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub duration: f64,
    pub source: String,
}

/// Known titles of Powder's bundled music (from Powder's music_list, by duration in seconds).
const POWDER_TRACKS: &[(&str, &str, u32)] = &[
    ("Controwla - Predator", "LEXOFLEX", 108),
    ("Cyberpunk 2088", "TAZDEV", 110),
    ("Cyberpunk", "ENERGYSOUND", 106),
    ("Cyberpunk Powerful (loop)", "ALEXZAVESA", 0),
    ("Dance Baby", "ELIOT NESS", 0),
    ("Drive Rock Epic", "FUTURE NEAR RECORDS", 0),
];

fn probe_audio(ffmpeg: &Path, p: &Path) -> Option<(f64, String, String)> {
    let out = Command::new(ffmpeg).args(["-hide_banner", "-i"]).arg(p).output().ok()?;
    let t = String::from_utf8_lossy(&out.stderr);
    if !t.contains("Audio:") || t.contains("Video: mjpeg") && !t.contains("Audio:") {
        return None;
    }
    let dur = regex::Regex::new(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)").unwrap().captures(&t).map(|c| {
        c[1].parse::<f64>().unwrap() * 3600.0 + c[2].parse::<f64>().unwrap() * 60.0 + c[3].parse::<f64>().unwrap()
    })?;
    let tag = |k: &str| regex::Regex::new(&format!(r"(?mi)^\s*{k}\s*:\s*(.+)$")).unwrap().captures(&t).map(|c| c[1].trim().to_string());
    Some((dur, tag("title").unwrap_or_default(), tag("artist").unwrap_or_default()))
}

pub fn list(ffmpeg: &Path, powder_dir: Option<&Path>, folders: &[PathBuf]) -> Vec<Track> {
    let mut v = Vec::new();
    if let Some(pd) = powder_dir {
        for d in [pd.join("resources").join("app.asar.unpacked").join("dist").join("main"), pd.join("resources").join("app-unpacked").join("dist").join("main")] {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let big = e.metadata().map(|m| m.len() > 1_000_000).unwrap_or(false);
                if p.extension().map(|x| x == "pow").unwrap_or(false) && big {
                    if let Some((dur, title, artist)) = probe_audio(ffmpeg, &p) {
                        let known = POWDER_TRACKS.iter().find(|t| t.2 > 0 && (t.2 as f64 - dur).abs() < 2.0);
                        let (t, a) = match known {
                            Some(k) => (k.0.to_string(), k.1.to_string()),
                            None if !title.is_empty() => (title, artist),
                            None => (format!("Powder track {}", &p.file_stem().unwrap().to_string_lossy()[..6]), String::new()),
                        };
                        v.push(Track { path: p, title: t, artist: a, duration: dur, source: "powder".into() });
                    }
                }
            }
            if !v.is_empty() { break; }
        }
    }
    for f in folders {
        for e in walkdir::WalkDir::new(f).max_depth(3).into_iter().flatten() {
            let p = e.path();
            let ok = matches!(p.extension().and_then(|x| x.to_str()).map(|x| x.to_ascii_lowercase()).as_deref(), Some("mp3" | "ogg" | "wav" | "flac" | "m4a" | "opus"));
            if ok {
                if let Some((dur, title, artist)) = probe_audio(ffmpeg, p) {
                    let t = if title.is_empty() { p.file_stem().unwrap().to_string_lossy().into_owned() } else { title };
                    v.push(Track { path: p.to_path_buf(), title: t, artist, duration: dur, source: "user".into() });
                }
            }
        }
    }
    v
}
