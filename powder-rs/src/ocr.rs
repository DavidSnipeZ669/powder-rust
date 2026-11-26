//! PaddleOCR (PP-OCR) text detection + recognition for HUD crops.
//!
//! Mirrors PowderRecorderCli's PaddleOCR pipeline (struct PaddleCropData / PaddleInputSection):
//!   1. cut the crop (normalised l,t,r,b) out of the frame
//!   2. scale it to fit the detector input (512x288) and run the text detector
//!   3. threshold + dilate the probability map (detectorDilateDiameter), take connected
//!      components, drop those whose bounding-box area < detectorMinimumArea, grow by detectorMargin
//!      (areas/margins are measured in detector pixels, exactly like Powder's configs assume)
//!   4. recognise each box (48 px high, up to 320 px wide) with CTC greedy decoding,
//!      optionally restricted to `restrictedCharacters`
//!   5. join the box texts (reading order) into one string per crop per frame.

use crate::lua_engine::CropSpec;
use anyhow::{anyhow, Context, Result};
use image::{imageops, Rgb, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;

pub const DET_W: u32 = 512;
pub const DET_H: u32 = 288;
pub const REC_H: u32 = 48;
pub const REC_W: u32 = 320;
const DET_THRESH: f32 = 0.3;

pub struct Ocr {
    det: Session,
    rec: Session,
    det_out: String,
    rec_out: String,
    /// index 0 = CTC blank
    chars: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TextBox {
    /// in crop pixel coordinates (original frame scale)
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct CropResult {
    pub text: String,
    pub boxes: Vec<TextBox>,
}

pub fn crop_rect(img_w: u32, img_h: u32, c: &[f32; 4]) -> (u32, u32, u32, u32) {
    let cl = |v: f32| v.clamp(0.0, 1.0);
    let x0 = (cl(c[0]) * img_w as f32).floor() as u32;
    let y0 = (cl(c[1]) * img_h as f32).floor() as u32;
    let x1 = ((cl(c[2]) * img_w as f32).ceil() as u32).max(x0 + 1).min(img_w);
    let y1 = ((cl(c[3]) * img_h as f32).ceil() as u32).max(y0 + 1).min(img_h);
    (x0, y0, x1 - x0, y1 - y0)
}

impl Ocr {
    pub fn load(models_dir: &Path) -> Result<Self> {
        let pick = |cands: &[&str]| -> Result<std::path::PathBuf> {
            cands
                .iter()
                .map(|c| models_dir.join(c))
                .find(|p| p.exists())
                .ok_or_else(|| anyhow!("none of {:?} found in {}", cands, models_dir.display()))
        };
        let det_p = pick(&["det.onnx", "ch_PP-OCRv4_det_infer.onnx", "en_PP-OCRv4_det_infer.onnx"])?;
        let rec_p = pick(&["rec.onnx", "ch_PP-OCRv4_rec_infer.onnx", "en_PP-OCRv4_rec_infer.onnx"])?;
        let det = crate::runtime::session(&det_p)?;
        let rec = crate::runtime::session(&rec_p)?;
        // Character list: prefer a dict file, otherwise the model's "character" metadata.
        let mut chars = vec![String::new()];
        let dict = ["dict.txt", "ppocr_keys_v1.txt", "en_dict.txt"].iter().map(|d| models_dir.join(d)).find(|p| p.exists());
        if let Some(d) = dict {
            let txt = std::fs::read_to_string(&d).with_context(|| format!("reading {}", d.display()))?;
            chars.extend(txt.lines().map(|l| l.trim_end_matches('\r').to_string()));
        } else {
            let meta = rec.metadata().map_err(|e| anyhow!("{e}"))?;
            let c = meta.custom("character").context("no dict.txt and no 'character' metadata in rec model")?;
            chars.extend(c.lines().map(|l| l.to_string()));
        }
        chars.push(" ".into());
        let det_out = det.outputs()[0].name().to_string();
        let rec_out = rec.outputs()[0].name().to_string();
        Ok(Self { det, rec, det_out, rec_out, chars })
    }

    pub fn run_crop(&mut self, frame: &RgbImage, spec: &CropSpec) -> Result<CropResult> {
        self.run_crop_sized(frame, spec, DET_W, DET_H)
    }

    /// Same as run_crop but with a custom detector input size (used for full-frame discovery).
    pub fn run_crop_sized(&mut self, frame: &RgbImage, spec: &CropSpec, det_w: u32, det_h: u32) -> Result<CropResult> {
        let (cx, cy, cw, ch) = crop_rect(frame.width(), frame.height(), &spec.coords);
        let crop = imageops::crop_imm(frame, cx, cy, cw, ch).to_image();
        let s = (det_w as f32 / cw as f32).min(det_h as f32 / ch as f32);
        let sw = ((cw as f32 * s).round() as u32).clamp(1, det_w);
        let sh = ((ch as f32 * s).round() as u32).clamp(1, det_h);
        let scaled = imageops::resize(&crop, sw, sh, imageops::FilterType::Triangle);

        // detector input: BGR, ImageNet normalisation, zero padding
        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];
        let mut inp = Array4::<f32>::zeros((1, 3, det_h as usize, det_w as usize));
        for (x, y, p) in scaled.enumerate_pixels() {
            for c in 0..3 {
                let v = p[2 - c] as f32 / 255.0; // RGB -> BGR
                inp[[0, c, y as usize, x as usize]] = (v - mean[2 - c]) / std[2 - c];
            }
        }
        let prob: Vec<f32> = {
            let out = self.det.run(ort::inputs![TensorRef::from_array_view(&inp)?]).map_err(|e| anyhow!("det: {e}"))?;
            let (_, d) = out[self.det_out.as_str()].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
            d.to_vec()
        };

        let (w, h) = (sw as usize, sh as usize);
        let mut mask = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                mask[y * w + x] = prob[y * det_w as usize + x] > DET_THRESH;
            }
        }
        let mask = dilate(&mask, w, h, spec.dilate_diameter.max(1) as usize);
        let mut rects = components(&mask, w, h);
        rects.retain(|r| ((r.2 - r.0 + 1) * (r.3 - r.1 + 1)) as i32 >= spec.min_area);
        if rects.is_empty() {
            return Ok(CropResult::default());
        }
        // reading order: by line (vertical overlap), then left to right
        // (grouped explicitly: an "overlaps vertically" comparator is not a total order and panics)
        rects.sort_by_key(|r| (r.1, r.0));
        let mut lines: Vec<(usize, usize, Vec<_>)> = Vec::new(); // (top, bottom, rects)
        for r in rects.drain(..) {
            match lines.last_mut() {
                Some(l) if r.1.max(l.0) <= r.3.min(l.1) => {
                    l.1 = l.1.max(r.3);
                    l.2.push(r);
                }
                _ => lines.push((r.1, r.3, vec![r])),
            }
        }
        for (_, _, mut l) in lines {
            l.sort_by_key(|r| r.0);
            rects.extend(l);
        }

        let m = spec.margin.max(0) as usize;
        let mut boxes = Vec::new();
        for r in &rects {
            let x0 = r.0.saturating_sub(m);
            let y0 = r.1.saturating_sub(m);
            let x1 = (r.2 + m).min(w - 1);
            let y1 = (r.3 + m).min(h - 1);
            // back to crop coordinates (full resolution for better recognition)
            let bx0 = ((x0 as f32 / s).floor() as u32).min(cw - 1);
            let by0 = ((y0 as f32 / s).floor() as u32).min(ch - 1);
            let bx1 = (((x1 + 1) as f32 / s).ceil() as u32).clamp(bx0 + 1, cw);
            let by1 = (((y1 + 1) as f32 / s).ceil() as u32).clamp(by0 + 1, ch);
            boxes.push(TextBox { x0: bx0, y0: by0, x1: bx1, y1: by1, text: String::new() });
        }
        self.recognise(&crop, &mut boxes, spec)?;
        let text = boxes.iter().map(|b| b.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ");
        Ok(CropResult { text, boxes })
    }

    fn recognise(&mut self, crop: &RgbImage, boxes: &mut [TextBox], spec: &CropSpec) -> Result<()> {
        let n = boxes.len();
        let mut inp = Array4::<f32>::zeros((n, 3, REC_H as usize, REC_W as usize));
        for (i, b) in boxes.iter().enumerate() {
            let sub = imageops::crop_imm(crop, b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0).to_image();
            let (bw, bh) = (sub.width() as f32, sub.height() as f32);
            let tw = if spec.stretch_vertical {
                (bw as u32).clamp(8, REC_W)
            } else {
                ((bw * REC_H as f32 / bh).round() as u32).clamp(8, REC_W)
            };
            let r = imageops::resize(&sub, tw, REC_H, imageops::FilterType::Triangle);
            for (x, y, p) in r.enumerate_pixels() {
                for c in 0..3 {
                    inp[[i, c, y as usize, x as usize]] = (p[2 - c] as f32 / 255.0 - 0.5) / 0.5;
                }
            }
        }
        let out = self.rec.run(ort::inputs![TensorRef::from_array_view(&inp)?]).map_err(|e| anyhow!("rec: {e}"))?;
        let (shape, d) = out[self.rec_out.as_str()].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
        let (t_len, classes) = (shape[1] as usize, shape[2] as usize);
        let allowed: Option<Vec<bool>> = if spec.restricted_chars.is_empty() {
            None
        } else {
            let mut a = vec![false; classes];
            a[0] = true;
            for (i, c) in self.chars.iter().enumerate().skip(1).take(classes - 1) {
                if c.chars().count() == 1 && spec.restricted_chars.contains(c.as_str()) {
                    a[i] = true;
                }
            }
            Some(a)
        };
        for (i, b) in boxes.iter_mut().enumerate() {
            let mut s = String::new();
            let mut last = usize::MAX;
            for t in 0..t_len {
                let row = &d[(i * t_len + t) * classes..(i * t_len + t + 1) * classes];
                let mut best = 0usize;
                let mut bv = f32::MIN;
                for (k, &v) in row.iter().enumerate() {
                    if let Some(a) = &allowed {
                        if !a[k] {
                            continue;
                        }
                    }
                    if v > bv {
                        bv = v;
                        best = k;
                    }
                }
                if best != 0 && best != last {
                    if let Some(c) = self.chars.get(best) {
                        s.push_str(c);
                    }
                }
                last = best;
            }
            b.text = s;
        }
        Ok(())
    }
}

/// Square dilation (separable running max), kernel size `d`.
fn dilate(mask: &[bool], w: usize, h: usize, d: usize) -> Vec<bool> {
    if d <= 1 {
        return mask.to_vec();
    }
    let r = d / 2;
    let mut tmp = vec![false; w * h];
    for y in 0..h {
        let row = &mask[y * w..(y + 1) * w];
        let mut count = 0usize;
        // sliding window count over [x-r, x+r]
        for x in 0..(r.min(w)) {
            count += row[x] as usize;
        }
        for x in 0..w {
            if x + r < w {
                count += row[x + r] as usize;
            }
            if x > r {
                count -= row[x - r - 1] as usize;
            }
            tmp[y * w + x] = count > 0;
        }
    }
    let mut out = vec![false; w * h];
    for x in 0..w {
        let mut count = 0usize;
        for y in 0..(r.min(h)) {
            count += tmp[y * w + x] as usize;
        }
        for y in 0..h {
            if y + r < h {
                count += tmp[(y + r) * w + x] as usize;
            }
            if y > r {
                count -= tmp[(y - r - 1) * w + x] as usize;
            }
            out[y * w + x] = count > 0;
        }
    }
    out
}

/// 8-connected components -> bounding boxes (x0, y0, x1, y1) inclusive.
fn components(mask: &[bool], w: usize, h: usize) -> Vec<(usize, usize, usize, usize)> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            for dy in [-1i64, 0, 1] {
                for dx in [-1i64, 0, 1] {
                    let nx = x as i64 + dx;
                    let ny = y as i64 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push((x0, y0, x1, y1));
    }
    out
}

/// Draw crop + detected boxes for debugging/calibration.
pub fn debug_image(frame: &RgbImage, spec: &CropSpec, res: &CropResult) -> RgbImage {
    let (cx, cy, cw, ch) = crop_rect(frame.width(), frame.height(), &spec.coords);
    let mut crop = imageops::crop_imm(frame, cx, cy, cw, ch).to_image();
    for b in &res.boxes {
        draw_rect(&mut crop, b.x0, b.y0, b.x1.saturating_sub(1), b.y1.saturating_sub(1), Rgb([0, 255, 0]));
    }
    crop
}

pub fn draw_rect(img: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, c: Rgb<u8>) {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return;
    }
    let (x1, y1) = (x1.min(w - 1), y1.min(h - 1));
    for t in 0..2u32 {
        for x in x0..=x1 {
            if y0 + t < h { img.put_pixel(x, y0 + t, c); }
            if y1 >= t { img.put_pixel(x, y1 - t, c); }
        }
        for y in y0..=y1 {
            if x0 + t < w { img.put_pixel(x0 + t, y, c); }
            if x1 >= t { img.put_pixel(x1 - t, y, c); }
        }
    }
}
