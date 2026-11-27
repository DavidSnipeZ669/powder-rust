//! Rewind visual models (.rwdmdl): learn what each event looks like on screen from frames saved
//! into per-event dataset folders, so events are still found when the text reading misses them.
//!
//! Dataset layout:  <datasets>/<ID>/<ID>_<event>/*.jpg   (+ <ID>_nothing for "no event" frames)
//! People can also drop their own screenshots into those folders; every training run uses them all.
//!
//! The model is a small classifier over hand-made image features (brightness + edge directions on a
//! grid), so it trains in seconds on the CPU and needs no extra downloads.
//! The file is encrypted with a key built into Rewind: that stops casual copying and editing, it is
//! not protection against someone determined to reverse-engineer the engine.

use anyhow::{bail, Context, Result};
use image::{imageops::FilterType, RgbImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"RWDMDL\x00\x01";
const KEY: &[u8] = b"Rewind/rwdmdl/v1:7f3c9a21e8d54b06a1c2f9e0b7d3a4c5";
const GW: u32 = 32; // feature grid
const GH: u32 = 18;

#[derive(Serialize, Deserialize, Clone)]
pub struct Meta {
    pub game: String,
    pub classes: Vec<String>, // classes[0] == "nothing"
    pub dims: usize,
    pub threshold: f32,
    pub trained_images: usize,
    pub accuracy: BTreeMapF,
    pub created: String,
}
type BTreeMapF = std::collections::BTreeMap<String, f32>;

pub struct Model {
    pub meta: Meta,
    mean: Vec<f32>,
    std: Vec<f32>,
    w: Vec<f32>, // classes x dims
    b: Vec<f32>,
}

// ---------- features ----------

pub fn features(img: &RgbImage) -> Vec<f32> {
    let small = image::imageops::resize(img, GW * 4, GH * 4, FilterType::Triangle);
    let (w, h) = (GW * 4, GH * 4);
    let gray: Vec<f32> = small.pixels().map(|p| (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) / 255.0).collect();
    let at = |x: u32, y: u32| gray[(y.min(h - 1) * w + x.min(w - 1)) as usize];
    // per cell: mean brightness + 4 edge-direction energies
    let mut f = vec![0f32; (GW * GH * 5) as usize];
    for y in 0..h {
        for x in 0..w {
            let gx = at(x + 1, y) - at(x.saturating_sub(1), y);
            let gy = at(x, y + 1) - at(x, y.saturating_sub(1));
            let mag = (gx * gx + gy * gy).sqrt();
            let ang = gy.atan2(gx).rem_euclid(std::f32::consts::PI);
            let bin = ((ang / std::f32::consts::PI * 4.0) as usize).min(3);
            let c = ((y / 4) * GW + x / 4) as usize * 5;
            f[c] += at(x, y) / 16.0;
            f[c + 1 + bin] += mag / 16.0;
        }
    }
    // coarse colour (HUD colours: red kill markers, gold medals...)
    let col = image::imageops::resize(img, 16, 9, FilterType::Triangle);
    f.extend(col.pixels().flat_map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]));
    f
}

// ---------- file format ----------

fn keystream_xor(nonce: &[u8], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(32).enumerate() {
        let mut h = Sha256::new();
        h.update(KEY);
        h.update(nonce);
        h.update((i as u64).to_le_bytes());
        let k = h.finalize();
        for (a, b) in chunk.iter_mut().zip(k.iter()) {
            *a ^= b;
        }
    }
}
fn tag(nonce: &[u8], ct: &[u8]) -> [u8; 32] {
    let inner: [u8; 32] = Sha256::new().chain_update(KEY).chain_update(nonce).chain_update(ct).finalize().into();
    Sha256::new().chain_update(KEY).chain_update(inner).finalize().into()
}

impl Model {
    pub fn save(&self, path: &Path) -> Result<()> {
        let meta = serde_json::to_vec(&self.meta)?;
        let mut p = Vec::new();
        p.extend((meta.len() as u32).to_le_bytes());
        p.extend(&meta);
        for v in [&self.mean, &self.std, &self.w, &self.b] {
            for x in v.iter() {
                p.extend(x.to_le_bytes());
            }
        }
        let nonce: [u8; 16] = Sha256::digest(format!("{:?}{}", std::time::SystemTime::now(), self.meta.game))[..16].try_into().unwrap();
        keystream_xor(&nonce, &mut p);
        let mut out = MAGIC.to_vec();
        out.extend(nonce);
        out.extend(tag(&nonce, &p));
        out.extend(p);
        let tmp = path.with_extension("rwdmdl.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let b = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if b.len() < 56 || &b[..8] != MAGIC {
            bail!("{} is not a Rewind model (.rwdmdl)", path.display());
        }
        let nonce = &b[8..24];
        let mut p = b[56..].to_vec();
        if tag(nonce, &p)[..] != b[24..56] {
            bail!("{} is damaged or was modified", path.display());
        }
        keystream_xor(nonce, &mut p);
        let ml = u32::from_le_bytes(p[..4].try_into()?) as usize;
        let meta: Meta = serde_json::from_slice(&p[4..4 + ml])?;
        let mut fl = p[4 + ml..].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()));
        let d = meta.dims;
        let k = meta.classes.len();
        let mut take = |n: usize| -> Vec<f32> { (&mut fl).take(n).collect() };
        let (mean, std, w, bias) = (take(d), take(d), take(k * d), take(k));
        if bias.len() != k {
            bail!("{} is incomplete", path.display());
        }
        Ok(Self { meta, mean, std, w, b: bias })
    }

    /// probability per class (classes[0] = nothing)
    pub fn predict(&self, f: &[f32]) -> Vec<f32> {
        let d = self.meta.dims;
        let x: Vec<f32> = f.iter().zip(&self.mean).zip(&self.std).map(|((v, m), s)| (v - m) / s).collect();
        let z: Vec<f32> = (0..self.meta.classes.len()).map(|c| self.b[c] + self.w[c * d..(c + 1) * d].iter().zip(&x).map(|(a, b)| a * b).sum::<f32>()).collect();
        softmax(&z)
    }
}

fn softmax(z: &[f32]) -> Vec<f32> {
    let m = z.iter().cloned().fold(f32::MIN, f32::max);
    let e: Vec<f32> = z.iter().map(|v| (v - m).exp()).collect();
    let s: f32 = e.iter().sum();
    e.iter().map(|v| v / s).collect()
}

/// The model file of a game, if it has one.
pub fn find(game_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(game_dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.extension().map(|x| x == "rwdmdl").unwrap_or(false))
}

// ---------- dataset ----------

pub fn dataset_dir(games_dir: &Path, id: &str) -> PathBuf {
    games_dir.parent().unwrap_or(games_dir).join("datasets").join(id)
}

/// Save labelled frames from a video into the per-event folders. `events` = (event, time_sec).
pub fn extract(ffmpeg: &Path, video: &Path, id: &str, events: &[(String, f64)], ds: &Path) -> Result<(usize, usize)> {
    let info = crate::video::probe(ffmpeg, video)?;
    let mut r = crate::video::FrameReader::open(ffmpeg, video, &info, &crate::video::SampleOptions { fps: 4.0, start: None, end: None, max_height: Some(360) })?;
    let stem: String = video.file_stem().map(|s| s.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()).unwrap_or_default();
    let (mut pos, mut neg, mut n) = (0, 0, 0usize);
    let dur = info.duration_sec.unwrap_or(0.0);
    while let Some((t, img)) = r.next_frame()? {
        n += 1;
        if n % 200 == 0 && dur > 0.0 {
            eprintln!("[model] saving training frames {:.0}%", (t / dur * 100.0).min(100.0));
        }
        // the frame belongs to an event if the event's text was on screen around then
        let near = events.iter().filter(|(_, et)| t >= et - 0.5 && t <= et + 1.5).min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs()));
        let label = match near {
            Some((e, _)) => e.clone(),
            None if events.iter().all(|(_, et)| (et - t).abs() > 4.0) && n % 6 == 0 => "nothing".to_string(),
            None => continue,
        };
        let dir = ds.join(format!("{id}_{label}"));
        std::fs::create_dir_all(&dir)?;
        let f = dir.join(format!("{stem}_{:07}.jpg", (t * 100.0) as u64));
        if !f.exists() {
            image::DynamicImage::ImageRgb8(img).save_with_format(&f, image::ImageFormat::Jpeg)?;
        }
        if label == "nothing" { neg += 1 } else { pos += 1 }
    }
    Ok((pos, neg))
}

// ---------- training ----------

pub fn train(id: &str, ds: &Path) -> Result<Model> {
    let prefix = format!("{id}_");
    let mut classes = vec!["nothing".to_string()];
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(ds).with_context(|| format!("no dataset at {}", ds.display()))?.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if !p.is_dir() {
            continue;
        }
        let ev = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        if ev != "nothing" && !classes.contains(&ev) {
            classes.push(ev.clone());
        }
        dirs.push((ev, p));
    }
    let mut xs: Vec<Vec<f32>> = Vec::new();
    let mut ys: Vec<usize> = Vec::new();
    let mut files: Vec<(usize, PathBuf)> = Vec::new();
    for (ev, d) in &dirs {
        let c = classes.iter().position(|x| x == ev).unwrap();
        for f in std::fs::read_dir(d)?.flatten() {
            let p = f.path();
            if matches!(p.extension().and_then(|x| x.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("jpg" | "jpeg" | "png" | "webp")) {
                files.push((c, p));
            }
        }
    }
    // drop events with too few pictures to learn from
    let count = |c: usize, f: &[(usize, PathBuf)]| f.iter().filter(|x| x.0 == c).count();
    let keep: Vec<usize> = (0..classes.len()).filter(|&c| c == 0 || count(c, &files) >= 8).collect();
    if count(0, &files) < 8 || keep.len() < 2 {
        bail!("not enough training pictures yet (need at least 8 for \"nothing\" and for one event) - add another video");
    }
    let classes: Vec<String> = keep.iter().map(|&c| classes[c].clone()).collect();
    let total = files.len();
    for (i, (c, p)) in files.iter().enumerate() {
        let Some(nc) = keep.iter().position(|k| k == c) else { continue };
        if i % 250 == 0 {
            eprintln!("[model] reading pictures {}/{}", i, total);
        }
        match image::open(p) {
            Ok(img) => {
                xs.push(features(&img.to_rgb8()));
                ys.push(nc);
            }
            Err(e) => eprintln!("[model] skipped {}: {e}", p.display()),
        }
    }
    let d = xs[0].len();
    let k = classes.len();
    let n = xs.len();
    let mut mean = vec![0f32; d];
    let mut std = vec![0f32; d];
    for x in &xs {
        for j in 0..d { mean[j] += x[j] / n as f32 }
    }
    for x in &xs {
        for j in 0..d { std[j] += (x[j] - mean[j]).powi(2) / n as f32 }
    }
    for s in std.iter_mut() { *s = s.sqrt().max(1e-3) }
    for x in xs.iter_mut() {
        for j in 0..d { x[j] = (x[j] - mean[j]) / std[j] }
    }
    // every 5th picture is held back to check how well the model does
    let (train_i, test_i): (Vec<usize>, Vec<usize>) = (0..n).partition(|i| i % 5 != 0);
    let mut cnt = vec![0usize; k];
    for &i in &train_i { cnt[ys[i]] += 1 }
    let cw: Vec<f32> = cnt.iter().map(|&c| if c == 0 { 0.0 } else { train_i.len() as f32 / (k as f32 * c as f32) }).collect();
    let mut w = vec![0f32; k * d];
    let mut b = vec![0f32; k];
    let epochs = 40;
    let mut order = train_i.clone();
    let mut seed = 0x9e3779b97f4a7c15u64;
    for ep in 0..epochs {
        let lr = 0.02 / (1.0 + ep as f32 * 0.15);
        for i in (1..order.len()).rev() {
            seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
            order.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        for &i in &order {
            let x = &xs[i];
            let z: Vec<f32> = (0..k).map(|c| b[c] + w[c * d..(c + 1) * d].iter().zip(x).map(|(a, v)| a * v).sum::<f32>()).collect();
            let p = softmax(&z);
            for c in 0..k {
                let g = (p[c] - if ys[i] == c { 1.0 } else { 0.0 }) * cw[ys[i]] * lr;
                let row = &mut w[c * d..(c + 1) * d];
                for j in 0..d { row[j] -= g * x[j] + lr * 1e-4 * row[j] }
                b[c] -= g;
            }
        }
        if ep % 10 == 9 {
            eprintln!("[model] training {}%", (ep + 1) * 100 / epochs);
        }
    }
    let mut m = Model {
        meta: Meta { game: id.into(), classes: classes.clone(), dims: d, threshold: 0.8, trained_images: n, accuracy: Default::default(), created: String::new() },
        mean: vec![0.0; d], std: vec![1.0; d], w, b,
    };
    // accuracy on the held-back pictures (features already normalised here)
    let mut right = vec![(0usize, 0usize); k];
    for &i in &test_i {
        let p = m.predict(&xs[i]);
        let best = p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|x| x.0).unwrap();
        right[ys[i]].1 += 1;
        if best == ys[i] { right[ys[i]].0 += 1 }
    }
    for (c, (r, t)) in right.iter().enumerate() {
        if *t > 0 {
            m.meta.accuracy.insert(classes[c].clone(), *r as f32 / *t as f32);
        }
    }
    m.mean = mean;
    m.std = std;
    m.meta.created = format!("{:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    Ok(m)
}

// ---------- detection ----------

/// Turn per-frame probabilities into events: an event fires when it is the most likely class above
/// the threshold on 2 frames in a row, then stays quiet for 3 s.
pub struct Detector {
    pub model: Model,
    streak: Vec<u32>,
    last: Vec<f64>,
}
impl Detector {
    pub fn new(model: Model) -> Self {
        let k = model.meta.classes.len();
        Self { model, streak: vec![0; k], last: vec![f64::MIN; k] }
    }
    pub fn push(&mut self, t: f64, img: &RgbImage) -> Option<String> {
        let p = self.model.predict(&features(img));
        let (best, pb) = p.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, v)| (i, *v)).unwrap();
        let mut out = None;
        for c in 1..p.len() {
            if c == best && pb >= self.model.meta.threshold {
                self.streak[c] += 1;
                if self.streak[c] == 2 && t - self.last[c] > 3.0 {
                    self.last[c] = t;
                    out = Some(self.model.meta.classes[c].clone());
                }
            } else {
                self.streak[c] = 0;
            }
        }
        out
    }
}

/// Run a model over a whole video (no text reading), returns (event, time_sec).
pub fn detect(ffmpeg: &Path, video: &Path, model: Model, start: Option<f64>, end: Option<f64>) -> Result<Vec<(String, f64)>> {
    let info = crate::video::probe(ffmpeg, video)?;
    let mut r = crate::video::FrameReader::open(ffmpeg, video, &info, &crate::video::SampleOptions { fps: 4.0, start, end, max_height: Some(360) })?;
    let mut det = Detector::new(model);
    let mut out = Vec::new();
    let dur = info.duration_sec.unwrap_or(0.0);
    let mut n = 0;
    while let Some((t, img)) = r.next_frame()? {
        n += 1;
        if n % 400 == 0 && dur > 0.0 {
            eprintln!("[model] looking for events {:.0}%", (t / dur * 100.0).min(100.0));
        }
        if let Some(e) = det.push(t, &img) {
            out.push((e, (t - 0.25).max(0.0)));
        }
    }
    Ok(out)
}
