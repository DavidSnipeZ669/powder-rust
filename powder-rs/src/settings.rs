//! App settings (folders, recorder, hotkeys, montage defaults). Stored as JSON in the user data
//! folder; on first run the folders/recorder options are imported from Powder's own settings.json.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// folders scanned for videos in the Library
    pub library_folders: Vec<PathBuf>,
    /// where new recordings are written
    pub recordings_folder: PathBuf,
    /// where clips are exported
    pub clips_folder: PathBuf,
    /// where montages are written
    pub montages_folder: PathBuf,
    /// extra music folders for montages
    pub music_folders: Vec<PathBuf>,
    pub recorder: RecorderSettings,
    /// analyse recordings automatically when they stop
    pub auto_analyze: bool,
    /// default analysis rate ("default" = game's own, a number, or "native")
    pub analysis_fps: String,
    pub powder_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecorderSettings {
    /// output height (e.g. 1080, 1440, 2160) - 0 = native
    pub height: u32,
    pub fps: u32,
    /// "high" | "medium" | "low" (bitrate presets)
    pub quality: String,
    /// encoder name or "auto"
    pub encoder: String,
    /// monitor index for screen capture
    pub monitor: u32,
    pub capture_system_audio: bool,
    pub capture_microphone: bool,
    /// microphone device name (dshow/wasapi) or "default"
    pub microphone: String,
    /// seconds kept in memory/disk for instant replay (0 = off)
    pub replay_seconds: u32,
    pub hotkey_toggle_record: String,
    pub hotkey_bookmark: String,
    pub hotkey_save_replay: String,
    /// start the replay buffer when the app starts
    pub replay_autostart: bool,
    /// shift audio relative to video (ms, + = audio later)
    pub audio_sync_ms: i64,
}

impl Default for RecorderSettings {
    fn default() -> Self {
        Self {
            height: 1080,
            fps: 60,
            quality: "high".into(),
            encoder: "auto".into(),
            monitor: 0,
            capture_system_audio: true,
            capture_microphone: false,
            microphone: "default".into(),
            replay_seconds: 60,
            hotkey_toggle_record: "Ctrl+F8".into(),
            hotkey_bookmark: "Insert".into(),
            hotkey_save_replay: "Ctrl+F9".into(),
            replay_autostart: false,
            audio_sync_ms: 0,
        }
    }
}

fn videos_dir() -> PathBuf {
    dirs::video_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Videos"))
}

impl Default for Settings {
    fn default() -> Self {
        let v = videos_dir();
        Self {
            library_folders: vec![v.join("Powder"), v.join("Powder Recordings")],
            recordings_folder: v.join("Powder Recordings"),
            clips_folder: v.join("Powder Clips"),
            montages_folder: v.join("Powder Montages"),
            music_folders: vec![],
            recorder: RecorderSettings::default(),
            auto_analyze: true,
            analysis_fps: "default".into(),
            powder_dir: None,
        }
    }
}

pub fn data_dir() -> PathBuf {
    let d = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("powder-rs");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn path() -> PathBuf {
    data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        if let Ok(s) = std::fs::read_to_string(path()) {
            if let Ok(v) = serde_json::from_str(&s) {
                return v;
            }
        }
        let mut s = Settings::default();
        s.import_from_powder();
        let _ = s.save();
        s
    }

    pub fn save(&self) -> Result<()> {
        std::fs::write(path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Import folders/recorder options from %LOCALAPPDATA%\Powder\settings.json if present.
    pub fn import_from_powder(&mut self) {
        let Some(local) = dirs::data_local_dir() else { return };
        let p = local.join("Powder").join("settings.json");
        let Ok(txt) = std::fs::read_to_string(&p) else { return };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return };
        let pathv = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(PathBuf::from);
        let mut lib = Vec::new();
        for k in ["libraryFolder", "recorderFolder", "downloadFolder"] {
            if let Some(p) = pathv(k) {
                if !lib.contains(&p) {
                    lib.push(p);
                }
            }
        }
        if !lib.is_empty() {
            self.library_folders = lib;
        }
        if let Some(p) = pathv("recorderFolder") { self.recordings_folder = p; }
        if let Some(p) = pathv("montagesFolder") { self.clips_folder = p; }
        if let Some(p) = pathv("autoMontagesFolder") { self.montages_folder = p; }
        if let Some(h) = v["recorderSizeHeight"].as_u64() { self.recorder.height = h as u32; }
        if let Some(f) = v["recorderFramerate"].as_u64() { self.recorder.fps = f as u32; }
        if let Some(q) = v["recorderQuality"].as_str() { self.recorder.quality = q.to_lowercase(); }
        if let Some(b) = v["recorderAudioInputEnabled"].as_bool() { self.recorder.capture_microphone = b; }
        if let Some(s) = v["shortcuts"]["TOGGLE_RECORD_SESSION"]["shortcut"].as_str() { self.recorder.hotkey_toggle_record = s.into(); }
        if let Some(s) = v["shortcuts"]["MANUAL_BOOKMARK"]["shortcut"].as_str() { self.recorder.hotkey_bookmark = s.into(); }
    }
}
