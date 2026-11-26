//! Finds Powder's original files (interface, preload, native engine) in a Powder install.
//! Accepts an installed Powder (resources/app.asar), an extracted one (resources/app-unpacked) or a folder
//! containing `dist/`.

use crate::asar::Asar;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub enum Source {
    Folder(PathBuf), // folder that contains dist/
    Asar(Asar),
}

pub struct Dist {
    pub source: Source,
    /// Folder with PowderRecorderCli.exe, ffmpeg.exe, whisper.dll ... (app.asar.unpacked/dist or <folder>/dist)
    pub native: PathBuf,
    /// The Powder install root (contains ai-configs/ in the standalone build)
    pub root: PathBuf,
    pub label: String,
}

impl Dist {
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        let rel = rel.trim_start_matches('/');
        match &self.source {
            Source::Folder(p) => std::fs::read(p.join(rel)).ok(),
            Source::Asar(a) => a.read(rel).ok().or_else(|| std::fs::read(self.native.parent()?.join(rel)).ok()),
        }
    }

    fn try_resources(res: &Path) -> Option<Dist> {
        let root = res.parent()?.to_path_buf();
        let asar = res.join("app.asar");
        let unpacked = res.join("app.asar.unpacked").join("dist");
        if asar.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
            if let Ok(a) = Asar::open(&asar) {
                return Some(Dist { source: Source::Asar(a), native: unpacked, root, label: asar.display().to_string() });
            }
        }
        let folder = res.join("app-unpacked");
        if folder.join("dist").join("renderer").join("index.html").exists() {
            let native = if folder.join("dist").join("PowderRecorderCli.exe").exists() { folder.join("dist") } else { unpacked };
            return Some(Dist { source: Source::Folder(folder.clone()), native, root, label: folder.display().to_string() });
        }
        None
    }

    pub fn locate(explicit: Option<&Path>) -> Result<Dist> {
        let mut cands: Vec<PathBuf> = Vec::new();
        if let Some(p) = explicit {
            cands.push(p.to_path_buf());
        }
        if let Ok(p) = std::env::var("POWDER_DIR") {
            cands.push(p.into());
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                cands.push(d.to_path_buf());
                if let Ok(cfg) = std::fs::read_to_string(d.join("powder-app.json")) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cfg) {
                        if let Some(p) = v["powderDir"].as_str() {
                            cands.insert(0, p.into());
                        }
                    }
                }
            }
        }
        if let Some(l) = dirs::data_local_dir() {
            cands.push(l.join("Programs").join("powder-desktop"));
            cands.push(l.join("Programs").join("Powder"));
        }
        for c in &cands {
            // a Powder install folder, its resources/ folder, or a folder that has dist/ directly
            for res in [c.join("resources"), c.clone()] {
                if let Some(d) = Self::try_resources(&res) {
                    return Ok(d);
                }
            }
            if c.join("dist").join("renderer").join("index.html").exists() {
                return Ok(Dist { source: Source::Folder(c.clone()), native: c.join("dist"), root: c.clone(), label: c.display().to_string() });
            }
        }
        bail!(
            "Rewind's app files were not found (resources\\app-unpacked next to Rewind.exe). Run build-app.bat again to copy them in. Looked in: {}",
            cands.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("; ")
        )
    }
}
