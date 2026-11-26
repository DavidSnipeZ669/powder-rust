//! Game packs. A game is just a folder:
//!
//!   games/<GAME_ID>/
//!     game.json             metadata (name, store ids...) - used by the Powder patcher and UI
//!     events.json           Powder-format event definitions (names, icons, scores, automontage)
//!     game_postprocess.lua  detection logic (Powder-compatible API)
//!     model.json/.onnx      optional open visual-cue model
//!     assets/               optional banner / cover / icon images
//!
//! A Powder install's `ai-configs/visual_cues` folder is also a valid games folder.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GameMeta {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steam_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twitch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_game_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_slug: Option<String>,
    #[serde(default)]
    pub free2play: bool,
    /// Resolution the crops were calibrated on (informational)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibrated_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    /// executable names used to detect that the game is running (e.g. ["DeltaForceClient-Win64-Shipping.exe"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GamePack {
    pub id: String,
    pub dir: PathBuf,
    pub meta: GameMeta,
    pub events: Value,
}

impl GamePack {
    pub fn load(dir: &Path) -> Result<Self> {
        let id = dir.file_name().and_then(|s| s.to_str()).context("bad game folder")?.to_string();
        if !dir.join("game_postprocess.lua").exists() {
            bail!("{} has no game_postprocess.lua", dir.display());
        }
        let ev_path = dir.join("events.json");
        let events: Value = if ev_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&ev_path)?)
                .with_context(|| format!("{} is not valid JSON (check for a trailing comma or missing quote)", ev_path.display()))?
        } else {
            serde_json::json!({ "name": id, "events": [] })
        };
        let meta_path = dir.join("game.json");
        let meta = if meta_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&meta_path)?).with_context(|| format!("parsing {}", meta_path.display()))?
        } else {
            GameMeta { name: id.clone(), ..Default::default() }
        };
        Ok(Self { id, dir: dir.to_path_buf(), meta, events })
    }

    pub fn event_names(&self) -> Vec<String> {
        self.events["events"]
            .as_array()
            .map(|a| a.iter().filter_map(|e| e["name"].as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    pub fn display_name(&self, event: &str) -> String {
        self.events["events"]
            .as_array()
            .and_then(|a| a.iter().find(|e| e["name"] == event))
            .and_then(|e| e["displayName"].as_str())
            .unwrap_or(event)
            .to_string()
    }
}

pub fn find_game(id: &str, dirs: &[PathBuf]) -> Result<GamePack> {
    let p = Path::new(id);
    if p.is_dir() {
        return GamePack::load(p);
    }
    for d in dirs {
        let g = d.join(id);
        if g.is_dir() {
            return GamePack::load(&g);
        }
        // case-insensitive match
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().eq_ignore_ascii_case(id) && e.path().is_dir() {
                    return GamePack::load(&e.path());
                }
            }
        }
    }
    bail!("game '{id}' not found in {:?}. Use --games <folder> or pass a game folder path.", dirs)
}

pub fn list_games(dirs: &[PathBuf]) -> Vec<GamePack> {
    let mut v = Vec::new();
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.path().join("game_postprocess.lua").exists() {
                    if let Ok(g) = GamePack::load(&e.path()) {
                        v.push(g);
                    }
                }
            }
        }
    }
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}
