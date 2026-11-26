//! The analysis pipeline: video -> sampled frames -> OCR + visual cues -> Lua -> events.

use crate::game::GamePack;
use crate::lua_engine::{CropSpec, GameScript, LuaResult};
use crate::ocr::Ocr;
use crate::video::{FrameReader, SampleOptions};
use crate::vision::VisionModel;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct AnalyzeOptions {
    pub ffmpeg: PathBuf,
    pub lua_libs: Vec<PathBuf>,
    pub ocr_models: PathBuf,
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub fps: crate::video::FpsSettings,
    /// reuse OCR when a crop's pixels did not change (default on)
    pub skip_unchanged: bool,
    pub max_height: Option<u32>,
    /// write crop debug images for every crop (not only those with debug=true)
    pub debug_dir: Option<PathBuf>,
    pub debug_all: bool,
    pub quiet: bool,
    /// user explicitly asked for native fps (every frame)
    pub fps_native_requested: bool,
    /// parallel analysis workers (default: auto)
    pub workers: Option<usize>,
    /// optional progress sink (frames done / total)
    pub progress: Option<crate::discover::SharedProgress>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputEvent {
    pub name: String,
    pub display_name: String,
    pub time_sec: f64,
    pub timestamp: String,
    pub known: bool,
}

#[derive(Debug, Serialize)]
pub struct Analysis {
    pub engine: String,
    pub game: String,
    pub video: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: usize,
    pub seconds_taken: f64,
    pub events: Vec<OutputEvent>,
    pub warnings: Vec<String>,
    /// per-frame debug data ("TCT" timeline): numeric cue values and OCR strings
    pub frame_times: Vec<f64>,
    pub timeline_values: BTreeMap<String, Vec<f64>>,
    pub timeline_text: BTreeMap<String, Vec<String>>,
    /// raw engine inputs to Lua, so `powder-rs replay` can re-run detection without the video
    pub raw_ocr: BTreeMap<String, Vec<String>>,
    pub raw_legacy_ocr: BTreeMap<String, Vec<String>>,
    pub raw_cues: BTreeMap<String, Vec<f32>>,
}

pub fn fmt_ts(t: f64) -> String {
    let t = t.max(0.0);
    let h = (t / 3600.0) as u64;
    let m = ((t % 3600.0) / 60.0) as u64;
    let s = t % 60.0;
    if h > 0 { format!("{h}:{m:02}:{s:05.2}") } else { format!("{m:02}:{s:05.2}") }
}

pub fn analyze(game: &GamePack, video: &Path, opt: &AnalyzeOptions) -> Result<Analysis> {
    let started = Instant::now();
    let libs: Vec<&Path> = opt.lua_libs.iter().map(|p| p.as_path()).collect();
    let probe_script = GameScript::load(&game.dir, &libs)?;
    let script_fps = probe_script.fps()?;
    drop(probe_script);
    let info = crate::video::probe(&opt.ffmpeg, video)?;
    let mut fps_cfg = opt.fps.clone();
    if fps_cfg.sample.is_none() && opt.fps.sample.is_none() && !opt.fps_native_requested {
        fps_cfg.sample = Some(script_fps);
    }
    let (fps, fps_why) = fps_cfg.effective(&info, script_fps);
    // Time-based scripts (powder-rs template) read POWDER_RS_FPS; reload with the real rate.
    let script = GameScript::load_with_fps(&game.dir, &libs, Some(fps))?;
    let crops: Vec<CropSpec> = script.ocr_crops()?;
    let mut warnings = Vec::new();
    if (fps - script_fps).abs() > 0.01 && !script.is_time_based() {
        warnings.push(format!(
            "this script counts frames assuming {script_fps} fps (get_fps) but analysis runs at {fps:.2} fps; its frame windows/slack will be scaled wrong. Prefer the script's own rate or convert it to time-based (seconds) settings."
        ));
    }

    if VisionModel::spec_path(&game.dir).exists() == false && game.dir.join("model.pwdmdl").exists() {
        warnings.push("encrypted Powder model.pwdmdl ignored; visual cues unavailable (OCR still runs)".into());
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let workers = opt.workers.unwrap_or(if crate::runtime::gpu() { 2 } else { (cores / 2).clamp(1, 6) }).max(1);
    crate::runtime::set_threads((cores / workers).max(1));

    struct Worker {
        ocr: Option<Ocr>,
        vision: Option<VisionModel>,
    }
    let mut pool = Vec::new();
    for _ in 0..workers {
        pool.push(Worker {
            ocr: if crops.is_empty() { None } else { Some(Ocr::load(&opt.ocr_models).context("loading OCR models")?) },
            vision: VisionModel::load(&game.dir)?,
        });
    }
    let cue_names = pool[0].vision.as_ref().map(|v| v.cue_names()).unwrap_or_default();

    let mut reader = FrameReader::open(
        &opt.ffmpeg,
        video,
        &info,
        &SampleOptions { fps, start: opt.start, end: opt.end, max_height: opt.max_height },
    )?;
    let total_est = info.duration_sec.map(|d| {
        let s = opt.start.unwrap_or(0.0);
        let e = opt.end.unwrap_or(d).min(d);
        ((e - s).max(0.0) * fps) as usize
    });
    if !opt.quiet {
        eprintln!(
            "[powder-rs] {} | {} | {}x{} -> {}x{} | {} | {} OCR crop(s) | visual model: {} | {} worker(s){}",
            game.id,
            video.display(),
            info.width,
            info.height,
            reader.width,
            reader.height,
            fps_why,
            crops.len(),
            if cue_names.is_empty() { "none".to_string() } else { format!("{} cues", cue_names.len()) },
            workers,
            if crate::runtime::gpu() { " (DirectML)" } else { " (CPU)" }
        );
    }
    if let Some(d) = &opt.debug_dir {
        std::fs::create_dir_all(d)?;
    }

    /// Per-frame result: OCR text per crop (same order as `crops`) + visual cue values.
    type FrameOut = (usize, f64, Vec<Option<String>>, BTreeMap<String, f32>);
    let (tx_frames, rx_frames) = std::sync::mpsc::sync_channel::<(usize, f64, std::sync::Arc<image::RgbImage>, Vec<bool>)>(workers * 2);
    let mut last_sigs: Vec<Option<Vec<u8>>> = vec![None; crops.len()];
    let mut skipped = 0usize;
    let rx_frames = std::sync::Arc::new(std::sync::Mutex::new(rx_frames));
    let (tx_out, rx_out) = std::sync::mpsc::channel::<Result<FrameOut>>();

    let mut results: Vec<Option<FrameOut>> = Vec::new();
    let mut decoded = 0usize;
    std::thread::scope(|scope| -> Result<()> {
        for mut w in pool.drain(..) {
            let rx = rx_frames.clone();
            let tx = tx_out.clone();
            let crops = &crops;
            let debug_dir = opt.debug_dir.clone();
            let debug_all = opt.debug_all;
            scope.spawn(move || loop {
                let msg = { rx.lock().unwrap().recv() };
                let Ok((idx, t, frame, reuse)) = msg else { break };
                let r = (|| -> Result<FrameOut> {
                    let mut texts = Vec::with_capacity(crops.len());
                    if let Some(o) = w.ocr.as_mut() {
                        for (ci, c) in crops.iter().enumerate() {
                            if reuse[ci] {
                                texts.push(None);
                                continue;
                            }
                            let r = o.run_crop(&frame, c)?;
                            if let Some(d) = &debug_dir {
                                if c.debug || debug_all {
                                    let img = crate::ocr::debug_image(&frame, c, &r);
                                    let _ = img.save(d.join(format!("{}_{:06}_{:.2}s.png", c.name, idx, t)));
                                }
                            }
                            texts.push(Some(r.text));
                        }
                    }
                    let cues = match w.vision.as_mut() { Some(v) => v.run(&frame)?, None => BTreeMap::new() };
                    Ok((idx, t, texts, cues))
                })();
                if tx.send(r).is_err() {
                    break;
                }
            });
        }
        drop(tx_out);
        // producer: decode frames on this thread, collect results as they arrive
        let mut done = 0usize;
        let collect = |r: Result<FrameOut>, results: &mut Vec<Option<FrameOut>>, done: &mut usize| -> Result<()> {
            let r = r?;
            let i = r.0;
            if results.len() <= i { results.resize_with(i + 1, || None); }
            results[i] = Some(r);
            *done += 1;
            Ok(())
        };
        while let Some((t, frame)) = reader.next_frame()? {
            let mut reuse = vec![false; crops.len()];
            if opt.skip_unchanged {
                for (ci, c) in crops.iter().enumerate() {
                    let sig = crop_signature(&frame, &c.coords);
                    if let Some(prev) = &last_sigs[ci] {
                        if sig_diff(prev, &sig) < 1.5 {
                            reuse[ci] = true;
                            skipped += 1;
                        }
                    }
                    if !reuse[ci] { last_sigs[ci] = Some(sig); }
                }
            }
            tx_frames.send((decoded, t, std::sync::Arc::new(frame), reuse)).map_err(|_| anyhow::anyhow!("worker stopped"))?;
            decoded += 1;
            if let Some(p) = &opt.progress {
                let mut p = p.lock().unwrap();
                p.done = decoded;
                p.total = total_est.unwrap_or(0).max(decoded);
                p.state = "analyzing".into();
            }
            while let Ok(r) = rx_out.try_recv() { collect(r, &mut results, &mut done)?; }
            if !opt.quiet && decoded % 20 == 0 {
                let el = started.elapsed().as_secs_f64();
                match total_est {
                    Some(tot) if tot > 0 => {
                        let eta = el / decoded as f64 * (tot.saturating_sub(decoded)) as f64;
                        eprint!("\r  frame {decoded}/{tot} ({:.0}%)  video {}  eta {:.0}s   ", 100.0 * decoded as f64 / tot as f64, fmt_ts(t), eta)
                    }
                    _ => eprint!("\r  frame {decoded}  video {}   ", fmt_ts(t)),
                }
                let _ = std::io::stderr().flush();
            }
        }
        drop(tx_frames);
        while done < decoded {
            match rx_out.recv() {
                Ok(r) => collect(r, &mut results, &mut done)?,
                Err(_) => break,
            }
        }
        Ok(())
    })?;
    let idx = decoded;
    if !opt.quiet {
        let total_crops = (idx * crops.len()).max(1);
        eprintln!(
            "\r  {} frames analysed in {:.1}s ({:.1} frames/s; {:.0}% of crop reads skipped as unchanged)            ",
            idx,
            started.elapsed().as_secs_f64(),
            idx as f64 / started.elapsed().as_secs_f64().max(0.001),
            100.0 * skipped as f64 / total_crops as f64
        );
    }
    if idx == 0 {
        anyhow::bail!("no frames were decoded from {}", video.display());
    }

    let mut frame_times = Vec::with_capacity(idx);
    let mut paddle: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut legacy: BTreeMap<String, (Vec<String>, Vec<f32>)> = BTreeMap::new();
    let mut model_out: BTreeMap<String, Vec<f32>> = cue_names.iter().map(|n| (n.clone(), Vec::with_capacity(idx))).collect();
    for c in &crops {
        if c.legacy { legacy.insert(c.name.clone(), (vec![], vec![])); } else { paddle.insert(c.name.clone(), vec![]); }
    }
    for r in results.into_iter() {
        let (_, t, texts, cues) = r.context("missing frame result")?;
        frame_times.push(t);
        for (ci, (c, text)) in crops.iter().zip(texts).enumerate() {
            // unchanged crop: repeat the previous frame's text
            let text = match text {
                Some(t) => t,
                None => {
                    if c.legacy {
                        legacy[&c.name].0.last().map(|s| s.trim_end_matches('\n').to_string()).unwrap_or_default()
                    } else {
                        paddle[&c.name].last().cloned().unwrap_or_default()
                    }
                }
            };
            let _ = ci;
            if c.legacy {
                let e = legacy.get_mut(&c.name).unwrap();
                e.1.push(if text.is_empty() { 0.0 } else { 0.9 });
                e.0.push(if text.is_empty() { String::new() } else { format!("{text}\n") });
            } else {
                paddle.get_mut(&c.name).unwrap().push(text);
            }
        }
        for (k, v) in cues {
            model_out.entry(k).or_default().push(v);
        }
    }

    let LuaResult { events, tct_values, tct_descriptions } = script.compute_events(&model_out, &legacy, &frame_times, &paddle)?;
    let raw_legacy_ocr = legacy.iter().map(|(k, v)| (k.clone(), v.0.clone())).collect();

    let known = game.event_names();
    let mut out_events = Vec::new();
    for e in events {
        let k = known.is_empty() || known.contains(&e.name);
        if !k {
            let w = format!("event '{}' is produced by the Lua script but not defined in events.json (Powder will ignore it)", e.name);
            if !warnings.contains(&w) { warnings.push(w); }
        }
        out_events.push(OutputEvent { display_name: game.display_name(&e.name), timestamp: fmt_ts(e.time_sec), time_sec: e.time_sec, name: e.name, known: k });
    }

    // timeline text: prefer what Lua reported, fall back to raw OCR
    let mut timeline_text = tct_descriptions;
    for (k, v) in &paddle {
        if !timeline_text.keys().any(|x| x.ends_with(&format!("::{k}"))) {
            timeline_text.insert(format!("ocr::{k}"), v.clone());
        }
    }
    let mut timeline_values = tct_values;
    for (k, v) in &model_out {
        timeline_values.entry(k.clone()).or_insert_with(|| v.iter().map(|x| *x as f64).collect());
    }

    Ok(Analysis {
        engine: format!("powder-rs {}", env!("CARGO_PKG_VERSION")),
        game: game.id.clone(),
        video: video.display().to_string(),
        width: info.width,
        height: info.height,
        fps,
        frames: idx,
        seconds_taken: started.elapsed().as_secs_f64(),
        events: out_events,
        warnings,
        frame_times,
        timeline_values,
        timeline_text,
        raw_ocr: paddle,
        raw_legacy_ocr,
        raw_cues: model_out,
    })
}

/// Re-run only the Lua detection on the raw inputs saved in a results JSON.
pub fn replay(game: &GamePack, results: &serde_json::Value, lua_libs: &[PathBuf]) -> Result<Analysis> {
    let started = Instant::now();
    let libs: Vec<&Path> = lua_libs.iter().map(|p| p.as_path()).collect();
    let script = GameScript::load(&game.dir, &libs)?;
    let frame_times: Vec<f64> = serde_json::from_value(results["frame_times"].clone()).context("results file has no frame_times")?;
    let paddle: BTreeMap<String, Vec<String>> = serde_json::from_value(results["raw_ocr"].clone()).unwrap_or_default();
    let legacy_raw: BTreeMap<String, Vec<String>> = serde_json::from_value(results["raw_legacy_ocr"].clone()).unwrap_or_default();
    let model_out: BTreeMap<String, Vec<f32>> = serde_json::from_value(results["raw_cues"].clone()).unwrap_or_default();
    let legacy = legacy_raw
        .into_iter()
        .map(|(k, v)| {
            let c = v.iter().map(|s| if s.trim().is_empty() { 0.0 } else { 0.9 }).collect();
            (k, (v, c))
        })
        .collect();
    let r = script.compute_events(&model_out, &legacy, &frame_times, &paddle)?;
    let known = game.event_names();
    let events = r
        .events
        .into_iter()
        .map(|e| OutputEvent {
            display_name: game.display_name(&e.name),
            timestamp: fmt_ts(e.time_sec),
            time_sec: e.time_sec,
            known: known.is_empty() || known.contains(&e.name),
            name: e.name,
        })
        .collect();
    Ok(Analysis {
        engine: format!("powder-rs {} (replay)", env!("CARGO_PKG_VERSION")),
        game: game.id.clone(),
        video: results["video"].as_str().unwrap_or("").to_string(),
        width: results["width"].as_u64().unwrap_or(0) as u32,
        height: results["height"].as_u64().unwrap_or(0) as u32,
        fps: results["fps"].as_f64().unwrap_or(0.0),
        frames: frame_times.len(),
        seconds_taken: started.elapsed().as_secs_f64(),
        events,
        warnings: vec![],
        frame_times,
        timeline_values: r.tct_values,
        timeline_text: r.tct_descriptions,
        raw_ocr: paddle,
        raw_legacy_ocr: BTreeMap::new(),
        raw_cues: model_out,
    })
}

/// Tiny grayscale thumbnail of a crop (32x12) used to detect unchanged regions.
fn crop_signature(frame: &image::RgbImage, coords: &[f32; 4]) -> Vec<u8> {
    let (x, y, w, h) = crate::ocr::crop_rect(frame.width(), frame.height(), coords);
    let (gw, gh) = (32u32, 12u32);
    let mut v = Vec::with_capacity((gw * gh) as usize);
    for j in 0..gh {
        for i in 0..gw {
            // average a 2x2 patch at each grid point for a little noise robustness
            let px = x + (i * w) / gw;
            let py = y + (j * h) / gh;
            let mut acc = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = frame.get_pixel((px + dx).min(frame.width() - 1), (py + dy).min(frame.height() - 1));
                acc += (p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10;
            }
            v.push((acc / 4) as u8);
        }
    }
    v
}

fn sig_diff(a: &[u8], b: &[u8]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs()).sum::<u32>() as f32 / a.len().max(1) as f32
}
