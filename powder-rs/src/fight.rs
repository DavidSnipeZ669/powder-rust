//! Fight starts: where a highlight's clip should begin. Walking back from an event (a kill and
//! its medals), the fight is the stretch of shooting before it. Two signals:
//!  - the ammo counter (text on the HUD): a shot when it drops, a reload when it goes up (a reload
//!    keeps the fight going);
//!  - a "fighting" picture model (Model Creation) trained on the game's ordinary matches, where
//!    the ammo counter labels each moment as fighting or calm. It only sees the gameplay (the HUD
//!    is blacked out), so it also works in hardcore, with no ammo counter or hitmarkers.
use crate::discover::Discovery;
use crate::rwdmdl::{Extractor, Model};
use anyhow::{Context, Result};
use image::{Rgb, RgbImage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Clip starts this long before the first shot of the fight.
pub const LEAD: f64 = 1.0;
/// Shortest and longest lead-up before an event.
pub const MIN_BEFORE: f64 = 2.0;
pub const MAX_BEFORE: f64 = 10.0;
/// Two shots further apart than this are separate fights (longer when a reload is between them).
const GAP: f64 = 2.0;
const GAP_RELOAD: f64 = 4.0;
/// Frames read per second around events.
const FPS: f64 = 4.0;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct FightCfg {
    /// where the ammo counter is (l, t, r, b), if the game shows one
    pub ammo: Option<[f32; 4]>,
    /// HUD areas blacked out for the picture model
    pub mask: Vec<[f32; 4]>,
}

pub fn cfg_path(game_dir: &Path) -> PathBuf {
    game_dir.join("fight.json")
}
pub fn load_cfg(game_dir: &Path) -> FightCfg {
    std::fs::read(cfg_path(game_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}
pub fn model_path(game_dir: &Path, id: &str) -> PathBuf {
    game_dir.join(format!("{id}.fight.rwdmdl"))
}

/// The first number of a text box: "030 090" (magazine, then reserve) -> 30; a dim leading zero
/// ("030") is just padding.
fn digits(s: &str) -> Option<u32> {
    let t = s.split_whitespace().next().unwrap_or("");
    if t.is_empty() || t.len() > 3 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// The ammo counter: a number on screen that keeps going down by a little (shots) and sometimes
/// jumps up (reloads), always in the same place.
pub fn find_ammo(scans: &[Discovery]) -> Option<[f32; 4]> {
    // (cell) -> [(scan, time, value, box)]
    let mut cells: HashMap<(i32, i32), Vec<(usize, f64, u32, [f32; 4])>> = HashMap::new();
    for (si, d) in scans.iter().enumerate() {
        for det in &d.detections {
            let Some(v) = digits(&det.text) else { continue };
            let t = d.frame_times.get(det.frame).copied().unwrap_or(det.frame as f64);
            let (cx, cy) = ((det.bbox[0] + det.bbox[2]) / 2.0, (det.bbox[1] + det.bbox[3]) / 2.0);
            cells.entry(((cx / 0.03) as i32, (cy / 0.03) as i32)).or_default().push((si, t, v, det.bbox));
        }
    }
    let mut best: Option<(usize, [f32; 4])> = None;
    for (_, mut v) in cells {
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let (mut down, mut up) = (0usize, 0usize);
        for w in v.windows(2) {
            if w[0].0 != w[1].0 || w[1].1 - w[0].1 > 1.6 {
                continue;
            }
            if w[1].2 < w[0].2 && w[0].2 - w[1].2 <= 10 {
                down += 1;
            } else if w[1].2 > w[0].2 {
                up += 1;
            }
        }
        if down < 8 || up * 2 > down {
            continue;
        }
        if best.as_ref().map(|b| down > b.0).unwrap_or(true) {
            let mut r = [1f32, 1.0, 0.0, 0.0];
            for x in &v {
                r[0] = r[0].min(x.3[0]);
                r[1] = r[1].min(x.3[1]);
                r[2] = r[2].max(x.3[2]);
                r[3] = r[3].max(x.3[3]);
            }
            let (pw, ph) = ((r[2] - r[0]) * 0.3 + 0.004, (r[3] - r[1]) * 0.3 + 0.004);
            best = Some((down, [(r[0] - pw).max(0.0), (r[1] - ph).max(0.0), (r[2] + pw).min(1.0), (r[3] + ph).min(1.0)]));
        }
    }
    best.map(|b| b.1)
}

/// HUD to black out: places where text is on screen in many frames (score, timer, kill feed,
/// ammo...) plus the game's text-reading areas.
pub fn hud_mask(scans: &[Discovery], crops: &[[f32; 4]], ammo: Option<[f32; 4]>) -> Vec<[f32; 4]> {
    const GW: usize = 32;
    const GH: usize = 18;
    let mut heat = vec![0usize; GW * GH];
    let mut frames = 0usize;
    for d in scans {
        frames += d.frame_times.len();
        let mut seen = std::collections::HashSet::new();
        for det in &d.detections {
            let b = det.bbox;
            for gy in (b[1] * GH as f32) as usize..=((b[3] * GH as f32) as usize).min(GH - 1) {
                for gx in (b[0] * GW as f32) as usize..=((b[2] * GW as f32) as usize).min(GW - 1) {
                    if seen.insert((det.frame, gx, gy)) {
                        heat[gy * GW + gx] += 1;
                    }
                }
            }
        }
    }
    let mut out: Vec<[f32; 4]> = Vec::new();
    if frames > 0 {
        for gy in 0..GH {
            for gx in 0..GW {
                if heat[gy * GW + gx] * 10 >= frames {
                    out.push([gx as f32 / GW as f32, gy as f32 / GH as f32, (gx + 1) as f32 / GW as f32, (gy + 1) as f32 / GH as f32]);
                }
            }
        }
    }
    out.extend(crops.iter().copied());
    out.extend(ammo);
    out
}

pub fn apply_mask(img: &mut RgbImage, mask: &[[f32; 4]]) {
    let (w, h) = (img.width(), img.height());
    for r in mask {
        let (x0, y0, cw, ch) = crate::ocr::crop_rect(w, h, r);
        for y in y0..(y0 + ch).min(h) {
            for x in x0..(x0 + cw).min(w) {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
    }
}

/// Screen areas the game reads text from (its crops).
pub fn game_crops(game_dir: &Path) -> Vec<[f32; 4]> {
    let src = std::fs::read_to_string(game_dir.join("game_postprocess.lua")).unwrap_or_default();
    let re = regex::Regex::new(r"cropCoords\s*=\s*\{\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)").unwrap();
    re.captures_iter(&src)
        .filter_map(|c| Some([c[1].parse().ok()?, c[2].parse().ok()?, c[3].parse().ok()?, c[4].parse().ok()?]))
        .filter(|r: &[f32; 4]| !(r[0] <= 0.01 && r[1] <= 0.01 && r[2] >= 0.99 && r[3] >= 0.99)) // not a whole-screen area
        .collect()
}

fn ammo_spec(r: [f32; 4]) -> crate::lua_engine::CropSpec {
    crate::lua_engine::CropSpec {
        name: "Ammo".into(),
        coords: r,
        dilate_diameter: 3,
        min_area: 10,
        margin: 6,
        stretch_vertical: false,
        restricted_chars: String::new(),
        debug: false,
        legacy: false,
    }
}

/// The ammo number on a frame (the tallest number in the counter's area).
pub fn read_ammo(ocr: &mut crate::ocr::Ocr, img: &RgbImage, r: [f32; 4]) -> Option<u32> {
    let res = ocr.run_crop(img, &ammo_spec(r)).ok()?;
    res.boxes
        .iter()
        .filter_map(|b| digits(&b.text).map(|v| (b.y1 - b.y0, v)))
        .max_by_key(|x| x.0)
        .map(|x| x.1)
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub t: f64,
    pub ammo: Option<u32>,
    pub fight: Option<f32>,
}

/// Stretches of fighting: shots and reloads from the ammo counter, and moments the picture model
/// is sure are fighting.
pub fn intervals(samples: &[Sample]) -> Vec<(f64, f64)> {
    // (time, is_reload)
    let mut pts: Vec<(f64, bool)> = Vec::new();
    let mut prev: Option<(f64, u32)> = None;
    for s in samples {
        if let Some(a) = s.ammo {
            if let Some((pt, pa)) = prev {
                if s.t - pt <= 2.0 {
                    if a < pa && pa - a <= 10 {
                        pts.push((s.t, false));
                    } else if a > pa {
                        pts.push((s.t, true));
                    }
                }
            }
            prev = Some((s.t, a));
        }
        if s.fight.map(|p| p >= 0.6).unwrap_or(false) {
            pts.push((s.t, false));
        }
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64, bool)> = Vec::new(); // (start, end, last point was a reload)
    for (t, reload) in pts {
        match out.last_mut() {
            Some(l) if t - l.1 <= if reload || l.2 { GAP_RELOAD } else { GAP } => {
                l.1 = t;
                l.2 = reload;
            }
            _ => out.push((t, t, reload)),
        }
    }
    out.into_iter().map(|x| (x.0, x.1)).collect()
}

/// Where the clip for an event at `t` should start, if a fight is found just before it.
pub fn start_for(t: f64, ivs: &[(f64, f64)], max_before: f64) -> Option<f64> {
    let iv = ivs.iter().filter(|iv| iv.0 <= t + 0.5 && iv.1 >= t - 1.5).min_by(|a, b| a.0.total_cmp(&b.0))?;
    Some((iv.0 - LEAD).max(t - max_before).min(t - MIN_BEFORE).max(0.0))
}

/// Read the frames of `ranges` (seconds) at FPS: the ammo counter on each, the picture model on
/// every second one.
pub fn sample(
    ffmpeg: &Path,
    video: &Path,
    ranges: &[(f64, f64)],
    cfg: &FightCfg,
    mut ocr: Option<&mut crate::ocr::Ocr>,
    mut model: Option<(&Model, &mut Extractor)>,
    mut on_frame: impl FnMut(f64, &RgbImage, Option<u32>, Option<f32>),
) -> Result<Vec<Sample>> {
    let info = crate::video::probe(ffmpeg, video)?;
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let mut r = crate::video::FrameReader::open(
            ffmpeg,
            video,
            &info,
            &crate::video::SampleOptions { fps: FPS, start: Some(a.max(0.0)), end: Some(b), max_height: Some(720) },
        )?;
        let mut n = 0usize;
        while let Some((t, img)) = r.next_frame()? {
            n += 1;
            let ammo = match (cfg.ammo, ocr.as_deref_mut()) {
                (Some(rect), Some(o)) => read_ammo(o, &img, rect),
                _ => None,
            };
            let fight = match model.as_mut() {
                Some((m, ex)) if n % 2 == 0 => {
                    let mut masked = img.clone();
                    apply_mask(&mut masked, &cfg.mask);
                    let p = m.predict(&ex.features(&masked)?);
                    p.get(1).copied()
                }
                _ => None,
            };
            on_frame(t, &img, ammo, fight);
            out.push(Sample { t, ammo, fight });
        }
    }
    Ok(out)
}

/// Merge [t - before, t + 0.5] windows around the events into as few reads as possible.
pub fn windows(times: &[f64], before: f64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = times.iter().map(|&t| ((t - before - 1.0).max(0.0), t + 0.5)).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for w in v {
        match out.last_mut() {
            Some(l) if w.0 <= l.1 + 1.0 => l.1 = l.1.max(w.1),
            _ => out.push(w),
        }
    }
    out
}

/// Model Creation: find the ammo counter and the HUD, then teach the "fighting" picture model
/// from this video (ammo counter = the labels) and every earlier one (kept as features).
pub fn train(
    ffmpeg: &Path,
    ocr: &mut crate::ocr::Ocr,
    work: &Path,
    game_dir: &Path,
    id: &str,
    ds: &Path,
    video: Option<&Path>,
    event_times: &[f64],
) -> Result<Option<String>> {
    let scans = crate::discover::load_all(work);
    let mut cfg = load_cfg(game_dir);
    if cfg.ammo.is_none() {
        cfg.ammo = find_ammo(&scans);
    }
    cfg.mask = hud_mask(&scans, &game_crops(game_dir), cfg.ammo);
    std::fs::write(cfg_path(game_dir), serde_json::to_vec_pretty(&cfg)?)?;
    let mut ex = Extractor::best();
    let cache_path = ds.join(format!(".features-fight-{}.bin", ex.kind));
    let mut cache = crate::rwdmdl::load_cache_pub(&cache_path);
    if let (Some(v), Some(_)) = (video, cfg.ammo) {
        let stem = v.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        cache.retain(|k, _| !k.starts_with(&format!("{stem}|")));
        let info = crate::video::probe(ffmpeg, v)?;
        let dur = info.duration_sec.unwrap_or(0.0);
        eprintln!("[fight] reading the ammo counter through the video");
        // every frame's ammo; a masked picture every second for the model
        let mut pics: Vec<(f64, RgbImage)> = Vec::new();
        let mut next_pic = 0.0;
        let samples = sample(ffmpeg, v, &[(0.0, dur.max(1.0))], &cfg, Some(ocr), None, |t, img, _, _| {
            if t >= next_pic {
                let mut m = img.clone();
                apply_mask(&mut m, &cfg.mask);
                pics.push((t, image::imageops::resize(&m, 640, (640 * m.height() / m.width().max(1)).max(1), image::imageops::FilterType::Triangle)));
                next_pic = t + 1.0;
            }
        })?;
        let read = samples.iter().filter(|s| s.ammo.is_some()).count();
        if read * 3 < samples.len() {
            eprintln!("[fight] the ammo counter was only readable on {read} of {} frames (hardcore, or another HUD) - no fight labels from this video", samples.len());
        } else {
            let ivs = intervals(&samples);
            let near_event = |t: f64| event_times.iter().any(|e| (e - t).abs() < 3.0);
            let dist = |t: f64| ivs.iter().map(|iv| if t < iv.0 { iv.0 - t } else if t > iv.1 { t - iv.1 } else { 0.0 }).fold(f64::MAX, f64::min);
            let fights: Vec<&(f64, RgbImage)> = pics.iter().filter(|p| dist(p.0) <= 0.25).collect();
            let calm: Vec<&(f64, RgbImage)> = pics.iter().filter(|p| dist(p.0) >= 3.0 && !near_event(p.0)).collect();
            // keep the classes roughly balanced (calm moments are the majority)
            let step = (calm.len() / (fights.len() * 2).max(1)).max(1);
            let calm: Vec<_> = calm.into_iter().step_by(step).collect();
            eprintln!("[fight] {} fight(s): {} fighting and {} calm picture(s) from this video", ivs.len(), fights.len(), calm.len());
            for (label, set) in [("fight", fights), ("calm", calm)] {
                for (t, img) in set {
                    cache.insert(format!("{stem}|{t:.1}|{label}"), ex.features(img)?);
                }
            }
        }
    }
    crate::rwdmdl::save_cache_pub(&cache_path, &cache)?;
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (k, v) in &cache {
        xs.push(v.clone());
        ys.push(if k.ends_with("|fight") { 1 } else { 0 });
    }
    let nf = ys.iter().filter(|&&y| y == 1).count();
    if nf < 20 || ys.len() - nf < 20 {
        return Ok(Some(match cfg.ammo {
            None => "fight model: no ammo counter found in this game's videos yet (needed to learn what fighting looks like)".into(),
            Some(_) => format!("fight model: not enough examples yet ({nf} fighting, {} calm; needs 20 of each)", ys.len() - nf),
        }));
    }
    let m = crate::rwdmdl::train_simple(id, &ex.kind, vec!["calm".into(), "fight".into()], xs, ys, 0.6)?;
    m.save(&model_path(game_dir, id))?;
    let acc = m.meta.accuracy.get("fight").copied().unwrap_or(0.0);
    let accc = m.meta.accuracy.get("calm").copied().unwrap_or(0.0);
    Ok(Some(format!("fight model trained on {} picture(s): {:.0}% of fighting and {:.0}% of calm moments right on held-back pictures", m.meta.trained_images, acc * 100.0, accc * 100.0)))
}

/// Fight starts for events (seconds); None where no fight was found.
pub fn starts(
    ffmpeg: &Path,
    ocr: Option<&mut crate::ocr::Ocr>,
    game_dir: &Path,
    id: &str,
    video: &Path,
    times: &[f64],
    max_before: f64,
) -> Result<Vec<Option<f64>>> {
    let cfg = load_cfg(game_dir);
    let model = Model::load(&model_path(game_dir, id)).ok();
    if cfg.ammo.is_none() && model.is_none() {
        return Ok(vec![None; times.len()]);
    }
    let mut ex = match &model {
        Some(m) => Some(Extractor::new(&m.meta.features).context("image network")?),
        None => None,
    };
    let ocr = if cfg.ammo.is_some() { ocr } else { None };
    let samples = sample(ffmpeg, video, &windows(times, max_before), &cfg, ocr, model.as_ref().zip(ex.as_mut()), |_, _, _, _| {})?;
    let ivs = intervals(&samples);
    Ok(times.iter().map(|&t| start_for(t, &ivs, max_before)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(t: f64, a: u32) -> Sample {
        Sample { t, ammo: Some(a), fight: None }
    }
    #[test]
    fn reloads_keep_the_fight() {
        // shots 10..12 s, reload at 14.5 s, shots again 15..16 s, kill at 16.2 s
        let mut v = Vec::new();
        let mut a = 30;
        for i in 0..9 {
            v.push(s(10.0 + i as f64 * 0.25, a));
            a -= 1;
        }
        v.push(s(13.0, a));
        v.push(s(14.5, 30));
        for i in 0..5 {
            v.push(s(15.0 + i as f64 * 0.25, 30 - i));
        }
        let ivs = intervals(&v);
        assert_eq!(ivs.len(), 1, "{ivs:?}");
        let st = start_for(16.2, &ivs, 10.0).unwrap();
        assert!((st - 9.25).abs() < 0.01, "{st}");
        // a pause of 5 s without a reload is a new fight
        let mut w = vec![s(1.0, 30), s(1.25, 29), s(1.5, 28)];
        w.push(s(6.75, 27));
        w.push(s(7.0, 26));
        assert_eq!(intervals(&w).len(), 2);
        // nothing found -> None; quick kill -> at least the minimum lead-up
        assert!(start_for(50.0, &ivs, 10.0).is_none());
        let q = intervals(&[s(20.0, 5), s(20.25, 4)]);
        assert!((start_for(20.3, &q, 10.0).unwrap() - 18.3).abs() < 0.01);
    }
}
