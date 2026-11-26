//! Audio features: loudness spikes (Powder's "loudness-peak" event), transcription with
//! whisper.cpp (optional `transcribe` feature; uses Powder's bundled ggml-base.bin model) and
//! keyword events from the transcript.

use anyhow::{bail, Result};
#[cfg(feature = "transcribe")]
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Decode a video's audio as mono f32 at `rate` Hz.
pub fn decode(ffmpeg: &Path, video: &Path, rate: u32) -> Result<Vec<f32>> {
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-vn", "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut buf = Vec::new();
    child.stdout.as_mut().unwrap().read_to_end(&mut buf)?;
    child.wait()?;
    if buf.is_empty() {
        bail!("no audio track");
    }
    Ok(buf.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Moments that are much louder than their surroundings (shouting, explosions, hype).
/// Returns event times in seconds.
pub fn loudness_peaks(samples: &[f32], rate: u32, sensitivity_db: f32) -> Vec<f64> {
    let win = (rate as usize) / 2; // 0.5 s windows
    let db: Vec<f32> = samples
        .chunks(win)
        .map(|c| {
            let rms = (c.iter().map(|x| x * x).sum::<f32>() / c.len().max(1) as f32).sqrt();
            20.0 * rms.max(1e-6).log10()
        })
        .collect();
    let n = db.len();
    let half = 60; // 30 s each side
    let mut out = Vec::new();
    let mut last = -10.0f64;
    for i in 0..n {
        let a = i.saturating_sub(half);
        let b = (i + half).min(n);
        let mut ctx: Vec<f32> = db[a..b].to_vec();
        ctx.sort_by(|x, y| x.total_cmp(y));
        let median = ctx[ctx.len() / 2];
        let t = i as f64 * 0.5;
        if db[i] > -30.0 && db[i] - median > sensitivity_db && t - last > 4.0 {
            // need the loud part to last at least two windows (1 s)
            if i + 1 < n && db[i + 1] - median > sensitivity_db * 0.7 {
                out.push(t);
                last = t;
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    pub from: f64,
    pub to: f64,
    pub text: String,
}

/// Find the whisper model: models/whisper/*.bin next to the exe, or Powder's ai-configs/whisper.
pub fn find_model(powder: Option<&Path>) -> Option<PathBuf> {
    let mut cands = Vec::new();
    for b in crate::paths::base_dirs() {
        cands.push(b.join("models").join("whisper"));
    }
    if let Some(p) = powder {
        cands.push(p.join("ai-configs").join("whisper"));
    }
    for d in cands {
        if let Ok(rd) = std::fs::read_dir(&d) {
            let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "bin").unwrap_or(false)).collect();
            v.sort();
            if let Some(p) = v.into_iter().next() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(feature = "transcribe")]
pub fn transcribe(ffmpeg: &Path, video: &Path, model: &Path, language: &str, progress: &dyn Fn(f64)) -> Result<Vec<Line>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
    let samples = decode(ffmpeg, video, 16000)?;
    whisper_rs::install_logging_hooks(); // keep whisper.cpp's console spam out of the output
    let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default()).context("loading whisper model")?;
    let mut state = ctx.create_state().context("whisper state")?;
    let mut out = Vec::new();
    // 10-minute chunks keep memory bounded and give progress
    let chunk = 16000 * 600;
    let total = samples.len().max(1);
    for (ci, part) in samples.chunks(chunk).enumerate() {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if language != "auto" {
            p.set_language(Some(language));
        }
        p.set_print_progress(false);
        p.set_print_realtime(false);
        p.set_print_special(false);
        p.set_print_timestamps(false);
        state.full(p, part).context("whisper")?;
        let base = (ci * chunk) as f64 / 16000.0;
        for seg in state.as_iter() {
            let text = seg.to_str_lossy().map(|s| s.trim().to_string()).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            out.push(Line { from: base + seg.start_timestamp() as f64 / 100.0, to: base + seg.end_timestamp() as f64 / 100.0, text });
        }
        progress(((ci + 1) * chunk).min(total) as f64 / total as f64);
    }
    Ok(out)
}

#[cfg(not(feature = "transcribe"))]
pub fn transcribe(_ffmpeg: &Path, _video: &Path, _model: &Path, _language: &str, _progress: &dyn Fn(f64)) -> Result<Vec<Line>> {
    bail!("this build has no transcription - rebuild with `cargo build --release --features transcribe` (build.bat tries this automatically)")
}

/// Keyword events: transcript lines containing any of the words (case-insensitive).
pub fn keyword_hits(lines: &[Line], keywords: &[String]) -> Vec<(f64, String)> {
    let kws: Vec<String> = keywords.iter().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
    lines
        .iter()
        .filter_map(|l| {
            let low = l.text.to_lowercase();
            kws.iter().find(|k| low.contains(k.as_str())).map(|k| (l.from, format!("\"{}\" - {}", k, l.text)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn keywords() {
        let l = vec![super::Line { from: 5.0, to: 6.0, text: "No WAY that was clutch".into() }, super::Line { from: 9.0, to: 10.0, text: "ok".into() }];
        let h = super::keyword_hits(&l, &["clutch".into(), " ".into()]);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].0, 5.0);
    }
}
