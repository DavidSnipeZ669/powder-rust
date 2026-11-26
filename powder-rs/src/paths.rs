//! Locating external resources: ffmpeg, onnxruntime, OCR models, Lua library, game packs.
//!
//! Search order for every resource: explicit CLI flag -> environment variable ->
//! next to the executable -> a few well-known relative folders -> PATH (ffmpeg only).

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Candidate base folders: the exe folder, its parents (useful for `target/release`), and cwd.
pub fn base_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut d = Some(exe_dir());
    for _ in 0..4 {
        if let Some(p) = d {
            v.push(p.clone());
            d = p.parent().map(|x| x.to_path_buf());
        }
    }
    if let Ok(c) = std::env::current_dir() {
        v.push(c);
    }
    v
}

fn first_existing(cands: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    cands.into_iter().find(|p| p.exists())
}

pub fn find_resource(flag: Option<&Path>, env: &str, rel: &[&str]) -> Option<PathBuf> {
    if let Some(f) = flag {
        return Some(f.to_path_buf());
    }
    if let Ok(e) = std::env::var(env) {
        if !e.is_empty() {
            return Some(PathBuf::from(e));
        }
    }
    let mut c = Vec::new();
    for b in base_dirs() {
        for r in rel {
            c.push(b.join(r));
        }
    }
    first_existing(c)
}

pub fn ffmpeg(flag: Option<&Path>) -> Result<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Some(p) = find_resource(flag, "POWDER_RS_FFMPEG", &[name, &format!("runtime/{name}"), &format!("bin/{name}")]) {
        return Ok(p);
    }
    // reuse the ffmpeg.exe that ships with Powder
    let mut powder = Vec::new();
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        powder.push(Path::new(&l).join("Programs").join("powder-desktop").join(name));
    }
    if let Ok(p) = std::env::var("POWDER_DIR") {
        powder.push(Path::new(&p).join(name));
    }
    if let Some(p) = powder.into_iter().find(|p| p.exists()) {
        return Ok(p);
    }
    if let Ok(path) = std::env::var("PATH") {
        for d in std::env::split_paths(&path) {
            let p = d.join(name);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    bail!("ffmpeg not found. Put {name} next to powder-rs (Powder ships one), set POWDER_RS_FFMPEG, or pass --ffmpeg")
}

pub fn onnxruntime(flag: Option<&Path>) -> Result<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["onnxruntime.dll", "runtime/onnxruntime.dll"]
    } else if cfg!(target_os = "macos") {
        &["libonnxruntime.dylib", "runtime/libonnxruntime.dylib"]
    } else {
        &["libonnxruntime.so", "runtime/libonnxruntime.so"]
    };
    find_resource(flag, "ORT_DYLIB_PATH", names).ok_or_else(|| {
        anyhow::anyhow!("onnxruntime library not found. Run setup (see README), set ORT_DYLIB_PATH, or pass --onnxruntime")
    })
}

pub fn ocr_models_dir(flag: Option<&Path>) -> Result<PathBuf> {
    find_resource(flag, "POWDER_RS_OCR_MODELS", &["models/ocr", "../models/ocr"])
        .ok_or_else(|| anyhow::anyhow!("OCR models folder (models/ocr) not found; pass --ocr-models"))
}

pub fn lua_lib_dir(flag: Option<&Path>) -> Result<PathBuf> {
    find_resource(flag, "POWDER_RS_LUA", &["lua", "../lua"])
        .ok_or_else(|| anyhow::anyhow!("Lua library folder (lua/postprocess) not found; pass --lua-lib"))
}

/// Default game folders: `games/` next to the exe (or repo root).
pub fn default_game_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    for b in base_dirs() {
        let g = b.join("games");
        if g.is_dir() && !v.contains(&g) {
            v.push(g);
        }
    }
    v
}
