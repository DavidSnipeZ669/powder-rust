//! Video helpers used by many features: stats, poster thumbnails and base64 thumbnails.
//! Uses Powder's native engine like the original (video_stats, thumbnail, thumbnail_at), and falls back
//! to the ffmpeg.exe Powder ships when the native engine isn't available.

use crate::gpu::HiddenExt;
use crate::native::{Accel, NativeProcess};
use anyhow::{bail, Context, Result};
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::OnceLock;
use std::time::Duration;

static FFMPEG: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn set_ffmpeg(p: Option<PathBuf>) {
    let _ = FFMPEG.set(p);
}

pub fn ffmpeg() -> Option<PathBuf> {
    FFMPEG.get().cloned().flatten().or_else(|| {
        let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|f| f.exists()))
    })
}

const NO_ACCEL: Accel = Accel { nvidia: false, intel: false, amd: false };

/// Run a one-shot native command and wait for one of `events`; returns that event's data.
fn native_once(cmd: &str, args: Vec<String>, accel: Accel, events: &'static [&'static str], start: Option<(&str, Value)>, timeout: Duration) -> Result<(String, Value)> {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let p = NativeProcess::start(
        cmd,
        args,
        accel,
        Box::new(move |name, data| {
            if events.contains(&name) || name.ends_with("ERROR_JSON") || name == "connection-closed" {
                let _ = tx.lock().send((name.to_string(), data.clone()));
            }
        }),
    )?;
    if let Some((c, a)) = start {
        let _ = p.push(c, Some(&a));
    }
    let r = rx.recv_timeout(timeout);
    p.kill();
    let (name, data) = r.context("native engine timed out")?;
    if name.ends_with("ERROR_JSON") || name == "connection-closed" {
        bail!("{cmd}: {name} {data}");
    }
    Ok((name, data))
}

#[derive(Clone, Debug)]
pub struct Meta {
    pub size: u64,
    pub created_at: u128,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub framerate: f64,
}

impl Meta {
    pub fn to_json(&self) -> Value {
        json!({"size": self.size, "createdAt": self.created_at as u64, "duration": self.duration_ms, "width": self.width, "height": self.height, "framerate": self.framerate})
    }
}

fn meta_cache() -> &'static Mutex<HashMap<PathBuf, Meta>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Meta>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// src/main/lib/readVideoMetadata.ts
pub fn metadata(path: &Path) -> Result<Meta> {
    if let Some(m) = meta_cache().lock().get(path) {
        return Ok(m.clone());
    }
    let md = std::fs::metadata(path)?;
    if md.len() == 0 {
        bail!("empty file");
    }
    let created = md.created().or_else(|_| md.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
    let (dur, w, h, fps) = match native_once("video_stats", vec!["--video".into(), path.display().to_string()], NO_ACCEL, &["VIDEO_STATS"], None, Duration::from_secs(60)) {
        Ok((_, v)) if v["succeeded"] == true => (
            v["durationSeconds"].as_f64().unwrap_or(0.0),
            v["width"].as_u64().unwrap_or(0) as u32,
            v["height"].as_u64().unwrap_or(0) as u32,
            v["normalizedFrameRate"].as_f64().unwrap_or(0.0),
        ),
        _ => probe_ffmpeg(path)?,
    };
    let m = Meta { size: md.len(), created_at: created, duration_ms: (dur * 1000.0).round() as u64, width: w, height: h, framerate: fps };
    meta_cache().lock().insert(path.to_path_buf(), m.clone());
    Ok(m)
}

/// ffmpeg -i prints "Duration: 00:01:02.50" and "1920x1080 ... 60 fps"
fn probe_ffmpeg(path: &Path) -> Result<(f64, u32, u32, f64)> {
    let ff = ffmpeg().context("no ffmpeg")?;
    let out = Command::new(ff).arg("-hide_banner").arg("-i").arg(path).stdout(Stdio::null()).stderr(Stdio::piped()).creation_flags_hidden().output()?;
    let s = String::from_utf8_lossy(&out.stderr);
    let dur = s.split("Duration: ").nth(1).and_then(|r| r.split(',').next()).and_then(|t| {
        let p: Vec<f64> = t.trim().split(':').filter_map(|x| x.parse().ok()).collect();
        (p.len() == 3).then(|| p[0] * 3600.0 + p[1] * 60.0 + p[2])
    });
    let video = s.lines().find(|l| l.contains("Video:")).context("no video stream")?;
    let re_wh = video.split([' ', ',']).find_map(|tok| {
        let (a, b) = tok.split_once('x')?;
        Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))
    });
    let fps = video.split(',').find_map(|p| p.trim().strip_suffix(" fps").and_then(|x| x.parse::<f64>().ok())).unwrap_or(0.0);
    let (w, h) = re_wh.context("no size")?;
    Ok((dur.unwrap_or(0.0), w, h, fps))
}

pub fn thumbnail_folder() -> PathBuf {
    std::env::temp_dir().join("Powder").join("thumbnails")
}

/// thumbnail.ensureSnapshot: cached JPEG poster for a video.
pub fn ensure_snapshot(video: &Path) -> Option<PathBuf> {
    use md5::Digest;
    if !video.exists() {
        return None;
    }
    let hash = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(md5::Md5::digest(video.to_string_lossy().as_bytes()));
    let out = thumbnail_folder().join(format!("snapshot.{hash}.jpg"));
    if out.exists() {
        return Some(out);
    }
    let _ = std::fs::create_dir_all(thumbnail_folder());
    let meta = metadata(video).ok()?;
    let native = native_once(
        "thumbnail",
        vec!["--video".into(), video.display().to_string(), "--out".into(), out.display().to_string(), "--width".into(), meta.width.to_string(), "--height".into(), meta.height.to_string()],
        Accel::from_settings(),
        &["THUMBNAIL_GENERATED"],
        None,
        Duration::from_secs(60),
    );
    if matches!(native, Ok((_, Value::Bool(true)))) && out.exists() {
        return Some(out);
    }
    let t = (meta.duration_ms as f64 / 1000.0 * 0.1).min(5.0);
    ffmpeg_frame(video, t, meta.width.min(1280), None, &out).ok()?;
    out.exists().then_some(out)
}

fn ffmpeg_frame(video: &Path, t: f64, width: u32, height: Option<u32>, out: &Path) -> Result<()> {
    let ff = ffmpeg().context("no ffmpeg")?;
    let scale = match height {
        Some(h) => format!("scale={width}:{h}"),
        None => format!("scale={width}:-2"),
    };
    let st = Command::new(ff)
        .args(["-v", "error", "-y", "-ss", &format!("{t:.3}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", &scale])
        .arg(out)
        .creation_flags_hidden()
        .status()?;
    if !st.success() {
        bail!("ffmpeg failed");
    }
    Ok(())
}

/// thumbnail_at: base64 PNG thumbnails at several times. Returns id -> base64.
pub fn b64_thumbnails(video: &Path, times: &[(String, f64)], width: u32, height: u32) -> Result<HashMap<String, String>> {
    let input = json!({
        "videoPath": video.display().to_string(),
        "times": times.iter().map(|(id, t)| json!({"id": id, "timeInS": t})).collect::<Vec<_>>(),
        "width": width, "height": height
    });
    if let Ok((_, v)) = native_once("thumbnail_at", vec![], Accel::from_settings(), &["thumbnail_finished"], Some(("start", input)), Duration::from_secs(120)) {
        let mut m = HashMap::new();
        for o in v.as_array().cloned().unwrap_or_default() {
            m.insert(o["id"].as_str().unwrap_or("").to_string(), o["thumbnail"].as_str().unwrap_or("").to_string());
        }
        return Ok(m);
    }
    let mut m = HashMap::new();
    let tmp = std::env::temp_dir().join(format!("pw-thumb-{}.png", std::process::id()));
    for (id, t) in times {
        ffmpeg_frame(video, *t, width.max(2), Some(height.max(2)), &tmp)?;
        m.insert(id.clone(), base64::engine::general_purpose::STANDARD.encode(std::fs::read(&tmp)?));
    }
    let _ = std::fs::remove_file(tmp);
    Ok(m)
}

/// Duration of any media file (music-metadata's parseFile in the original), via ffmpeg.
pub fn duration_secs(path: &Path) -> Option<f64> {
    let ff = ffmpeg()?;
    let out = Command::new(ff).arg("-hide_banner").arg("-i").arg(path).stdout(Stdio::null()).stderr(Stdio::piped()).creation_flags_hidden().output().ok()?;
    let s = String::from_utf8_lossy(&out.stderr);
    let t = s.split("Duration: ").nth(1)?.split(',').next()?;
    let p: Vec<f64> = t.trim().split(':').filter_map(|x| x.parse().ok()).collect();
    (p.len() == 3).then(|| p[0] * 3600.0 + p[1] * 60.0 + p[2])
}
