//! Picks the best available H.264 encoder in the ffmpeg build and builds its arguments.
//! Hardware first (NVIDIA NVENC, AMD AMF, Intel QSV, Windows Media Foundation), then libx264.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

static ENCODERS: OnceLock<Vec<String>> = OnceLock::new();

pub fn available(ffmpeg: &Path) -> &'static Vec<String> {
    ENCODERS.get_or_init(|| {
        let out = Command::new(ffmpeg).args(["-hide_banner", "-encoders"]).output();
        let txt = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        txt.lines()
            .filter_map(|l| {
                let p: Vec<&str> = l.split_whitespace().collect();
                (p.len() >= 2 && p[0].starts_with('V')).then(|| p[1].to_string())
            })
            .collect()
    })
}

static WORKING: OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = OnceLock::new();

/// Encoders can be compiled in but unusable (no matching GPU / driver): try one small frame.
pub fn works(ffmpeg: &Path, enc: &str) -> bool {
    let m = WORKING.get_or_init(Default::default);
    if let Some(v) = m.lock().unwrap().get(enc) {
        return *v;
    }
    let mut a: Vec<String> = vec!["-hide_banner".into(), "-v".into(), "error".into(), "-f".into(), "lavfi".into(), "-i".into(), "color=c=black:s=640x360:r=30".into(), "-frames:v".into(), "5".into()];
    a.extend(args(enc, "medium", 360));
    a.extend(["-f".into(), "null".into(), "-".into()]);
    let ok = Command::new(ffmpeg).args(&a).output().map(|o| o.status.success()).unwrap_or(false);
    m.lock().unwrap().insert(enc.to_string(), ok);
    ok
}

/// Best working H.264 encoder; `preferred` may name one explicitly ("auto" = choose).
pub fn pick(ffmpeg: &Path, preferred: &str) -> String {
    let av = available(ffmpeg);
    if preferred != "auto" && av.iter().any(|e| e == preferred) && works(ffmpeg, preferred) {
        return preferred.to_string();
    }
    for e in ["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf", "libx264", "libopenh264", "mpeg4"] {
        if av.iter().any(|x| x == e) && works(ffmpeg, e) {
            return e.to_string();
        }
    }
    "libx264".into()
}

/// All encoders with their status (for the settings page).
pub fn report(ffmpeg: &Path) -> Vec<(String, bool)> {
    let av = available(ffmpeg);
    ["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf", "libx264"].iter().filter(|e| av.iter().any(|x| x == *e)).map(|e| (e.to_string(), works(ffmpeg, e))).collect()
}

/// Encoder arguments for a target quality ("high"|"medium"|"low") and height.
pub fn args(encoder: &str, quality: &str, height: u32) -> Vec<String> {
    let base_mbps = match height { h if h >= 2000 => 40.0, h if h >= 1400 => 24.0, h if h >= 1000 => 14.0, _ => 8.0 };
    let mult = match quality { "low" => 0.5, "medium" => 0.75, _ => 1.0 };
    let br = format!("{}M", (base_mbps * mult) as u32);
    let mut a: Vec<String> = vec!["-c:v".into(), encoder.into()];
    match encoder {
        "libx264" => a.extend(["-preset", "veryfast", "-crf", if quality == "high" { "19" } else if quality == "medium" { "22" } else { "26" }].map(String::from)),
        "h264_nvenc" => a.extend(["-preset", "p4", "-rc", "vbr", "-b:v", &br, "-maxrate", &br].map(String::from)),
        "h264_amf" => a.extend(["-quality", "balanced", "-rc", "vbr_peak", "-b:v", &br, "-maxrate", &br].map(String::from)),
        "h264_qsv" => a.extend(["-preset", "faster", "-b:v", &br].map(String::from)),
        "h264_mf" => a.extend(["-hw_encoding", "1", "-b:v", &br, "-rate_control", "cbr"].map(String::from)),
        _ => a.extend(["-b:v", &br].map(String::from)),
    }
    a.extend(["-pix_fmt", if encoder == "h264_mf" || encoder == "h264_qsv" { "nv12" } else { "yuv420p" }].map(String::from));
    a
}
