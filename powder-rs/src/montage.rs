//! Auto-montage (port of Powder's automatic montage): pick the best moments from one or more
//! analysed videos, apply Powder's effect bundles (slow-mo / speed-up + visual effects), add music,
//! optional vertical 9:16 crop and an outro.
//!
//! Effects use the same config format as Powder (`templates/montage_effects.json`, or
//! `<Powder>/ai-configs/automatic_montage/config.json`), rendered with ffmpeg filters.

use crate::library::LibEvent;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_EFFECTS: &str = include_str!("../templates/montage_effects.json");

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeWarp {
    pub time_factor: f64,
    pub begin_offset_from_event: f64,
    pub end_offset_from_event: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualFx {
    pub effect_name: String,
    pub begin_offset_from_event: f64,
    pub duration_in_sec: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectBundle {
    pub bundle_name: String,
    #[serde(default)]
    pub time_warping_effects: Vec<TimeWarp>,
    #[serde(default)]
    pub visual_effects: Vec<VisualFx>,
}

pub fn load_effects(path: Option<&Path>) -> Vec<EffectBundle> {
    let txt = path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_else(|| DEFAULT_EFFECTS.to_string());
    let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
    serde_json::from_value(v["effectBundles"].clone()).unwrap_or_default()
}

/// Per-event montage settings (from the game's events.json `automontage` block).
#[derive(Debug, Clone)]
pub struct EventRule {
    pub score: f64,
    pub primary: bool,
    pub secondary: bool,
    pub before: f64,
    pub after: f64,
    pub effects: Vec<String>,
}

pub fn rule_for(events_json: Option<&Value>, ev: &LibEvent) -> EventRule {
    if ev.source == "bookmark" || ev.name == "bookmark" {
        return EventRule { score: 25.0, primary: true, secondary: false, before: 6.0, after: 3.0, effects: vec![] };
    }
    let def = events_json
        .and_then(|e| e["events"].as_array())
        .and_then(|a| a.iter().find(|x| x["name"] == ev.name.as_str()))
        .cloned()
        .or_else(|| {
            let cat: Value = serde_json::from_str(crate::discover::EVENT_CATALOG).ok()?;
            cat["events"].as_array()?.iter().find(|x| x["name"] == ev.name.as_str()).cloned()
        });
    let am = def.as_ref().map(|d| d["automontage"].clone()).unwrap_or(Value::Null);
    EventRule {
        score: def.as_ref().and_then(|d| d["eventScore"].as_f64()).unwrap_or(10.0),
        primary: am["primary"].as_bool().unwrap_or(true),
        secondary: am["secondary"].as_bool().unwrap_or(false),
        before: am["offsetBefore"].as_f64().filter(|x| *x > 0.0).unwrap_or(2.0),
        after: am["offsetAfter"].as_f64().filter(|x| *x > 0.0).unwrap_or(2.0),
        effects: am["effectTypes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    pub video: PathBuf,
    pub events: Vec<LibEvent>,
    /// the game's events.json (for scores / montage settings)
    pub events_json: Option<Value>,
    pub duration: f64,
    pub has_audio: bool,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedClip {
    pub source: usize,
    pub start: f64,
    pub end: f64,
    /// main event time (absolute in source)
    pub event_time: f64,
    pub event_name: String,
    pub score: f64,
    pub effect: Option<String>,
    /// output duration after time warps
    pub out_duration: f64,
}

#[derive(Debug, Clone)]
pub struct MontageOptions {
    pub target_sec: f64,
    pub music: Option<PathBuf>,
    pub music_start: f64,
    pub music_volume: f64,
    pub game_volume: f64,
    pub vertical: bool,
    /// left,top,width,height crop (fractions) used for vertical; default = Powder's centre template
    pub vertical_crop: [f64; 4],
    pub height: u32,
    pub fps: u32,
    pub effects: bool,
    pub outro: Option<PathBuf>,
    pub seed: u64,
    pub encoder: String,
    pub quality: String,
}

impl Default for MontageOptions {
    fn default() -> Self {
        Self {
            target_sec: 60.0,
            music: None,
            music_start: 0.0,
            music_volume: 0.8,
            game_volume: 0.6,
            vertical: false,
            vertical_crop: [0.34197530864, 0.0, 0.31604938271, 1.0],
            height: 1080,
            fps: 60,
            effects: true,
            outro: None,
            seed: 1,
            encoder: "auto".into(),
            quality: "high".into(),
        }
    }
}

fn warp_saving(bundle: Option<&EffectBundle>) -> f64 {
    bundle.map(|b| b.time_warping_effects.iter().map(|w| (w.end_offset_from_event - w.begin_offset_from_event) * (1.0 - 1.0 / w.time_factor)).sum()).unwrap_or(0.0)
}

/// Choose the moments. Highest-scoring windows first until the target length is reached,
/// then played back in chronological order.
pub fn plan(sources: &[Source], opt: &MontageOptions, bundles: &[EffectBundle]) -> Vec<PlannedClip> {
    let mut rng_state = opt.seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = || {
        rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng_state >> 33) as usize
    };
    let mut cands: Vec<PlannedClip> = Vec::new();
    for (si, s) in sources.iter().enumerate() {
        let mut wins: Vec<PlannedClip> = Vec::new();
        for ev in &s.events {
            let r = rule_for(s.events_json.as_ref(), ev);
            if !r.primary && !r.secondary {
                continue;
            }
            let effect = if opt.effects && r.primary && !r.effects.is_empty() {
                let e = &r.effects[next() % r.effects.len()];
                bundles.iter().any(|b| &b.bundle_name == e).then(|| e.clone())
            } else {
                None
            };
            let b = effect.as_ref().and_then(|e| bundles.iter().find(|x| &x.bundle_name == e));
            // make room for the effect's pre-roll / post-roll
            let pre = b.map(|b| b.time_warping_effects.iter().map(|w| -w.begin_offset_from_event).chain(b.visual_effects.iter().map(|v| -v.begin_offset_from_event)).fold(0.0, f64::max)).unwrap_or(0.0);
            let post = b.map(|b| b.time_warping_effects.iter().map(|w| w.end_offset_from_event).chain(b.visual_effects.iter().map(|v| v.begin_offset_from_event + v.duration_in_sec)).fold(0.0, f64::max)).unwrap_or(0.0);
            let start = (ev.time_sec - r.before.max(pre + 0.3)).max(0.0);
            let end = (ev.time_sec + r.after.max(post + 0.3)).min(if s.duration > 0.0 { s.duration } else { f64::MAX });
            if end - start < 0.5 {
                continue;
            }
            let score = r.score * if r.primary { 1.0 } else { 0.6 };
            wins.push(PlannedClip { source: si, start, end, event_time: ev.time_sec, event_name: ev.name.clone(), score, effect, out_duration: 0.0 });
        }
        // merge overlapping windows of the same video (scores add up; strongest event keeps the effect)
        wins.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut merged: Vec<PlannedClip> = Vec::new();
        for w in wins {
            if let Some(m) = merged.last_mut() {
                if w.start < m.end && w.end.max(m.end) - m.start <= 12.0 {
                    if w.score > m.score {
                        m.event_time = w.event_time;
                        m.event_name = w.event_name.clone();
                        m.effect = w.effect.clone();
                    }
                    m.end = m.end.max(w.end);
                    m.score += w.score;
                    continue;
                }
            }
            merged.push(w);
        }
        cands.extend(merged);
    }
    for c in cands.iter_mut() {
        let b = c.effect.as_ref().and_then(|e| bundles.iter().find(|x| &x.bundle_name == e));
        c.out_duration = (c.end - c.start - warp_saving(b)).max(0.3);
    }
    let mut by_score = cands.clone();
    by_score.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut picked = Vec::new();
    let mut total = 0.0;
    for c in by_score {
        if total >= opt.target_sec {
            break;
        }
        total += c.out_duration;
        picked.push(c);
    }
    picked.sort_by(|a, b| a.source.cmp(&b.source).then(a.start.total_cmp(&b.start)));
    picked
}

/// Filter chain for one visual effect; `spatial` effects are rendered on a copy and overlaid
/// only inside their time window.
fn fx_filter(name: &str, w: u32, h: u32) -> Option<(bool, String)> {
    let zoom = |f: f64| format!("scale=iw*{f}:ih*{f},crop={w}:{h}");
    let shake = format!("scale=iw*1.08:ih*1.08,crop={w}:{h}:x='(iw-ow)/2+(iw-ow)/2*sin(n*1.7)':y='(ih-oh)/2+(ih-oh)/2*cos(n*2.3)'");
    Some(match name {
        "saturation" => (false, "eq=saturation=1.9".into()),
        "grayscale" => (false, "hue=s=0".into()),
        "flash" => (false, "eq=brightness=0.35:contrast=1.15".into()),
        "shake" => (true, shake),
        "shake_color_distortion" => (true, format!("{shake},rgbashift=rh=-10:bh=10")),
        "elastic_zoom" => (true, zoom(1.15)),
        "hold_zoom_and_release" => (true, zoom(1.10)),
        "bokeh_blur" => (true, "gblur=sigma=12".into()),
        "screen_distortion" => (true, "rgbashift=rh=-14:gv=6:bh=14,noise=alls=18:allf=t".into()),
        "screen_distortion_crop" => (true, format!("{},rgbashift=rh=-12:bh=12", zoom(1.08))),
        "lens_distortion" => (true, format!("lenscorrection=k1=-0.28:k2=0.08,{}", zoom(1.12))),
        _ => return None, // e.g. "speed" - not a visual filter
    })
}

fn atempo_chain(f: f64) -> String {
    let mut parts = Vec::new();
    let mut r = f;
    while r > 2.0 { parts.push("atempo=2.0".to_string()); r /= 2.0; }
    while r < 0.5 { parts.push("atempo=0.5".to_string()); r /= 0.5; }
    parts.push(format!("atempo={r:.4}"));
    parts.join(",")
}

fn ff(ffmpeg: &Path, args: &[String]) -> Result<()> {
    let out = Command::new(ffmpeg).args(["-hide_banner", "-v", "error", "-y"]).args(args).output().context("running ffmpeg")?;
    if !out.status.success() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

pub fn output_size(opt: &MontageOptions) -> (u32, u32) {
    let h = opt.height.max(240) / 2 * 2;
    if opt.vertical { ((h as f64 * 9.0 / 16.0 / 2.0).round() as u32 * 2, h) } else { ((h as f64 * 16.0 / 9.0 / 2.0).round() as u32 * 2, h) }
}

/// Render one planned clip to `out` (normalised size/fps/audio so clips can be concatenated).
pub fn render_clip(ffmpeg: &Path, s: &Source, c: &PlannedClip, bundles: &[EffectBundle], opt: &MontageOptions, enc: &[String], out: &Path) -> Result<f64> {
    let (w, h) = output_size(opt);
    let len = c.end - c.start;
    let te = c.event_time - c.start;
    let b = c.effect.as_ref().and_then(|e| bundles.iter().find(|x| &x.bundle_name == e));
    let mut g = String::new();
    // base: crop for vertical, scale/pad to output, fps
    if opt.vertical {
        let [x, y, cw, ch] = opt.vertical_crop;
        g += &format!("[0:v]crop=iw*{cw}:ih*{ch}:iw*{x}:ih*{y},scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},fps={},setsar=1,format=yuv420p[v0];", opt.fps);
    } else {
        g += &format!("[0:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps={},setsar=1,format=yuv420p[v0];", opt.fps);
    }
    let mut cur = "v0".to_string();
    let mut k = 0;
    if let Some(b) = b {
        for v in &b.visual_effects {
            let a = (te + v.begin_offset_from_event).max(0.0);
            let e = (a + v.duration_in_sec).min(len);
            if e <= a { continue; }
            let Some((spatial, chain)) = fx_filter(&v.effect_name, w, h) else { continue };
            k += 1;
            let en = format!("enable='between(t,{a:.3},{e:.3})'");
            if spatial {
                g += &format!("[{cur}]split[fa{k}][fb{k}];[fb{k}]{chain}[fc{k}];[fa{k}][fc{k}]overlay={en}[fx{k}];");
            } else {
                g += &format!("[{cur}]{chain}:{en}[fx{k}];");
            }
            cur = format!("fx{k}");
        }
    }
    // audio
    if s.has_audio {
        g += &format!("[0:a]aresample=48000,aformat=channel_layouts=stereo,volume={}[a0];", opt.game_volume);
    } else {
        g += &format!("anullsrc=r=48000:cl=stereo,atrim=0:{len:.3}[a0];");
    }
    // time warps -> segments
    let mut cuts: Vec<(f64, f64, f64)> = Vec::new();
    if let Some(b) = b {
        let mut ws: Vec<(f64, f64, f64)> = b.time_warping_effects.iter().map(|tw| ((te + tw.begin_offset_from_event).max(0.0), (te + tw.end_offset_from_event).min(len), tw.time_factor)).filter(|x| x.1 - x.0 > 0.05).collect();
        ws.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut t = 0.0;
        for (a, e, f) in ws {
            let a = a.max(t);
            if e <= a { continue; }
            if a - t > 0.05 { cuts.push((t, a, 1.0)); }
            cuts.push((a, e, f));
            t = e;
        }
        if len - t > 0.05 { cuts.push((t, len, 1.0)); }
    }
    let out_len: f64;
    if cuts.iter().any(|c| (c.2 - 1.0).abs() > 1e-6) {
        let n = cuts.len();
        g += &format!("[{cur}]split={n}{};", (0..n).map(|i| format!("[vs{i}]")).collect::<String>());
        g += &format!("[a0]asplit={n}{};", (0..n).map(|i| format!("[as{i}]")).collect::<String>());
        let mut cat = String::new();
        for (i, (a, e, f)) in cuts.iter().enumerate() {
            g += &format!("[vs{i}]trim=start={a:.3}:end={e:.3},setpts=(PTS-STARTPTS)/{f}[vt{i}];");
            g += &format!("[as{i}]atrim=start={a:.3}:end={e:.3},asetpts=PTS-STARTPTS,{}[at{i}];", atempo_chain(*f));
            cat += &format!("[vt{i}][at{i}]");
        }
        g += &format!("{cat}concat=n={n}:v=1:a=1[vc][ac];");
        cur = "vc".into();
        out_len = cuts.iter().map(|(a, e, f)| (e - a) / f).sum();
        g += &format!("[ac]anull[a1];");
    } else {
        out_len = len;
        g += "[a0]anull[a1];";
    }
    let fd = 0.12f64.min(out_len / 4.0);
    g += &format!("[{cur}]fade=t=in:st=0:d={fd:.3},fade=t=out:st={:.3}:d={fd:.3},fps={}[vout];", (out_len - fd).max(0.0), opt.fps);
    g += &format!("[a1]afade=t=in:st=0:d={fd:.3},afade=t=out:st={:.3}:d={fd:.3}[aout]", (out_len - fd).max(0.0));
    let mut args: Vec<String> = vec!["-ss".into(), format!("{:.3}", c.start), "-t".into(), format!("{len:.3}"), "-i".into(), s.video.display().to_string()];
    args.extend(["-filter_complex".into(), g, "-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()]);
    args.extend(enc.iter().cloned());
    args.extend(["-r".into(), opt.fps.to_string(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-ar".into(), "48000".into(), "-ac".into(), "2".into()]);
    args.push(out.display().to_string());
    ff(ffmpeg, &args)?;
    Ok(out_len)
}

pub fn probe_source(ffmpeg: &Path, video: &Path) -> Result<(f64, bool, u32, u32)> {
    let out = Command::new(ffmpeg).args(["-hide_banner", "-i"]).arg(video).output()?;
    let t = String::from_utf8_lossy(&out.stderr);
    let info = crate::video::probe(ffmpeg, video)?;
    Ok((info.duration_sec.unwrap_or(0.0), t.contains("Audio:"), info.width, info.height))
}

/// Full montage: plan, render clips, join, music, outro.
pub fn make(ffmpeg: &Path, sources: &[Source], opt: &MontageOptions, bundles: &[EffectBundle], out: &Path, progress: &dyn Fn(f64, &str)) -> Result<Value> {
    let plan = plan(sources, opt, bundles);
    if plan.is_empty() {
        bail!("no usable events in the selected videos (analyse them first, or add bookmarks)");
    }
    let tmp = crate::settings::data_dir().join("tmp").join(format!("montage_{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let encoder = crate::encode::pick(ffmpeg, &opt.encoder);
    let (_, h) = output_size(opt);
    let enc = crate::encode::args(&encoder, &opt.quality, h);
    let mut list = String::new();
    let mut total = 0.0;
    for (i, c) in plan.iter().enumerate() {
        progress(i as f64 / (plan.len() as f64 + 1.0), &format!("rendering clip {}/{} ({}{})", i + 1, plan.len(), c.event_name, c.effect.as_ref().map(|e| format!(", {e}")).unwrap_or_default()));
        let p = tmp.join(format!("clip_{i:03}.mp4"));
        total += render_clip(ffmpeg, &sources[c.source], c, bundles, opt, &enc, &p)?;
        list += &format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"));
    }
    if let Some(o) = &opt.outro {
        progress(0.9, "adding outro");
        let p = tmp.join("outro.mp4");
        let (w, h) = output_size(opt);
        let (od, has_a, _, _) = probe_source(ffmpeg, o)?;
        let mut a = vec!["-i".into(), o.display().to_string()];
        if !has_a { a.extend(["-f".into(), "lavfi".into(), "-i".into(), "anullsrc=r=48000:cl=stereo".into(), "-shortest".into()]); }
        a.extend(["-vf".into(), format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,fps={},setsar=1,format=yuv420p", opt.fps)]);
        a.extend(enc.iter().cloned());
        a.extend(["-c:a".into(), "aac".into(), "-ar".into(), "48000".into(), "-ac".into(), "2".into(), p.display().to_string()]);
        ff(ffmpeg, &a)?;
        list += &format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"));
        total += od;
    }
    std::fs::write(tmp.join("list.txt"), list)?;
    let joined = tmp.join("joined.mp4");
    progress(0.92, "joining clips");
    ff(ffmpeg, &["-f".into(), "concat".into(), "-safe".into(), "0".into(), "-i".into(), tmp.join("list.txt").display().to_string(), "-c".into(), "copy".into(), joined.display().to_string()])?;
    if let Some(parent) = out.parent() { std::fs::create_dir_all(parent)?; }
    if let Some(m) = &opt.music {
        progress(0.96, "mixing music");
        let fade_st = (total - 2.0).max(0.0);
        let g = format!(
            "[1:a]atrim=0:{total:.3},asetpts=PTS-STARTPTS,afade=t=out:st={fade_st:.3}:d=2,volume={}[m];[0:a]volume=1.0[g];[g][m]amix=inputs=2:normalize=0:duration=first[a]",
            opt.music_volume
        );
        ff(ffmpeg, &[
            "-i".into(), joined.display().to_string(),
            "-ss".into(), format!("{:.3}", opt.music_start), "-stream_loop".into(), "-1".into(), "-i".into(), m.display().to_string(),
            "-filter_complex".into(), g, "-map".into(), "0:v".into(), "-map".into(), "[a]".into(),
            "-c:v".into(), "copy".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-movflags".into(), "+faststart".into(),
            out.display().to_string(),
        ])?;
    } else {
        ff(ffmpeg, &["-i".into(), joined.display().to_string(), "-c".into(), "copy".into(), "-movflags".into(), "+faststart".into(), out.display().to_string()])?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    progress(1.0, "done");
    Ok(serde_json::json!({ "output": out, "duration": total, "clips": plan, "encoder": encoder }))
}

/// Export a single clip [start, end] (re-encoded for frame accuracy).
pub fn export_clip(ffmpeg: &Path, video: &Path, start: f64, end: f64, out: &Path, encoder: &str, quality: &str, vertical: Option<[f64; 4]>) -> Result<()> {
    if let Some(p) = out.parent() { std::fs::create_dir_all(p)?; }
    let enc = crate::encode::pick(ffmpeg, encoder);
    let info = crate::video::probe(ffmpeg, video)?;
    let mut args: Vec<String> = vec!["-ss".into(), format!("{start:.3}"), "-t".into(), format!("{:.3}", (end - start).max(0.1)), "-i".into(), video.display().to_string()];
    let h = info.height.min(2160);
    if let Some([x, y, w, hh]) = vertical {
        let oh = h / 2 * 2;
        let ow = ((oh as f64 * 9.0 / 16.0) / 2.0).round() as u32 * 2;
        args.extend(["-vf".into(), format!("crop=iw*{w}:ih*{hh}:iw*{x}:ih*{y},scale={ow}:{oh}:force_original_aspect_ratio=increase,crop={ow}:{oh},setsar=1")]);
    }
    args.extend(crate::encode::args(&enc, quality, h));
    args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-movflags".into(), "+faststart".into(), out.display().to_string()]);
    ff(ffmpeg, &args)
}
