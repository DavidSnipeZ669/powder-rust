//! Port of src/main/features/montage-maker + powder-native/MontageMakerProcess: every export (clips,
//! editor projects, auto-montages) is rendered by Powder's native montage_maker.
use crate::host::Host;
use crate::native::{Accel, NativeProcess};
use crate::server::Ctx;
use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};

pub fn uuid() -> String {
    let t = crate::server::random_token();
    format!("{}-{}-4{}-a{}-{}", &t[0..8], &t[8..12], &t[13..16], &t[17..20], &t[20..32])
}

/// A running montage_maker (also used for the editor's live preview stream).
pub struct MontageProcess {
    proc: NativeProcess,
    killed: std::sync::atomic::AtomicBool,
    done: Mutex<Option<std::sync::mpsc::Receiver<Result<()>>>>,
    chunks: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next_req: std::sync::atomic::AtomicU64,
}

impl MontageProcess {
    pub fn start<P: Fn(f64) + Send + Sync + 'static>(input: &Value, on_progress: P) -> Result<Arc<MontageProcess>> {
        let (tx, rx) = channel::<Result<()>>();
        let tx = Mutex::new(Some(tx));
        let chunks: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let ch2 = chunks.clone();
        let proc = NativeProcess::start(
            "montage_maker",
            vec![],
            Accel::from_settings(),
            Box::new(move |name, data| match name {
                "montage_progress" => on_progress(data.as_f64().unwrap_or(0.0)),
                "montage_finished" => {
                    if let Some(t) = tx.lock().take() {
                        let _ = t.send(Ok(()));
                    }
                }
                "chunk_render_result" => {
                    if let Some(s) = ch2.lock().remove(&data["requestId"].as_u64().unwrap_or(0)) {
                        let _ = s.send(data["result"].clone());
                    }
                }
                n if n.ends_with("ERROR_JSON") || n == "connection-closed" => {
                    if n == "connection-closed" {
                        // unblock every chunk still waiting (an endless wait here froze the
                        // interface after leaving the editor)
                        ch2.lock().clear();
                    }
                    if let Some(t) = tx.lock().take() {
                        let _ = t.send(Err(anyhow::anyhow!("montage_maker: {n} {data}")));
                    }
                }
                _ => {}
            }),
        )?;
        let _ = proc.push("start", Some(input));
        Ok(Arc::new(MontageProcess { proc, killed: Default::default(), done: Mutex::new(Some(rx)), chunks, next_req: std::sync::atomic::AtomicU64::new(1) }))
    }

    pub fn wait(&self) -> Result<()> {
        let rx = self.done.lock().take();
        let r = match rx {
            Some(rx) => rx.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("connection closed"))),
            None => Ok(()),
        };
        if r.is_err() && self.killed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(anyhow::Error::new(crate::native::Aborted));
        }
        r
    }

    pub fn update(&self, input: &Value) {
        let _ = self.proc.push("start", Some(input));
    }

    pub fn render_chunk(&self, index: u64, out: &str) -> Result<Value> {
        let id = self.next_req.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = channel();
        self.chunks.lock().insert(id, tx);
        if let Err(e) = self.proc.call("render_chunk", Some(&json!({"chunkIndex": index, "outputPath": out, "requestId": id}))) {
            self.chunks.lock().remove(&id);
            return Err(e);
        }
        // never wait forever: a chunk that was cancelled or whose process ended gets no answer
        let r = rx.recv_timeout(std::time::Duration::from_secs(60));
        self.chunks.lock().remove(&id);
        Ok(r.map_err(|_| anyhow::anyhow!("chunk {index} was cancelled or took too long"))?)
    }

    pub fn cancel_all_chunks(&self) -> Result<()> {
        // cancelled chunks get no result: release everything waiting for one
        self.chunks.lock().clear();
        self.proc.call("cancel_all_render_chunk", None)?;
        Ok(())
    }

    pub fn thumbnail(&self, t: f64, exact: bool) -> Result<Value> {
        self.proc.call("generate_thumbnail", Some(&json!({"timestampSec": t, "exactTime": exact})))
    }

    pub fn stop(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.chunks.lock().clear();
        self.proc.kill();
    }
}

static OUTROS: OnceLock<(Option<PathBuf>, Option<PathBuf>)> = OnceLock::new(); // (horizontal, vertical)

fn find_outros(ctx: &Ctx) -> (Option<PathBuf>, Option<PathBuf>) {
    let dir = ctx.dist.native.join("main");
    let mut h = None;
    let mut v = None;
    for f in ["3df6b60e693248eb71cc9c497c400ac7.mp4", "90a752d391749a4362255d7ea209cf8e.mp4"] {
        let p = dir.join(f);
        if let Ok(m) = crate::media::metadata(&p) {
            if m.height > m.width {
                v = Some(p);
            } else {
                h = Some(p);
            }
        }
    }
    (h, v)
}

fn format_crop(ow: f64, oh: f64, c: &[f64]) -> [f64; 4] {
    let (mut vw, mut vh) = (ow, oh);
    if ow / oh < 16.0 / 9.0 {
        vw = oh * 16.0 / 9.0;
    } else {
        vh = ow * 9.0 / 16.0;
    }
    [(1.0 - vw / ow) / 2.0 + c[0] * (vw / ow), (1.0 - vh / oh) / 2.0 + c[1] * (vh / oh), c[2] * (vw / ow), c[3] * (vh / oh)]
}

pub fn outro_path(vertical: bool) -> Option<PathBuf> {
    let (h, v) = OUTROS.get().cloned().unwrap_or((None, None));
    if vertical { v } else { h }
}

/// adaptCrops: a vertical template as native "croppings" for this output size.
pub fn adapt_crops(ow: f64, oh: f64, template: &Value) -> Value {
    json!({"type": uuid(), "crops": template.as_array().cloned().unwrap_or_default().into_iter().map(|mut c| {
        let inn: Vec<f64> = c["in"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
        if inn.len() == 4 { c["in"] = json!(format_crop(ow, oh, &inn)); }
        c
    }).collect::<Vec<_>>()})
}

/// convertMontageDescriptionToRawMontageDescription (passes raw descriptions through).
pub fn to_raw(d: &Value) -> Result<Value> {
    if d["output_resolution"].is_object() {
        return Ok(d.clone());
    }
    if !d["outputHeight"].is_number() {
        bail!("MontageMakerProcess: Invalid description");
    }
    let ow = d["outputWidth"].as_f64().unwrap_or(1920.0);
    let oh = d["outputHeight"].as_f64().unwrap_or(1080.0);
    let sections = d["sections"].as_array().cloned().unwrap_or_default();
    let horiz = sections.iter().all(|s| s["verticalTemplate"] == false);
    let vert = sections.iter().all(|s| s["verticalTemplate"] != false);
    if !horiz && !vert {
        bail!("MontageMakerProcess: Missing verticalTemplate in some sections");
    }
    let mut clips = vec![];
    let mut transitions = vec![];
    let mut croppings = vec![];
    for s in &sections {
        let cropping = s["verticalTemplate"].as_array().map(|t| {
            json!({"type": uuid(), "crops": t.iter().map(|c| {
                let mut c = c.clone();
                let inn: Vec<f64> = c["in"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                if inn.len() == 4 { c["in"] = json!(format_crop(ow, oh, &inn)); }
                c
            }).collect::<Vec<_>>()})
        });
        let mut clip = json!({"path": s["path"], "inputTimeRange": s["inputTimeRange"], "montageTimeRange": s["montageTimeRange"]});
        if let Some(c) = &cropping {
            clip["croppings"] = json!([c]);
            croppings.push(c.clone());
        }
        if !s["playbackSpeed"].is_null() {
            clip["playbackSpeed"] = s["playbackSpeed"].clone();
        }
        clips.push(clip);
        transitions.push(s["transition"].clone());
    }
    let height = if vert { 16.0 * oh / 9.0 } else { oh };
    let width = if vert { oh } else { ow };
    let overlays: Vec<Value> = d["overlays"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut o| {
            if o["image"]["type"] != "Base64ImageSpec" {
                let r = &o["destRectNormalised"];
                let c = [r["x"].as_f64().unwrap_or(0.0), r["y"].as_f64().unwrap_or(0.0), r["w"].as_f64().unwrap_or(1.0), r["h"].as_f64().unwrap_or(1.0)];
                let n = format_crop(width, height, &c);
                o["destRectNormalised"] = json!({"x": n[0], "y": n[1], "w": n[2], "h": n[3]});
            }
            o
        })
        .collect();
    let (oh_p, ov_p) = OUTROS.get().cloned().unwrap_or((None, None));
    let outro = if d["outro"] == true { if vert { ov_p } else { oh_p } } else { None };
    let mut raw = json!({
        "output_resolution": {"width": width, "height": height},
        "output_framerate": d["outputFramerate"], "outputPath": d["outputPath"],
        "transitions": transitions, "clips": clips, "overlays": overlays,
        "visualEffects": d.get("visualEffects").cloned().filter(|v| !v.is_null()).unwrap_or(json!([])),
        "musics": d.get("musics").cloned().filter(|v| !v.is_null()).unwrap_or(json!([])),
    });
    if let Some(o) = outro {
        raw["outro"] = json!(o);
    }
    if vert {
        raw["croppings"] = json!(croppings);
    }
    Ok(raw)
}

fn current() -> &'static Mutex<Option<Arc<MontageProcess>>> {
    static C: OnceLock<Mutex<Option<Arc<MontageProcess>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

pub fn set_state(h: &Host, patch: Value) {
    let cur = h.get("montage-maker", "montageMakerState").unwrap_or(json!({"processing": false, "progress": 0, "videosIds": []}));
    h.set("montage-maker", "montageMakerState", super::projects::merge(cur, patch));
}

pub fn is_processing(h: &Host) -> bool {
    h.get("montage-maker", "montageMakerState").map(|s| s["processing"] == true).unwrap_or(false)
}

/// launchMontageMakerProcess: one montage at a time; returns the output path.
pub fn launch<P: Fn(f64) + Send + Sync + 'static>(h: &'static Host, description: &Value, on_progress: P) -> Result<Value> {
    if current().lock().is_some() {
        bail!("MontageMaker: montage already running");
    }
    let raw = to_raw(description)?;
    if let Some(dir) = raw["outputPath"].as_str().and_then(|p| std::path::Path::new(p).parent()) {
        let _ = std::fs::create_dir_all(dir);
    }
    set_state(h, json!({"processing": true, "progress": 0}));
    crate::gpu::block_sleep("montage", true);
    let r = (|| {
        let p = MontageProcess::start(&raw, move |x| {
            on_progress(x);
            set_state(h, json!({"progress": x}));
        })?;
        *current().lock() = Some(p.clone());
        p.wait()
    })();
    *current().lock() = None;
    crate::gpu::block_sleep("montage", false);
    set_state(h, json!({"processing": false}));
    r.map(|_| description["outputPath"].clone())
}

pub fn stop() {
    if let Some(p) = current().lock().as_ref() {
        p.stop();
    }
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    let _ = OUTROS.set(find_outros(ctx));
    h.set("montage-maker", "montageMakerState", json!({"processing": false, "progress": 0, "videosIds": []}));
    h.handle("montage-maker", "stopMontageCreation", |_, _| {
        stop();
        Ok(Value::Null)
    });
}
