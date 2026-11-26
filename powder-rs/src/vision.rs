//! Visual-cue models: the open replacement for Powder's encrypted `.pwdmdl`.
//!
//! Powder's VideoHighlightModel reads crop sections + a layout string ("bchw") from ONNX
//! metadata and emits one float per class per frame, which Lua sees as
//! `modelOutputs['<head>::<class>']` (e.g. `visual_cues::kill_animation`).
//! powder-rs does the same, but the description lives in a readable `model.json` beside a
//! normal `model.onnx`, so anyone can train one (see training/).

use anyhow::{anyhow, bail, Context, Result};
use image::{imageops, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::TensorRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSpec {
    /// ONNX input name
    pub name: String,
    /// normalised l,t,r,b region of the frame fed to this input
    #[serde(default = "full")]
    pub crop: [f32; 4],
    pub width: u32,
    pub height: u32,
    #[serde(default = "imagenet_mean")]
    pub mean: [f32; 3],
    #[serde(default = "imagenet_std")]
    pub std: [f32; 3],
    #[serde(default)]
    pub bgr: bool,
}
fn full() -> [f32; 4] { [0.0, 0.0, 1.0, 1.0] }
fn imagenet_mean() -> [f32; 3] { [0.485, 0.456, 0.406] }
fn imagenet_std() -> [f32; 3] { [0.229, 0.224, 0.225] }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSpec {
    /// ONNX output name; also the cue prefix (`<name>::<class>`)
    pub name: String,
    /// "softmax" (one class per frame, e.g. in_out), "sigmoid" (multi-label) or "none"
    #[serde(default = "sigmoid")]
    pub activation: String,
    pub classes: Vec<String>,
}
fn sigmoid() -> String { "sigmoid".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    #[serde(default = "fmt")]
    pub format: String,
    #[serde(default = "onnx_name")]
    pub onnx: String,
    pub inputs: Vec<InputSpec>,
    pub outputs: Vec<OutputSpec>,
    #[serde(default)]
    pub notes: String,
}
fn fmt() -> String { "powder-rs-vision/1".into() }
fn onnx_name() -> String { "model.onnx".into() }

pub struct VisionModel {
    pub spec: ModelSpec,
    session: Session,
}

impl VisionModel {
    pub fn spec_path(game_dir: &Path) -> PathBuf { game_dir.join("model.json") }

    pub fn load(game_dir: &Path) -> Result<Option<Self>> {
        let p = Self::spec_path(game_dir);
        if !p.exists() {
            if game_dir.join("model.pwdmdl").exists() {
                eprintln!(
                    "note: {} has an encrypted Powder model.pwdmdl; powder-rs can't use it. \
                     Visual cues will be missing - train an open model (training/README.md) or use an OCR-only script.",
                    game_dir.display()
                );
            }
            return Ok(None);
        }
        let spec: ModelSpec = serde_json::from_str(&std::fs::read_to_string(&p)?).with_context(|| format!("parsing {}", p.display()))?;
        let session = crate::runtime::session(&game_dir.join(&spec.onnx))?;
        for i in &spec.inputs {
            if !session.inputs().iter().any(|x| x.name() == i.name) {
                bail!("model.json input '{}' is not an input of {}", i.name, spec.onnx);
            }
        }
        for o in &spec.outputs {
            if !session.outputs().iter().any(|x| x.name() == o.name) {
                bail!("model.json output '{}' is not an output of {}", o.name, spec.onnx);
            }
        }
        Ok(Some(Self { spec, session }))
    }

    pub fn cue_names(&self) -> Vec<String> {
        self.spec.outputs.iter().flat_map(|o| o.classes.iter().map(move |c| format!("{}::{}", o.name, c))).collect()
    }

    pub fn prepare(spec: &InputSpec, frame: &RgbImage) -> Array4<f32> {
        let (x, y, w, h) = crate::ocr::crop_rect(frame.width(), frame.height(), &spec.crop);
        let c = imageops::crop_imm(frame, x, y, w, h).to_image();
        let r = imageops::resize(&c, spec.width, spec.height, imageops::FilterType::Triangle);
        let mut a = Array4::<f32>::zeros((1, 3, spec.height as usize, spec.width as usize));
        for (px, py, p) in r.enumerate_pixels() {
            for ch in 0..3 {
                let src = if spec.bgr { 2 - ch } else { ch };
                a[[0, ch, py as usize, px as usize]] = (p[src] as f32 / 255.0 - spec.mean[ch]) / spec.std[ch];
            }
        }
        a
    }

    /// Returns `<head>::<class>` -> score for one frame.
    pub fn run(&mut self, frame: &RgbImage) -> Result<BTreeMap<String, f32>> {
        let arrays: Vec<Array4<f32>> = self.spec.inputs.iter().map(|i| Self::prepare(i, frame)).collect();
        let mut inputs: Vec<(String, ort::session::SessionInputValue)> = Vec::new();
        for (i, a) in self.spec.inputs.iter().zip(arrays.iter()) {
            inputs.push((i.name.clone(), TensorRef::from_array_view(a)?.into()));
        }
        let out = self.session.run(inputs).map_err(|e| anyhow!("vision model: {e}"))?;
        let mut res = BTreeMap::new();
        for o in &self.spec.outputs {
            let (_, d) = out[o.name.as_str()].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
            let n = o.classes.len();
            if d.len() < n {
                bail!("output {} has {} values but model.json lists {} classes", o.name, d.len(), n);
            }
            let v = &d[..n];
            let probs: Vec<f32> = match o.activation.as_str() {
                "softmax" => {
                    let m = v.iter().cloned().fold(f32::MIN, f32::max);
                    let e: Vec<f32> = v.iter().map(|x| (x - m).exp()).collect();
                    let s: f32 = e.iter().sum();
                    e.iter().map(|x| x / s).collect()
                }
                "sigmoid" => v.iter().map(|x| 1.0 / (1.0 + (-x).exp())).collect(),
                _ => v.to_vec(),
            };
            for (c, p) in o.classes.iter().zip(probs) {
                res.insert(format!("{}::{}", o.name, c), p);
            }
        }
        Ok(res)
    }
}
