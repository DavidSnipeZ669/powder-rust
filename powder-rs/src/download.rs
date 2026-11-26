//! VOD / clip downloads with yt-dlp (Powder ships yt-dlp.exe) and Twitch chat spikes
//! (Powder's "chat-event"): chat replay is fetched as yt-dlp "rechat" subtitles.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn ytdlp(powder: Option<&Path>) -> Result<PathBuf> {
    let name = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
    let mut c: Vec<PathBuf> = crate::paths::base_dirs().iter().map(|b| b.join(name)).collect();
    if let Some(p) = powder {
        c.push(p.join(name));
        c.push(p.join("resources").join("app.asar.unpacked").join("dist").join(name));
    }
    if let Ok(path) = std::env::var("PATH") {
        c.extend(std::env::split_paths(&path).map(|d| d.join(name)));
    }
    c.into_iter().find(|p| p.exists()).context("yt-dlp not found (Powder ships yt-dlp.exe; put it next to powder-rs.exe or set the Powder folder in Settings)")
}

/// Download `url` into `folder`. Returns (video path, optional chat json).
pub fn download(ytdlp: &Path, ffmpeg: &Path, url: &str, folder: &Path, progress: &dyn Fn(f64, &str)) -> Result<(PathBuf, Option<PathBuf>)> {
    std::fs::create_dir_all(folder)?;
    let tmpl = folder.join("%(uploader)s - %(title).80s [%(id)s].%(ext)s");
    let mut child = Command::new(ytdlp)
        .args(["--newline", "--no-playlist", "--merge-output-format", "mp4", "-f", "bv*+ba/b", "--write-subs", "--sub-langs", "rechat,live_chat", "--ffmpeg-location"])
        .arg(ffmpeg.parent().unwrap_or(Path::new(".")))
        .arg("-o")
        .arg(&tmpl)
        .args(["--print", "after_move:filepath", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting yt-dlp")?;
    let re = regex::Regex::new(r"\[download\]\s+([\d.]+)%").unwrap();
    let mut final_path = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(|l| l.ok()) {
        if let Some(c) = re.captures(&line) {
            progress(c[1].parse::<f64>().unwrap_or(0.0) / 100.0, &line);
        } else if Path::new(line.trim()).exists() {
            final_path = Some(PathBuf::from(line.trim()));
        }
    }
    let st = child.wait()?;
    if !st.success() {
        let mut e = String::new();
        use std::io::Read;
        let _ = child.stderr.take().map(|mut s| s.read_to_string(&mut e));
        bail!("yt-dlp failed: {}", e.lines().filter(|l| l.contains("ERROR")).last().unwrap_or("unknown error"));
    }
    let video = final_path.context("yt-dlp did not report the output file")?;
    let stem = video.with_extension("");
    let chat = ["rechat.json", "live_chat.json"].iter().map(|s| PathBuf::from(format!("{}.{s}", stem.display()))).find(|p| p.exists());
    Ok((video, chat))
}

/// Chat message times (seconds) from a yt-dlp rechat/live_chat file.
pub fn chat_times(chat: &Path) -> Result<Vec<f64>> {
    let txt = std::fs::read_to_string(chat)?;
    let mut times = Vec::new();
    // Twitch rechat: one JSON document with "comments"; YouTube live_chat: JSON lines
    if let Ok(v) = serde_json::from_str::<Value>(&txt) {
        if let Some(cs) = v["comments"].as_array() {
            for c in cs {
                if let Some(t) = c["content_offset_seconds"].as_f64() {
                    times.push(t);
                }
            }
        }
    }
    if times.is_empty() {
        for l in txt.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                let ms = v["replayChatItemAction"]["videoOffsetTimeMsec"].as_str().and_then(|s| s.parse::<f64>().ok());
                if let Some(ms) = ms {
                    times.push(ms / 1000.0);
                }
            }
        }
    }
    times.sort_by(|a, b| a.total_cmp(b));
    Ok(times)
}

/// Spikes in chat activity: 10 s windows well above the local average.
pub fn chat_spikes(times: &[f64]) -> Vec<f64> {
    let Some(&last) = times.last() else { return vec![] };
    let n = (last / 10.0) as usize + 1;
    let mut counts = vec![0f64; n];
    for t in times {
        counts[(*t / 10.0) as usize] += 1.0;
    }
    let mut out = Vec::new();
    for i in 0..n {
        let a = i.saturating_sub(18);
        let b = (i + 18).min(n);
        let ctx = &counts[a..b];
        let mean = ctx.iter().sum::<f64>() / ctx.len() as f64;
        let sd = (ctx.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / ctx.len() as f64).sqrt();
        if counts[i] >= 5.0 && counts[i] > mean + 2.5 * sd.max(1.0) && out.last().map(|l: &f64| i as f64 * 10.0 - l > 30.0).unwrap_or(true) {
            // chat reacts after the moment: point ~10 s earlier
            out.push((i as f64 * 10.0 - 10.0).max(0.0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn spikes_found() {
        let mut t: Vec<f64> = (0..300).step_by(7).map(|x| x as f64).collect();
        t.extend((0..40).map(|i| 120.0 + i as f64 * 0.2));
        t.sort_by(|a, b| a.total_cmp(b));
        let s = super::chat_spikes(&t);
        assert_eq!(s, vec![110.0]);
    }
}
