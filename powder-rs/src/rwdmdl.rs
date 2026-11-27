//! Rewind visual models (.rwdmdl): learn what each event looks like on screen from frames saved
//! into per-event dataset folders, so events are still found when the text reading misses them.
//!
//! Dataset layout:  <datasets>/<ID>/<ID>_<event>/*.jpg   (+ <ID>_nothing for "no event" frames)
//! People can also drop their own screenshots into those folders; every training run uses them all.
//!
//! Pictures go through a pre-trained image network (DINOv2-small, downloaded once into
//! engine\\models\\vision) that already understands shapes and objects; only a small classifier on
//! top of it is trained, so training is quick and needs few videos. Without the network (offline)
//! a simpler built-in feature set ("grid") is used instead.
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
    #[serde(default = "grid_kind")]
    pub features: String,
    pub classes: Vec<String>, // classes[0] == "nothing"
    pub dims: usize,
    pub threshold: f32,
    pub trained_images: usize,
    pub accuracy: BTreeMapF,
    pub created: String,
}
fn grid_kind() -> String {
    GRID.into()
}
pub const GRID: &str = "grid-v1";
pub const DINO: &str = "dinov2s-v1";
const DINO_URL: &str = "https://huggingface.co/Xenova/dinov2-small/resolve/main/onnx/model.onnx";

pub fn backbone_path() -> PathBuf {
    if let Ok(p) = std::env::var("POWDER_RS_VISION") {
        return PathBuf::from(p);
    }
    crate::paths::exe_dir().join("models").join("vision").join("dinov2-small.onnx")
}

/// Download the network if this install doesn't have it yet (setup normally does this).
pub fn ensure_backbone() -> Result<PathBuf> {
    let p = backbone_path();
    if p.exists() {
        return Ok(p);
    }
    eprintln!("[model] downloading the image network (DINOv2-small, 88 MB, once)...");
    std::fs::create_dir_all(p.parent().unwrap())?;
    let r = ureq::get(DINO_URL).call().map_err(|e| anyhow::anyhow!("downloading the image network: {e}"))?;
    let tmp = p.with_extension("part");
    let mut f = std::fs::File::create(&tmp)?;
    std::io::copy(&mut r.into_reader(), &mut f)?;
    drop(f);
    if std::fs::metadata(&tmp)?.len() < 50 << 20 {
        let _ = std::fs::remove_file(&tmp);
        bail!("the image network download was incomplete");
    }
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Turns a picture into numbers the classifier learns from.
pub struct Extractor {
    pub kind: String,
    session: Option<ort::session::Session>,
}
impl Extractor {
    pub fn new(kind: &str) -> Result<Self> {
        let session = if kind == DINO { Some(crate::runtime::session(&ensure_backbone()?)?) } else { None };
        Ok(Self { kind: kind.into(), session })
    }
    /// the network if it can be had, otherwise the built-in features
    pub fn best() -> Self {
        match Self::new(DINO) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[model] image network unavailable ({e:#}); using the simpler built-in features");
                Self { kind: GRID.into(), session: None }
            }
        }
    }
    pub fn features(&mut self, img: &RgbImage) -> Result<Vec<f32>> {
        let Some(s) = self.session.as_mut() else { return Ok(features(img)) };
        // the network takes 224x224: the whole frame plus its 4 quarters (keeps small HUD details)
        let (w, h) = (img.width(), img.height());
        let views = [(0, 0, w, h), (0, 0, w / 2, h / 2), (w / 2, 0, w - w / 2, h / 2), (0, h / 2, w / 2, h - h / 2), (w / 2, h / 2, w - w / 2, h - h / 2)];
        let mut a = ndarray::Array4::<f32>::zeros((views.len(), 3, 224, 224));
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        for (v, &(x, y, cw, ch)) in views.iter().enumerate() {
            let c = image::imageops::crop_imm(img, x, y, cw.max(1), ch.max(1)).to_image();
            let r = image::imageops::resize(&c, 224, 224, FilterType::Triangle);
            for (px, py, p) in r.enumerate_pixels() {
                for k in 0..3 {
                    a[[v, k, py as usize, px as usize]] = (p[k] as f32 / 255.0 - MEAN[k]) / STD[k];
                }
            }
        }
        let out = s.run(vec![("pixel_values".to_string(), ort::session::SessionInputValue::from(ort::value::TensorRef::from_array_view(&a)?))]).map_err(|e| anyhow::anyhow!("image network: {e}"))?;
        let (shape, d) = out["last_hidden_state"].try_extract_tensor::<f32>().map_err(|e| anyhow::anyhow!("{e}"))?;
        let (n, t, c) = (shape[0] as usize, shape[1] as usize, shape[2] as usize);
        let mut f = Vec::with_capacity(n * c * 2);
        for v in 0..n {
            let base = v * t * c;
            f.extend_from_slice(&d[base..base + c]); // the summary token
            for j in 0..c {
                f.push((1..t).map(|i| d[base + i * c + j]).sum::<f32>() / (t - 1) as f32); // average of the patches
            }
        }
        Ok(f)
    }
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

// ---------- training ----------

fn bf16(v: f32) -> [u8; 2] {
    ((v.to_bits() >> 16) as u16).to_le_bytes()
}
fn unbf16(b: [u8; 2]) -> f32 {
    f32::from_bits((u16::from_le_bytes(b) as u32) << 16)
}
/// features of every picture already seen, so retraining doesn't run the network on them again
fn load_cache(p: &Path) -> std::collections::HashMap<String, Vec<f32>> {
    let mut m = std::collections::HashMap::new();
    let Ok(b) = std::fs::read(p) else { return m };
    let mut i = 0;
    while i + 6 <= b.len() {
        let nl = u16::from_le_bytes([b[i], b[i + 1]]) as usize;
        let nd = u32::from_le_bytes(b[i + 2..i + 6].try_into().unwrap()) as usize;
        i += 6;
        if i + nl + nd * 2 > b.len() {
            break;
        }
        let k = String::from_utf8_lossy(&b[i..i + nl]).to_string();
        i += nl;
        let v = (0..nd).map(|j| unbf16([b[i + j * 2], b[i + j * 2 + 1]])).collect();
        i += nd * 2;
        m.insert(k, v);
    }
    m
}
fn save_cache(p: &Path, m: &std::collections::HashMap<String, Vec<f32>>) -> Result<()> {
    let mut b = Vec::new();
    for (k, v) in m {
        b.extend((k.len() as u16).to_le_bytes());
        b.extend((v.len() as u32).to_le_bytes());
        b.extend(k.as_bytes());
        for x in v {
            b.extend(bf16(*x));
        }
    }
    std::fs::write(p, b)?;
    Ok(())
}

/// The look-based checks (3 = look-alikes, 5 = confident learning) in the given order, then (if
/// `fit`) the training itself. With `fit` false only the checks run (used to run check 5 early).
pub fn train_opts(id: &str, ds: &Path, checks: &[u8], fit_model: bool) -> Result<Option<Model>> {
    let mut ex = Extractor::best();
    let cache_path = ds.join(format!(".features-{}.bin", ex.kind));
    let mut cache = load_cache(&cache_path);
    let mut seen = std::collections::HashSet::new();
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
        if ev == "review" {
            continue; // pictures the second check couldn't confirm
        }
        if ev != "nothing" && !classes.contains(&ev) {
            classes.push(ev.clone());
        }
        dirs.push((ev, p));
    }
    let mut xs: Vec<Vec<f32>> = Vec::new();
    let mut ys: Vec<usize> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
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
        if i % 100 == 0 {
            eprintln!("[model] reading pictures {}/{}", i, total);
        }
        let key = p.strip_prefix(ds).unwrap_or(p).to_string_lossy().replace('\\', "/");
        seen.insert(key.clone());
        if let Some(v) = cache.get(&key) {
            xs.push(v.clone());
            ys.push(nc);
            paths.push(p.clone());
            continue;
        }
        match image::open(p).map_err(anyhow::Error::from).and_then(|img| ex.features(&img.to_rgb8())) {
            Ok(v) => {
                cache.insert(key, v.clone());
                xs.push(v);
                ys.push(nc);
                paths.push(p.clone());
            }
            Err(e) => eprintln!("[model] skipped {}: {e:#}", p.display()),
        }
    }
    cache.retain(|k, _| seen.contains(k)); // pictures people deleted
    let _ = save_cache(&cache_path, &cache);
    if xs.is_empty() {
        bail!("no readable pictures");
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
    let locked = trusted(ds);
    for &check in checks {
        let n = xs.len();
        // look-based checks need plenty of pictures to judge by: on small sets they mostly guess
        // (confident learning was tested on ~2,500 pictures; it moved a third of a hand-checked
        // 198-picture set), so they wait until the game has enough
        if n < if check == 5 { 600 } else { 200 } {
            continue;
        }
        // 3: pictures whose closest look-alikes are almost all filed under one other event
        // 5: confident learning - a cautious model that never saw the picture judges it
        let flagged: Vec<usize> = (if check == 3 { knn_flags(&xs, &ys, k) } else { cl_flags(&xs, &ys, k, d) })
            .into_iter()
            .filter(|&i| !paths[i].file_name().map(|f| locked.contains(&*f.to_string_lossy())).unwrap_or(false))
            .collect();
        if flagged.is_empty() {
            continue;
        }
        let review = ds.join(format!("{id}_review"));
        std::fs::create_dir_all(&review)?;
        for &i in &flagged {
            if let Some(f) = paths[i].file_name() {
                let _ = std::fs::rename(&paths[i], review.join(f));
            }
        }
        let what = if check == 3 { "third check (look-alikes)" } else { "fifth check (confident learning)" };
        eprintln!("[model] {what}: {} picture(s) moved to {id}_review", flagged.len());
        println!("{what}: {} picture(s) moved to {id}_review", flagged.len());
        let keep: Vec<bool> = (0..n).map(|i| !flagged.contains(&i)).collect();
        let mut it = keep.iter();
        xs.retain(|_| *it.next().unwrap());
        let mut it = keep.iter();
        ys.retain(|_| *it.next().unwrap());
        let mut it = keep.iter();
        paths.retain(|_| *it.next().unwrap());
    }
    if !fit_model {
        return Ok(None);
    }
    let n = xs.len();
    // every 5th picture is held back to check how well the model does
    let (train_i, test_i): (Vec<usize>, Vec<usize>) = (0..n).partition(|i| i % 5 != 0);
    let (w, b) = fit(&xs, &ys, &train_i, k, d, 40, true);
    let mut m = Model {
        meta: Meta { game: id.into(), features: ex.kind.clone(), classes: classes.clone(), dims: d, threshold: 0.8, trained_images: n, accuracy: Default::default(), created: String::new() },
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
    Ok(Some(m))
}

/// Third check: pictures whose 7 closest look-alikes are mostly (5+) filed under one other event
/// and at most one under their own. Events with fewer than 15 pictures are left alone.
pub fn knn_flags(xs: &[Vec<f32>], ys: &[usize], k: usize) -> Vec<usize> {
    let n = xs.len();
    let mut count = vec![0usize; k];
    for &y in ys { count[y] += 1 }
    let norms: Vec<f32> = xs.iter().map(|x| x.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6)).collect();
    let mut flagged = Vec::new();
    for i in 0..n {
        if count[ys[i]] < 15 {
            continue;
        }
        let mut sims: Vec<(f32, usize)> = (0..n).filter(|&j| j != i).map(|j| (xs[i].iter().zip(&xs[j]).map(|(a, b)| a * b).sum::<f32>() / (norms[i] * norms[j]), j)).collect();
        let nth = 7.min(sims.len().saturating_sub(1));
        sims.select_nth_unstable_by(nth, |a, b| b.0.total_cmp(&a.0));
        let mut votes = vec![0usize; k];
        let mut top: Vec<(f32, usize)> = sims.into_iter().take(8).collect();
        top.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, j) in top.iter().take(7) {
            votes[ys[*j]] += 1;
        }
        let (best, nb) = votes.iter().enumerate().max_by_key(|x| *x.1).map(|(c, v)| (c, *v)).unwrap();
        if best != ys[i] && nb >= 5 && votes[ys[i]] <= 1 {
            flagged.push(i);
        }
    }
    flagged
}

/// Fifth check (confident learning) on normalised features.
pub fn cl_flags(xs: &[Vec<f32>], ys: &[usize], k: usize, d: usize) -> Vec<usize> {
    let n = xs.len();
    let mut probs = vec![vec![0f32; k]; n];
    for fold in 0..5 {
        let tr: Vec<usize> = (0..n).filter(|i| (i * 7 + 3) % 5 != fold).collect();
        let (w, b) = fit_l2(xs, ys, &tr, k, d, 15, false, 5e-2);
        for i in (0..n).filter(|i| (i * 7 + 3) % 5 == fold) {
            let z: Vec<f32> = (0..k).map(|c| b[c] + w[c * d..(c + 1) * d].iter().zip(&xs[i]).map(|(a, v)| a * v).sum::<f32>()).collect();
            probs[i] = softmax(&z);
        }
    }
    let mut bar = vec![1f32; k];
    let mut count = vec![0usize; k];
    for c in 0..k {
        let own: Vec<f32> = (0..n).filter(|&i| ys[i] == c).map(|i| probs[i][c]).collect();
        count[c] = own.len();
        if !own.is_empty() {
            bar[c] = own.iter().sum::<f32>() / own.len() as f32;
        }
    }
    let mut flagged = Vec::new();
    for i in 0..n {
        let y = ys[i];
        if count[y] < 15 {
            continue;
        }
        let other = (0..k).filter(|&c| c != y && count[c] >= 5 && probs[i][c] >= bar[c]).max_by(|a, b| probs[i][*a].total_cmp(&probs[i][*b]));
        if let Some(c) = other {
            if probs[i][y] < bar[y] && probs[i][c] > probs[i][y] {
                flagged.push(i);
            }
        }
    }
    flagged
}

/// Fourth check on (class, x, y) text positions: indices far (>0.15) from their class's usual
/// spot, when those are under a quarter of a class with at least 10 positions.
/// Same, plus the detector areas per class (`areas[class]`, empty = unknown): text outside every
/// area of its event's detectors (e.g. a kill-feed "KILL" in the corner while the kill detector
/// reads the middle of the screen) is always flagged.
pub fn location_flags_in(items: &[(usize, f32, f32)], areas: &[Vec<[f32; 4]>]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(a) = areas.get(it.0).filter(|a| !a.is_empty()) {
            let m = 0.02;
            if !a.iter().any(|r| it.1 >= r[0] - m && it.1 <= r[2] + m && it.2 >= r[1] - m && it.2 <= r[3] + m) {
                out.push(i);
            }
        }
    }
    let classes: std::collections::BTreeSet<usize> = items.iter().map(|x| x.0).collect();
    for c in classes {
        let idx: Vec<usize> = (0..items.len()).filter(|&i| items[i].0 == c).collect();
        if idx.len() < 10 {
            continue;
        }
        let med = |mut v: Vec<f32>| { v.sort_by(|a, b| a.total_cmp(b)); v[v.len() / 2] };
        let mx = med(idx.iter().map(|&i| items[i].1).collect());
        let my = med(idx.iter().map(|&i| items[i].2).collect());
        let far: Vec<usize> = idx.iter().copied().filter(|&i| ((items[i].1 - mx).powi(2) + (items[i].2 - my).powi(2)).sqrt() > 0.15 && !out.contains(&i)).collect();
        if !far.is_empty() && far.len() * 4 <= idx.len() {
            out.extend(far);
        }
    }
    out
}

/// Softmax classifier by SGD, class-balanced (rare events count as much as common ones).
fn fit(xs: &[Vec<f32>], ys: &[usize], idx: &[usize], k: usize, d: usize, epochs: usize, progress: bool) -> (Vec<f32>, Vec<f32>) {
    fit_l2(xs, ys, idx, k, d, epochs, progress, 1e-4)
}

fn fit_l2(xs: &[Vec<f32>], ys: &[usize], idx: &[usize], k: usize, d: usize, epochs: usize, progress: bool, l2: f32) -> (Vec<f32>, Vec<f32>) {
    let mut cnt = vec![0usize; k];
    for &i in idx { cnt[ys[i]] += 1 }
    let cw: Vec<f32> = cnt.iter().map(|&c| if c == 0 { 0.0 } else { idx.len() as f32 / (k as f32 * c as f32) }).collect();
    let mut w = vec![0f32; k * d];
    let mut b = vec![0f32; k];
    let mut order = idx.to_vec();
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
                for j in 0..d { row[j] -= g * x[j] + lr * l2 * row[j] }
                b[c] -= g;
            }
        }
        if progress && ep % 10 == 9 {
            eprintln!("[model] training {}%", (ep + 1) * 100 / epochs);
        }
    }
    (w, b)
}

// ---------- detection ----------

/// Turn per-frame probabilities into events: an event fires when it is the most likely class above
/// the threshold on 2 frames in a row, then stays quiet for 3 s.
pub struct Detector {
    pub model: Model,
    ex: Extractor,
    streak: Vec<u32>,
    last: Vec<f64>,
}
impl Detector {
    pub fn new(model: Model) -> Result<Self> {
        let k = model.meta.classes.len();
        let ex = Extractor::new(&model.meta.features)?;
        Ok(Self { model, ex, streak: vec![0; k], last: vec![f64::MIN; k] })
    }
    pub fn push(&mut self, t: f64, img: &RgbImage) -> Result<Option<String>> {
        let p = self.model.predict(&self.ex.features(img)?);
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
        Ok(out)
    }
}

/// Run a model over a whole video (no text reading), returns (event, time_sec).
pub fn detect(ffmpeg: &Path, video: &Path, model: Model, start: Option<f64>, end: Option<f64>) -> Result<Vec<(String, f64)>> {
    let info = crate::video::probe(ffmpeg, video)?;
    let mut r = crate::video::FrameReader::open(ffmpeg, video, &info, &crate::video::SampleOptions { fps: 2.0, start, end, max_height: Some(360) })?;
    let mut det = Detector::new(model)?;
    let mut out = Vec::new();
    let dur = info.duration_sec.unwrap_or(0.0);
    let mut n = 0;
    while let Some((t, img)) = r.next_frame()? {
        n += 1;
        if n % 400 == 0 && dur > 0.0 {
            eprintln!("[model] looking for events {:.0}%", (t / dur * 100.0).min(100.0));
        }
        if let Some(e) = det.push(t, &img)? {
            out.push((e, (t - 0.25).max(0.0)));
        }
    }
    Ok(out)
}

/// Training pictures from the game's text scan of a video: a frame goes into an event's folder
/// only when a line of text on it reads exactly that event's word or phrase (a single "KILL", not
/// "DOUBLE KILL", "KILL CONFIRMED" or "KILLS"). "nothing" frames have none of the events' words
/// at all. Re-running on the same video replaces its earlier automatic pictures; pictures added
/// by hand (other file names) are kept.
pub fn extract_from_scan(work: &Path, game_dir: &Path, video: &Path, id: &str, ds: &Path) -> Result<(usize, usize)> {
    use crate::discover::tokens;
    let scans = crate::discover::load_all(work);
    let name = |p: &str| p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase();
    let want = name(&video.to_string_lossy());
    let Some(d) = scans.iter().find(|d| name(&d.video) == want) else {
        bail!("this video hasn't been scanned for the game yet - add it with Add Game first");
    };
    let lua = std::fs::read_to_string(game_dir.join("game_postprocess.lua"))?;
    let re = regex::Regex::new(r#"event\s*=\s*"([^"]+)"\s*,\s*crop\s*=\s*"[^"]*"\s*,\s*match\s*=\s*\{([^}]*)\}"#).unwrap();
    let mut dets: Vec<(String, Vec<String>)> = Vec::new();
    for c in re.captures_iter(&lua) {
        for w in c[2].split('"').skip(1).step_by(2) {
            let t = tokens(w);
            if !t.is_empty() && !dets.iter().any(|x| x.0 == c[1] && x.1 == t) {
                dets.push((c[1].to_string(), t));
            }
        }
    }
    if dets.is_empty() {
        bail!("no events found in the game's settings");
    }
    let all_words: std::collections::HashSet<String> = dets.iter().flat_map(|x| x.1.iter().cloned()).collect();
    let n = d.frame_times.len();
    // OCR sometimes splits one line into boxes ("KILL" + "CONFIRMED"): join boxes on the same line
    let (fw, fh) = (d.width.max(1) as f32, d.height.max(1) as f32);
    let mut boxes: Vec<Vec<([f32; 4], &str)>> = vec![Vec::new(); n];
    for det in &d.detections {
        if det.frame < n {
            boxes[det.frame].push((det.bbox, det.text.as_str()));
        }
    }
    let mut lines: Vec<Vec<Vec<String>>> = vec![Vec::new(); n];
    for f in 0..n {
        let mut bs = boxes[f].clone();
        bs.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
        let mut merged: Vec<([f32; 4], String)> = Vec::new();
        for (b, t) in bs {
            let h = (b[3] - b[1]) * fh;
            let cy = (b[1] + b[3]) / 2.0 * fh;
            match merged.iter_mut().find(|(m, _)| {
                let mh = (m[3] - m[1]) * fh;
                let my = (m[1] + m[3]) / 2.0 * fh;
                (my - cy).abs() < 0.5 * h.max(mh) && (b[0] - m[2]) * fw < 1.5 * h.max(mh) && b[0] >= m[0]
            }) {
                Some((m, mt)) => {
                    m[2] = m[2].max(b[2]);
                    m[1] = m[1].min(b[1]);
                    m[3] = m[3].max(b[3]);
                    mt.push(' ');
                    mt.push_str(t);
                }
                None => merged.push((b, t.to_string())),
            }
        }
        lines[f] = merged.iter().map(|(_, t)| tokens(t)).filter(|t| !t.is_empty()).collect();
    }
    // text that stays on screen (a mode name like KILL CONFIRMED) doesn't make a frame "busy"
    let mut freq: std::collections::HashMap<Vec<String>, usize> = std::collections::HashMap::new();
    for ls in &lines {
        let mut u: Vec<&Vec<String>> = ls.iter().collect();
        u.dedup();
        for l in u {
            *freq.entry(l.clone()).or_default() += 1;
        }
    }
    let mut label: Vec<Option<String>> = vec![None; n];
    let mut has_word = vec![false; n];
    for f in 0..n {
        let mut best: Option<&(String, Vec<String>)> = None;
        for l in &lines[f] {
            if l.iter().any(|w| all_words.contains(w)) && freq.get(l).copied().unwrap_or(0) * 10 < n {
                has_word[f] = true;
            }
            for dt in &dets {
                if l == &dt.1 && best.map(|b| dt.1.len() > b.1.len()).unwrap_or(true) {
                    best = Some(dt);
                }
            }
        }
        label[f] = best.map(|b| b.0.clone());
    }
    let stem: String = video.file_stem().map(|s| s.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()).unwrap_or_default();
    let locked_now = trusted(ds);
    // replace this video's earlier automatic pictures (not ones locked as sorted by hand)
    if let Ok(rd) = std::fs::read_dir(ds) {
        for e in rd.flatten().filter(|e| e.path().is_dir()) {
            for f in std::fs::read_dir(e.path())?.flatten() {
                let fname = f.file_name().to_string_lossy().to_string();
                if fname.starts_with(&format!("{stem}_")) && !locked_now.contains(&fname) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        }
    }
    let frames = Path::new(&d.frames_dir);
    let (mut pos, mut neg, mut quiet) = (0, 0, 0usize);
    // what the scan's text said for each picture, so check 1 can be applied again later
    let scan_p = ds.join(".scan.json");
    let mut scan_labels: std::collections::HashMap<String, String> = std::fs::read(&scan_p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    scan_labels.retain(|k, _| !k.starts_with(&format!("{stem}_")));
    for f in 0..n {
        let near_event = (f.saturating_sub(3)..(f + 4).min(n)).any(|k| has_word[k]);
        let lab = match &label[f] {
            Some(l) => l.clone(),
            None if !near_event => {
                quiet += 1;
                if quiet % 4 != 0 {
                    continue;
                }
                "nothing".into()
            }
            None => continue,
        };
        let src = frames.join(format!("frame_{f:06}.jpg"));
        if !src.exists() || locked_now.contains(&format!("{stem}_{f:06}.jpg")) {
            continue;
        }
        let dir = ds.join(format!("{id}_{lab}"));
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(&src, dir.join(format!("{stem}_{f:06}.jpg")))?;
        if lab != "nothing" {
            scan_labels.insert(format!("{stem}_{f:06}.jpg"), lab.clone());
        }
        if lab == "nothing" { neg += 1 } else { pos += 1 }
    }
    std::fs::write(&scan_p, serde_json::to_vec(&scan_labels)?)?;
    Ok((pos, neg))
}

/// event name + phrase (as words) for every detector in the game's settings
fn detector_phrases(game_dir: &Path) -> Result<Vec<(String, Vec<String>)>> {
    let lua = std::fs::read_to_string(game_dir.join("game_postprocess.lua"))?;
    let re = regex::Regex::new(r#"event\s*=\s*"([^"]+)"\s*,\s*crop\s*=\s*"[^"]*"\s*,\s*match\s*=\s*\{([^}]*)\}"#).unwrap();
    let mut dets: Vec<(String, Vec<String>)> = Vec::new();
    for c in re.captures_iter(&lua) {
        for w in c[2].split('"').skip(1).step_by(2) {
            let t = crate::discover::tokens(w);
            if !t.is_empty() && !dets.iter().any(|x| x.0 == c[1] && x.1 == t) {
                dets.push((c[1].to_string(), t));
            }
        }
    }
    Ok(dets)
}

/// Join OCR boxes that sit on the same line (pixel coords l,t,r,b) and return each line's words.
fn line_words(mut boxes: Vec<([f32; 4], String)>) -> Vec<([f32; 4], Vec<String>)> {
    boxes.sort_by(|a, b| a.0[0].total_cmp(&b.0[0]));
    let mut merged: Vec<([f32; 4], String)> = Vec::new();
    for (b, t) in boxes {
        let h = b[3] - b[1];
        let cy = (b[1] + b[3]) / 2.0;
        match merged.iter_mut().find(|(m, _)| {
            let mh = m[3] - m[1];
            ((m[1] + m[3]) / 2.0 - cy).abs() < 0.5 * h.max(mh) && b[0] - m[2] < 1.5 * h.max(mh) && b[0] >= m[0]
        }) {
            Some((m, mt)) => {
                m[2] = m[2].max(b[2]);
                m[1] = m[1].min(b[1]);
                m[3] = m[3].max(b[3]);
                mt.push(' ');
                mt.push_str(&t);
            }
            None => merged.push((b, t)),
        }
    }
    merged.iter().map(|(b, t)| (*b, crate::discover::tokens(t))).filter(|t| !t.1.is_empty()).collect()
}

/// SECOND CHECK of the training pictures: every picture is read again with OCR and must show its
/// folder's event. Pictures showing another event move to that event's folder; event pictures
/// with no readable event text move to <ID>_review (not used for training) so nothing wrong is
/// learned. Results are remembered, so each picture is only checked once.
pub fn verify(id: &str, game_dir: &Path, ds: &Path, ocr: &mut crate::ocr::Ocr, crops: &[crate::lua_engine::CropSpec]) -> Result<(usize, usize, usize)> {
    let dets = detector_phrases(game_dir)?;
    let cache_p = ds.join(".verified.json");
    let mut done: std::collections::HashMap<String, serde_json::Value> = std::fs::read(&cache_p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let (mut ok, mut moved, mut review) = (0, 0, 0);
    let prefix = format!("{id}_");
    let mut todo: Vec<(String, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(ds)?.flatten().filter(|e| e.path().is_dir()) {
        let name = e.file_name().to_string_lossy().to_string();
        let cls = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        if cls == "review" {
            continue;
        }
        for f in std::fs::read_dir(e.path())?.flatten() {
            let p = f.path();
            let key = format!("{name}/{}", f.file_name().to_string_lossy());
            if done.contains_key(&key) || !matches!(p.extension().and_then(|x| x.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(), Some("jpg" | "jpeg" | "png" | "webp")) {
                continue;
            }
            todo.push((cls.clone(), p));
        }
    }
    let locked = trusted(ds);
    todo.retain(|(_, p)| !p.file_name().map(|f| locked.contains(&*f.to_string_lossy())).unwrap_or(false));
    let scan: std::collections::HashMap<String, String> = std::fs::read(ds.join(".scan.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let total = todo.len();
    for (i, (cls, p)) in todo.into_iter().enumerate() {
        if i % 50 == 0 && total > 0 {
            eprintln!("[model] checking pictures {i}/{total}");
        }
        let Ok(img) = image::open(&p) else { continue };
        let img = img.to_rgb8();
        let mut boxes = Vec::new();
        for c in crops {
            let (x, y, _, _) = crate::ocr::crop_rect(img.width(), img.height(), &c.coords);
            if let Ok(r) = ocr.run_crop(&img, c) {
                boxes.extend(r.boxes.into_iter().map(|b| ([(b.x0 + x) as f32, (b.y0 + y) as f32, (b.x1 + x) as f32, (b.y1 + y) as f32], b.text)));
            }
        }
        let lines = line_words(boxes);
        let mut found: Vec<&(String, Vec<String>)> = dets.iter().filter(|d| lines.iter().any(|l| l.1 == d.1)).collect();
        found.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        // where on screen the event's text was (for the fourth check)
        let (iw, ih) = (img.width() as f32, img.height() as f32);
        let place = |ev: &str| -> serde_json::Value {
            dets.iter().filter(|d| d.0 == ev).find_map(|d| lines.iter().find(|l| l.1 == d.1)).map(|l| serde_json::json!([((l.0[0] + l.0[2]) / 2.0 / iw), ((l.0[1] + l.0[3]) / 2.0 / ih)])).unwrap_or(serde_json::json!(true))
        };
        let target = if found.iter().any(|d| d.0 == cls) {
            None
        } else if let Some(best) = found.first() {
            Some(best.0.clone())
        } else if cls == "nothing" {
            None
        } else if scan.get(&*p.file_name().unwrap().to_string_lossy()).map(|l| *l == cls).unwrap_or(false) {
            // the scan already read this event's text here (the saved picture is smaller, so OCR
            // can miss small text now): no contradiction, keep it
            None
        } else {
            Some("review".to_string())
        };
        let fname = p.file_name().unwrap().to_string_lossy().to_string();
        match target {
            None => {
                ok += 1;
                done.insert(format!("{prefix}{cls}/{fname}"), place(&cls));
            }
            Some(t) => {
                let dir = ds.join(format!("{prefix}{t}"));
                std::fs::create_dir_all(&dir)?;
                std::fs::rename(&p, dir.join(&fname))?;
                if t == "review" { review += 1 } else { moved += 1; done.insert(format!("{prefix}{t}/{fname}"), place(&t)); }
            }
        }
        if i % 50 == 49 {
            let _ = std::fs::write(&cache_p, serde_json::to_vec(&done)?);
        }
    }
    std::fs::write(&cache_p, serde_json::to_vec(&done)?)?;
    Ok((ok, moved, review))
}

/// FOURTH CHECK (where on screen): an event's text normally appears in the same place (the kill
/// popup in the middle, not the kill feed in a corner). Pictures whose event text was far from
/// where that event usually shows are moved to <ID>_review. Only applied when the outliers are a
/// small minority, so an event that genuinely appears in two places keeps both.
/// Each event's detector areas (crop rectangles) from the game's settings.
pub fn detector_areas(game_dir: &Path) -> std::collections::HashMap<String, Vec<[f32; 4]>> {
    let mut out: std::collections::HashMap<String, Vec<[f32; 4]>> = Default::default();
    let Ok(lua) = std::fs::read_to_string(game_dir.join("game_postprocess.lua")) else { return out };
    let crop_re = regex::Regex::new(r#"cropName\s*=\s*"([^"]+)"[^}]*?cropCoords\s*=\s*\{\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)\s*,\s*([0-9.]+)"#).unwrap();
    let crops: std::collections::HashMap<String, [f32; 4]> = crop_re.captures_iter(&lua).map(|c| (c[1].to_string(), [c[2].parse().unwrap_or(0.0), c[3].parse().unwrap_or(0.0), c[4].parse().unwrap_or(1.0), c[5].parse().unwrap_or(1.0)])).collect();
    let det_re = regex::Regex::new(r#"event\s*=\s*"([^"]+)"\s*,\s*crop\s*=\s*"([^"]+)""#).unwrap();
    for c in det_re.captures_iter(&lua) {
        if let Some(r) = crops.get(&c[2]) {
            let e = out.entry(c[1].to_string()).or_default();
            if !e.contains(r) {
                e.push(*r);
            }
        }
    }
    out
}

pub fn location_check_in(id: &str, ds: &Path, areas: &std::collections::HashMap<String, Vec<[f32; 4]>>) -> Result<usize> {
    let locked = trusted(ds);
    let cache_p = ds.join(".verified.json");
    let done: std::collections::HashMap<String, serde_json::Value> = std::fs::read(&cache_p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut by_class: std::collections::BTreeMap<String, Vec<(PathBuf, f32, f32)>> = Default::default();
    for (key, v) in &done {
        let (Some(cx), Some(cy)) = (v.get(0).and_then(|x| x.as_f64()), v.get(1).and_then(|x| x.as_f64())) else { continue };
        let p = ds.join(key);
        if !p.exists() || locked.contains(key.rsplit('/').next().unwrap_or("")) {
            continue;
        }
        let cls = key.split('/').next().unwrap_or("").to_string();
        by_class.entry(cls).or_default().push((p, cx as f32, cy as f32));
    }
    let review = ds.join(format!("{id}_review"));
    let mut moved = 0;
    let mut all: Vec<(usize, f32, f32)> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut class_areas: Vec<Vec<[f32; 4]>> = Vec::new();
    for (ci, (cls, items)) in by_class.into_iter().enumerate() {
        class_areas.push(areas.get(cls.strip_prefix(&format!("{id}_")).unwrap_or(&cls)).cloned().unwrap_or_default());
        for (p, x, y) in items {
            all.push((ci, x, y));
            files.push(p);
        }
    }
    let far = location_flags_in(&all, &class_areas);
    if !far.is_empty() {
        std::fs::create_dir_all(&review)?;
    }
    for i in far {
        if let Some(f) = files[i].file_name() {
            if std::fs::rename(&files[i], review.join(f)).is_ok() {
                moved += 1;
            }
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod location_tests {
    #[test]
    fn moves_far_ones() {
        let ds = std::env::temp_dir().join("rw_loc_test");
        let _ = std::fs::remove_dir_all(&ds);
        let k = ds.join("G_kill");
        std::fs::create_dir_all(&k).unwrap();
        let mut done = serde_json::Map::new();
        for i in 0..20 {
            std::fs::write(k.join(format!("a{i}.jpg")), b"x").unwrap();
            done.insert(format!("G_kill/a{i}.jpg"), serde_json::json!([0.5 + (i as f32) * 0.002, 0.45]));
        }
        for i in 0..3 {
            std::fs::write(k.join(format!("far{i}.jpg")), b"x").unwrap();
            done.insert(format!("G_kill/far{i}.jpg"), serde_json::json!([0.05, 0.1]));
        }
        std::fs::write(ds.join(".verified.json"), serde_json::to_vec(&done).unwrap()).unwrap();
        assert_eq!(super::location_check_in("G", &ds, &Default::default()).unwrap(), 3);
        assert!(ds.join("G_review/far0.jpg").exists());
        assert!(k.join("a0.jpg").exists());
    }
}

// ---------- simulation (tests the five checks on a pool of pictures with known answers) ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct SimItem {
    pub truth: String,
    pub text: Option<String>,
    pub loc: Option<(f32, f32)>,
    pub feat: Vec<f32>,
}

/// Read every pool picture once: image features + what OCR reads (exact event phrase and where).
pub fn sim_prepare(dir: &Path, phrases: &[(String, Vec<String>)], ocr: &mut crate::ocr::Ocr) -> Result<Vec<SimItem>> {
    let manifest: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(dir.join("pool.json"))?)?;
    let mut ex = Extractor::best();
    let crop = crate::lua_engine::CropSpec { name: "All".into(), coords: [0.0, 0.0, 1.0, 1.0], dilate_diameter: 5, min_area: 40, margin: 8, stretch_vertical: false, restricted_chars: String::new(), debug: false, legacy: false };
    let mut out = Vec::new();
    for (i, m) in manifest.iter().enumerate() {
        if i % 100 == 0 {
            eprintln!("[sim] reading pool {i}/{}", manifest.len());
        }
        let img = image::open(dir.join(m["file"].as_str().unwrap_or("")))?.to_rgb8();
        let r = ocr.run_crop(&img, &crop)?;
        let lines = line_words(r.boxes.into_iter().map(|b| ([b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32], b.text)).collect());
        let mut text = None;
        let mut loc = None;
        for (ev, ph) in phrases {
            if let Some(l) = lines.iter().find(|l| &l.1 == ph) {
                if text.as_ref().map(|t: &String| phrases.iter().find(|p| &p.0 == t).map(|p| p.1.len()).unwrap_or(0) < ph.len()).unwrap_or(true) {
                    text = Some(ev.clone());
                    loc = Some(((l.0[0] + l.0[2]) / 2.0 / img.width() as f32, (l.0[1] + l.0[3]) / 2.0 / img.height() as f32));
                }
            }
        }
        out.push(SimItem { truth: m["truth"].as_str().unwrap_or("").into(), text, loc, feat: ex.features(&img)? });
    }
    Ok(out)
}

/// One simulated run: `mistakes` pictures filed under a wrong event and `good` filed correctly,
/// then the checks in `order` (1 = sort by text, 2 = re-read text, 3 = look-alikes,
/// 4 = position, 5 = confident learning). Returns (mistakes fixed, mistakes sent to review,
/// mistakes missed, good kept, good sent to review, good moved to a wrong folder).
/// For the simulator: every pool picture's 60 closest look-alikes (cosine on standardised features),
/// worked out once so each simulated run doesn't redo the slow comparison.
pub fn sim_neighbours(pool: &[SimItem]) -> Vec<Vec<u32>> {
    let n = pool.len();
    let d = pool[0].feat.len();
    let mut mean = vec![0f32; d];
    let mut sd = vec![0f32; d];
    for it in pool { for j in 0..d { mean[j] += it.feat[j] / n as f32 } }
    for it in pool { for j in 0..d { sd[j] += (it.feat[j] - mean[j]).powi(2) / n as f32 } }
    for v in sd.iter_mut() { *v = v.sqrt().max(1e-3) }
    let xs: Vec<Vec<f32>> = pool.iter().map(|it| { let v: Vec<f32> = (0..d).map(|j| (it.feat[j] - mean[j]) / sd[j]).collect(); let nn = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6); v.iter().map(|x| x / nn).collect() }).collect();
    let rows: Vec<Vec<u32>> = std::thread::scope(|sc| {
        let half = n / 2;
        let xs = &xs;
        let job = |a: usize, b: usize| move || -> Vec<Vec<u32>> {
            (a..b).map(|i| {
                let mut sims: Vec<(f32, u32)> = (0..n).filter(|&j| j != i).map(|j| (xs[i].iter().zip(&xs[j]).map(|(x, y)| x * y).sum::<f32>(), j as u32)).collect();
                sims.select_nth_unstable_by(60, |a, b| b.0.total_cmp(&a.0));
                let mut top: Vec<(f32, u32)> = sims.into_iter().take(61).collect();
                top.sort_by(|a, b| b.0.total_cmp(&a.0));
                top.into_iter().take(60).map(|x| x.1).collect()
            }).collect()
        };
        let h1 = sc.spawn(job(0, half));
        let h2 = sc.spawn(job(half, n));
        let mut r = h1.join().unwrap();
        r.extend(h2.join().unwrap());
        r
    });
    rows
}

pub fn sim_run(pool: &[SimItem], classes: &[String], mistakes: usize, good: usize, order: &[u8], seed: u64) -> [usize; 6] {
    sim_run_fast(pool, None, &[], classes, mistakes, good, order, seed)
}

/// `nbrs` = sim_neighbours (fast look-alike check), `areas[class]` = detector areas.
pub fn sim_run_fast(pool: &[SimItem], nbrs: Option<&[Vec<u32>]>, areas: &[Vec<[f32; 4]>], classes: &[String], mistakes: usize, good: usize, order: &[u8], seed: u64) -> [usize; 6] {
    let mut rng = seed.wrapping_mul(0x9e3779b97f4a7c15) | 1;
    let mut next = move || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
    let mut idx: Vec<usize> = (0..pool.len()).collect();
    for i in (1..idx.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        idx.swap(i, j);
    }
    let pick: Vec<usize> = idx.into_iter().take(mistakes + good).collect();
    let ci = |s: &str| classes.iter().position(|c| c == s).unwrap_or(0);
    let truth: Vec<usize> = pick.iter().map(|&p| ci(&pool[p].truth)).collect();
    // folder per picture (None = review); the first `mistakes` get a wrong folder
    let mut folder: Vec<Option<usize>> = truth.iter().enumerate().map(|(i, &t)| {
        if i < mistakes { let mut w = (next() as usize) % (classes.len() - 1); if w >= t { w += 1 } Some(w) } else { Some(t) }
    }).collect();
    let nothing = ci("nothing");
    // features normalised like training does
    let d = pool[0].feat.len();
    let mut xs: Vec<Vec<f32>> = pick.iter().map(|&p| pool[p].feat.clone()).collect();
    let n = xs.len();
    let mut mean = vec![0f32; d];
    let mut sd = vec![0f32; d];
    for x in &xs { for j in 0..d { mean[j] += x[j] / n as f32 } }
    for x in &xs { for j in 0..d { sd[j] += (x[j] - mean[j]).powi(2) / n as f32 } }
    for v in sd.iter_mut() { *v = v.sqrt().max(1e-3) }
    for x in xs.iter_mut() { for j in 0..d { x[j] = (x[j] - mean[j]) / sd[j] } }
    let mut confirmed = vec![false; n];
    for &f in order {
        let live: Vec<usize> = (0..n).filter(|&i| folder[i].is_some()).collect();
        match f {
            1 => for &i in &live { if let Some(t) = &pool[pick[i]].text { folder[i] = Some(ci(t)) } },
            2 => for &i in &live {
                match &pool[pick[i]].text {
                    Some(t) if Some(ci(t)) == folder[i] => confirmed[i] = true,
                    Some(t) => { folder[i] = Some(ci(t)); confirmed[i] = true }
                    None if folder[i] == Some(nothing) => {}
                    None => folder[i] = None,
                }
            },
            3 if nbrs.is_some() => {
                let nb = nbrs.unwrap();
                let mut run_of = vec![usize::MAX; pool.len()];
                for &i in &live { run_of[pick[i]] = i }
                let mut count = vec![0usize; classes.len()];
                for &i in &live { count[folder[i].unwrap()] += 1 }
                let mut fl = Vec::new();
                for &i in &live {
                    let y = folder[i].unwrap();
                    if count[y] < 15 { continue }
                    let mut votes = vec![0usize; classes.len()];
                    let mut taken = 0;
                    for &j in &nb[pick[i]] {
                        let r = run_of[j as usize];
                        if r == usize::MAX || r == i { continue }
                        votes[folder[r].unwrap()] += 1;
                        taken += 1;
                        if taken == 7 { break }
                    }
                    let (best, nbv) = votes.iter().enumerate().max_by_key(|x| *x.1).map(|(c, v)| (c, *v)).unwrap();
                    if best != y && nbv >= 5 && votes[y] <= 1 { fl.push(i) }
                }
                for i in fl { folder[i] = None }
            }
            3 | 5 => {
                let sx: Vec<Vec<f32>> = live.iter().map(|&i| xs[i].clone()).collect();
                let sy: Vec<usize> = live.iter().map(|&i| folder[i].unwrap()).collect();
                let fl = if f == 3 { knn_flags(&sx, &sy, classes.len()) } else { cl_flags(&sx, &sy, classes.len(), d) };
                for k in fl { folder[live[k]] = None }
            }
            4 => {
                // positions are known for pictures whose text was read in check 2 (or check 1)
                let with: Vec<usize> = live.iter().copied().filter(|&i| pool[pick[i]].loc.is_some() && (confirmed[i] || order.iter().position(|&o| o == 2) < order.iter().position(|&o| o == 4))).collect();
                let items: Vec<(usize, f32, f32)> = with.iter().map(|&i| { let l = pool[pick[i]].loc.unwrap(); (folder[i].unwrap(), l.0, l.1) }).collect();
                for k in location_flags_in(&items, areas) { folder[with[k]] = None }
            }
            _ => {}
        }
    }
    let mut r = [0usize; 6];
    for i in 0..n {
        let m = i < mistakes;
        let slot = match folder[i] { None => 1, Some(f) if f == truth[i] => 0, Some(_) => 2 };
        r[if m { slot } else { 3 + slot }] += 1;
    }
    r
}

/// Check 1 applied again (it runs in 4th place): a picture whose scan text named a different event
/// than the folder it's in now goes back to that event's folder. Pictures in review stay there.
pub fn rescan_sort(id: &str, ds: &Path) -> Result<usize> {
    let scan: std::collections::HashMap<String, String> = std::fs::read(ds.join(".scan.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let locked = trusted(ds);
    let prefix = format!("{id}_");
    let mut moved = 0;
    for e in std::fs::read_dir(ds)?.flatten().filter(|e| e.path().is_dir()) {
        let name = e.file_name().to_string_lossy().to_string();
        let cls = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        if cls == "review" {
            continue;
        }
        for f in std::fs::read_dir(e.path())?.flatten() {
            let fname = f.file_name().to_string_lossy().to_string();
            if locked.contains(&fname) {
                continue;
            }
            if let Some(l) = scan.get(&fname).filter(|l| **l != cls) {
                let dir = ds.join(format!("{prefix}{l}"));
                std::fs::create_dir_all(&dir)?;
                if std::fs::rename(f.path(), dir.join(&fname)).is_ok() {
                    moved += 1;
                }
            }
        }
    }
    Ok(moved)
}

// ---------- pictures people sorted themselves ----------

fn trusted(ds: &Path) -> std::collections::HashSet<String> {
    std::fs::read(ds.join(".trusted.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Undo the checks: every picture goes back to the folder it was in before they ran (taken from
/// the picture cache, which records each picture's folder when it's read), and everything is then
/// locked as sorted by hand, so no check moves it again. Returns pictures moved back.
pub fn restore_and_lock(id: &str, ds: &Path) -> Result<(usize, usize)> {
    let mut origin: std::collections::HashMap<String, String> = Default::default();
    for e in std::fs::read_dir(ds)?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with(".features-") && n.ends_with(".bin") {
            for k in load_cache(&e.path()).into_keys() {
                if let Some((dir, f)) = k.split_once('/') {
                    if !dir.ends_with("_review") {
                        origin.insert(f.to_string(), dir.to_string());
                    }
                }
            }
        }
    }
    let mut moved = 0;
    let already = trusted(ds);
    for e in std::fs::read_dir(ds)?.flatten().filter(|e| e.path().is_dir()) {
        let dir = e.file_name().to_string_lossy().to_string();
        for f in std::fs::read_dir(e.path())?.flatten() {
            let fname = f.file_name().to_string_lossy().to_string();
            // a locked picture is where the person put it (maybe moved by hand since): leave it
            if already.contains(&fname) {
                continue;
            }
            if let Some(o) = origin.get(&fname).filter(|o| **o != dir) {
                let to = ds.join(o);
                std::fs::create_dir_all(&to)?;
                if std::fs::rename(f.path(), to.join(&fname)).is_ok() {
                    moved += 1;
                }
            }
        }
    }
    // lock everything that's in an event / nothing folder now
    let mut lock = trusted(ds);
    let prefix = format!("{id}_");
    for e in std::fs::read_dir(ds)?.flatten().filter(|e| e.path().is_dir()) {
        let dir = e.file_name().to_string_lossy().to_string();
        if dir == format!("{prefix}review") {
            continue;
        }
        for f in std::fs::read_dir(e.path())?.flatten() {
            lock.insert(f.file_name().to_string_lossy().to_string());
        }
    }
    let n = lock.len();
    std::fs::write(ds.join(".trusted.json"), serde_json::to_vec(&lock)?)?;
    Ok((moved, n))
}
