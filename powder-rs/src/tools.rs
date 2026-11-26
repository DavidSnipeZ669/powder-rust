//! Game-maker tools: new-game, calibrate, ocr-test, validate, extract-frames.

use crate::game::{GameMeta, GamePack};
use crate::lua_engine::{CropSpec, GameScript};
use crate::ocr::{crop_rect, draw_rect, Ocr};
use anyhow::{bail, Context, Result};
use image::{imageops, Rgb, RgbImage};
use std::path::{Path, PathBuf};

pub const TEMPLATE_LUA: &str = include_str!("../templates/game_postprocess.lua");
pub const TEMPLATE_EVENTS: &str = include_str!("../templates/events.json");

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 12 && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn new_game(dir: &Path, id: &str, name: &str, steam: Option<String>, twitch: Option<String>) -> Result<PathBuf> {
    if !valid_id(id) {
        bail!("game id '{id}' must be 1-12 chars of A-Z, 0-9 or _ (Powder style, e.g. DF, MARVELR, ARCRDR)");
    }
    let g = dir.join(id);
    if g.exists() {
        bail!("{} already exists", g.display());
    }
    std::fs::create_dir_all(g.join("assets"))?;
    std::fs::write(g.join("game_postprocess.lua"), TEMPLATE_LUA.replace("{{ID}}", id).replace("{{NAME}}", name))?;
    std::fs::write(g.join("events.json"), TEMPLATE_EVENTS.replace("{{ID}}", id))?;
    let meta = GameMeta {
        name: name.to_string(),
        steam_id: steam,
        twitch_id: twitch,
        url_slug: Some(name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()),
        ..Default::default()
    };
    std::fs::write(g.join("game.json"), serde_json::to_string_pretty(&meta)?)?;
    std::fs::write(
        g.join("assets").join("README.txt"),
        "Optional artwork used when installing into the original Powder app:\n\
         banner.(png|webp|jpg)  ~ 480x270   (library banner)\n\
         cover.(png|webp|jpg)   ~ 264x352   (game cover)\n\
         icon.(svg|png)         ~ 96x96     (small icon)\n\
         Missing images get a generated placeholder.\n",
    )?;
    Ok(g)
}

fn palette(i: usize) -> Rgb<u8> {
    const P: [[u8; 3]; 6] = [[255, 64, 64], [64, 200, 255], [255, 210, 0], [120, 255, 120], [255, 120, 255], [255, 150, 40]];
    Rgb(P[i % P.len()])
}

/// Draw every crop on the frame, save each crop with detected text boxes, and print OCR text.
pub fn calibrate(script: &GameScript, ocr: Option<&mut Ocr>, frame: &RgbImage, out: &Path) -> Result<Vec<(String, String)>> {
    std::fs::create_dir_all(out)?;
    let crops = script.ocr_crops()?;
    if crops.is_empty() {
        bail!("this game has no OCR crops (get_paddle_ocr_config / get_ocr_config)");
    }
    let mut overview = frame.clone();
    for (i, c) in crops.iter().enumerate() {
        let (x, y, w, h) = crop_rect(frame.width(), frame.height(), &c.coords);
        draw_rect(&mut overview, x, y, x + w - 1, y + h - 1, palette(i));
    }
    overview.save(out.join("_overview.png"))?;
    let mut res = Vec::new();
    let mut ocr = ocr;
    for c in &crops {
        let (text, img) = match ocr.as_deref_mut() {
            Some(o) => {
                let r = o.run_crop(frame, c)?;
                (r.text.clone(), crate::ocr::debug_image(frame, c, &r))
            }
            None => {
                let (x, y, w, h) = crop_rect(frame.width(), frame.height(), &c.coords);
                (String::new(), imageops::crop_imm(frame, x, y, w, h).to_image())
            }
        };
        img.save(out.join(format!("{}.png", c.name)))?;
        res.push((c.name.clone(), text));
    }
    Ok(res)
}

pub fn parse_crop(s: &str) -> Result<[f32; 4]> {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse::<f32>()).collect::<Result<_, _>>().context("crop must be l,t,r,b")?;
    if v.len() != 4 || v[0] >= v[2] || v[1] >= v[3] {
        bail!("crop must be 4 numbers left,top,right,bottom with left<right and top<bottom (fractions 0-1)");
    }
    Ok([v[0], v[1], v[2], v[3]])
}

pub fn adhoc_crop(name: &str, coords: [f32; 4], dilate: i32, min_area: i32, margin: i32, chars: &str) -> CropSpec {
    CropSpec {
        name: name.into(),
        coords,
        dilate_diameter: dilate,
        min_area,
        margin,
        stretch_vertical: false,
        restricted_chars: chars.into(),
        debug: false,
        legacy: false,
    }
}

#[derive(Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub info: Vec<String>,
}

/// Static checks that catch the mistakes that made earlier custom games fail silently.
pub fn validate(game: &GamePack, lua_libs: &[&Path]) -> Report {
    let mut r = Report::default();
    if !valid_id(&game.id) {
        r.warnings.push(format!("folder name '{}' is not a Powder-style id (A-Z, 0-9, _ ; max 12)", game.id));
    }
    match game.events["name"].as_str() {
        Some(n) if n == game.id => {}
        Some(n) => r.warnings.push(format!("events.json \"name\" is '{n}' but the folder id is '{}' (Powder keys games by folder id; keep them equal to avoid confusion)", game.id)),
        None => r.errors.push("events.json has no \"name\"".into()),
    }
    let names = game.event_names();
    if names.is_empty() {
        r.errors.push("events.json defines no events".into());
    }
    for e in game.events["events"].as_array().cloned().unwrap_or_default() {
        let n = e["name"].as_str().unwrap_or("?");
        for k in ["displayName", "icon", "automontage"] {
            if e.get(k).is_none() {
                r.warnings.push(format!("event '{n}' has no '{k}' (Powder's UI expects it)"));
            }
        }
    }
    let script = match GameScript::load(&game.dir, lua_libs) {
        Ok(s) => s,
        Err(e) => {
            r.errors.push(format!("Lua failed to load: {e}"));
            return r;
        }
    };
    match script.fps() {
        Ok(f) if (1.0..=30.0).contains(&f) => r.info.push(format!("fps: {f}")),
        Ok(f) => r.warnings.push(format!("get_fps() returns {f}; 2-8 is typical")),
        Err(e) => r.errors.push(e.to_string()),
    }
    match script.ocr_crops() {
        Ok(c) => {
            for c in &c {
                let [l, t, rr, b] = c.coords;
                if !(0.0..=1.0).contains(&l) || !(0.0..=1.0).contains(&rr) || !(0.0..=1.0).contains(&t) || !(0.0..=1.0).contains(&b) || l >= rr || t >= b {
                    r.errors.push(format!("crop {}: cropCoords {:?} must be fractions 0-1 as left,top,right,bottom", c.name, c.coords));
                }
                if c.min_area < 1 {
                    r.warnings.push(format!("crop {}: detectorMinimumArea < 1 lets every speck through", c.name));
                }
            }
            r.info.push(format!("OCR crops: {}", c.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ")));
        }
        Err(e) => r.errors.push(e.to_string()),
    }
    // Dry run of computeEvents with empty data catches runtime errors in setEventsSpecs.
    let crops = script.ocr_crops().unwrap_or_default();
    let mut paddle = std::collections::BTreeMap::new();
    let mut legacy = std::collections::BTreeMap::new();
    let n = 12;
    for c in &crops {
        if c.legacy { legacy.insert(c.name.clone(), (vec![String::new(); n], vec![0.0; n])); } else { paddle.insert(c.name.clone(), vec![String::new(); n]); }
    }
    let times: Vec<f64> = (0..n).map(|i| i as f64 * 0.25).collect();
    if let Err(e) = script.compute_events_dry(&legacy, &times, &paddle) {
        r.errors.push(format!("dry run of computeEvents failed: {e:#}"));
    }
    // Fuzz run: inject every quoted UPPERCASE phrase from the script into every crop, so detector
    // code paths that only run when text matches are exercised too (catches e.g. comparing
    // booleans with numbers, which crashes the whole analysis in Powder).
    if let Ok(src) = std::fs::read_to_string(game.dir.join("game_postprocess.lua")) {
        let words: Vec<String> = regex::Regex::new(r#"['"]([A-Z][A-Z0-9 +!]{1,30})['"]"#)
            .unwrap()
            .captures_iter(&src)
            .map(|c| c[1].to_string())
            .filter(|w| !crops.iter().any(|c| &c.name == w))
            .collect();
        if !words.is_empty() {
            let n = (words.len() * 6 + 20).min(4000);
            let times: Vec<f64> = (0..n).map(|i| i as f64 * 0.25).collect();
            let mut paddle = std::collections::BTreeMap::new();
            let mut legacy = std::collections::BTreeMap::new();
            for c in &crops {
                let seq: Vec<String> = (0..n).map(|i| if (i / 3) % 2 == 0 { words[(i / 6) % words.len()].clone() } else { String::new() }).collect();
                if c.legacy {
                    let conf = seq.iter().map(|s| if s.is_empty() { 0.0 } else { 0.9 }).collect();
                    legacy.insert(c.name.clone(), (seq.iter().map(|s| if s.is_empty() { s.clone() } else { format!("{s}\n") }).collect(), conf));
                } else {
                    paddle.insert(c.name.clone(), seq);
                }
            }
            if let Err(e) = script.compute_events_dry(&legacy, &times, &paddle) {
                r.errors.push(format!("computeEvents crashes when its own match words appear on screen: {e:#}"));
            }
        }
    }
    if let Ok(src) = std::fs::read_to_string(game.dir.join("game_postprocess.lua")) {
        let code: String = src.lines().filter(|l| !l.trim_start().starts_with("--")).collect::<Vec<_>>().join("\n");
        let uses_cues = regex::Regex::new(r"'[a-z_]+::[a-z_0-9]+'").unwrap().is_match(&code);
        if uses_cues && !game.dir.join("model.json").exists() {
            r.warnings.push("script uses visual cues ('head::class') but there is no open model.json/model.onnx; events that need them will never fire in powder-rs".into());
        }
    }
    // Lua event names vs events.json
    if let Ok(src) = std::fs::read_to_string(game.dir.join("game_postprocess.lua")) {
        let re = regex::Regex::new(r#"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*\{\s*name\s*=\s*"([^"]+)""#).unwrap();
        let mut specified: Vec<String> = re.captures_iter(&src).map(|c| c[2].to_string()).collect();
        if let Some(m) = regex::Regex::new(r"(?s)local EVENT_MERGE = \{([^}]*)\}").unwrap().captures(&src) {
            let k = regex::Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*[0-9.]").unwrap();
            specified.extend(k.captures_iter(&m[1]).map(|c| c[1].to_string()));
        }
        for ev in specified {
            if !names.contains(&ev) {
                r.warnings.push(format!("eventsSpecs has '{ev}' but events.json does not define it (Powder will drop that event)"));
            }
        }
    }
    if game.dir.join("model.pwdmdl").exists() && !game.dir.join("model.json").exists() {
        r.warnings.push("model.pwdmdl is encrypted Powder data; powder-rs only uses model.json + model.onnx".into());
    }
    r
}

/// Dump frames (optionally a crop) for labelling / training.
pub fn extract_frames(ffmpeg: &Path, video: &Path, out: &Path, fps: f64, crop: Option<[f32; 4]>, size: Option<(u32, u32)>, start: Option<f64>, end: Option<f64>) -> Result<usize> {
    std::fs::create_dir_all(out)?;
    let info = crate::video::probe(ffmpeg, video)?;
    let mut r = crate::video::FrameReader::open(ffmpeg, video, &info, &crate::video::SampleOptions { fps, start, end, max_height: None })?;
    let stem = video.file_stem().and_then(|s| s.to_str()).unwrap_or("video").to_string();
    let mut n = 0;
    while let Some((t, f)) = r.next_frame()? {
        let mut img = match crop {
            Some(c) => {
                let (x, y, w, h) = crop_rect(f.width(), f.height(), &c);
                imageops::crop_imm(&f, x, y, w, h).to_image()
            }
            None => f,
        };
        if let Some((w, h)) = size {
            img = imageops::resize(&img, w, h, imageops::FilterType::Triangle);
        }
        img.save(out.join(format!("{stem}_{:08.2}.jpg", t)))?;
        n += 1;
    }
    Ok(n)
}
