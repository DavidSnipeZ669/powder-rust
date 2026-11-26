//! Port of src/main/features/settings: same file (%LOCALAPPDATA%\Powder\settings.json), same defaults.
use super::core::write_json;
use crate::host::Host;
use crate::server::Ctx;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub fn path() -> PathBuf {
    super::app_data().join("settings.json")
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn defaults() -> Value {
    let videos = dirs::video_dir().unwrap_or_else(|| home().join("Videos"));
    let downloads = dirs::download_dir().unwrap_or_else(|| home().join("Downloads")).join("Powder");
    json!({
        "deviceId": crate::server::random_token()[..32].to_string(),
        "installedAt": super::core::now_iso(),
        "tutorialShown": false,
        "iAmADeveloper": false,
        "runOnStartup": true,
        "allowNvidiaAcceleration": true,
        "allowIntelAcceleration": true,
        "allowAmdAcceleration": true,
        "downloadFolder": downloads,
        "libraryFolder": videos.join("Powder"),
        "montagesFolder": videos.join("Powder Montages"),
        "autoMontagesFolder": videos.join("Powder Auto Montages"),
        "dumpInstructions": false,
        "allowLiveOnCpu": false,
        "dumpMinipowData": false,
        "recorderFolder": videos.join("Powder Recordings"),
        "recorderScreenDevice": 0,
        "recorderAudioInputEnabled": true,
        "recorderAudioInputDevice": "CAPTURE default",
        "recorderAudioForegroundEnabled": true,
        "recorderAudioDiscordEnabled": false,
        "recorderSizeHeight": -1,
        "recorderFramerate": 60,
        "recorderQuality": "LOW",
        "shortcuts": {
            "TOGGLE_RECORD_SESSION": { "registered": false, "shortcut": "Control+F8" },
            "MANUAL_BOOKMARK": { "registered": false, "shortcut": "Control+F9" }
        },
        "allowRecorder": false,
        "theme": "dark"
    })
}

/// readSettings(): defaults overlaid with the known keys from the file.
pub fn read() -> Value {
    let mut s = defaults();
    if let Ok(t) = std::fs::read_to_string(path()) {
        if let Ok(Value::Object(file)) = serde_json::from_str::<Value>(&t) {
            let keys: Vec<String> = s.as_object().unwrap().keys().cloned().chain(["clipsFolder".to_string()]).collect();
            for k in keys {
                if let Some(v) = file.get(&k) {
                    if k == "shortcuts" {
                        if let Value::Object(m) = v {
                            for (sk, sv) in m {
                                s["shortcuts"][sk] = sv.clone();
                            }
                        }
                    } else {
                        s[&k] = v.clone();
                    }
                }
            }
        }
    }
    s
}

pub fn update(partial: Value) -> anyhow::Result<Value> {
    let mut s = read();
    if let Value::Object(m) = partial {
        for (k, v) in m {
            s[&k] = v;
        }
    }
    write_json(&path(), &s)?;
    Ok(s)
}

#[allow(dead_code)] // kept for upcoming features
pub fn get(key: &str) -> Value {
    read()[key].clone()
}

const VARS: &[&str] = &[
    "runOnStartup", "allowNvidiaAcceleration", "allowIntelAcceleration", "allowAmdAcceleration", "downloadFolder",
    "autoMontagesFolder", "recorderFolder", "recorderScreenDevice", "recorderAudioInputEnabled", "recorderAudioInputDevice",
    "recorderAudioDiscordEnabled", "recorderAudioForegroundEnabled", "recorderSizeHeight", "recorderFramerate",
    "recorderQuality", "installedAt", "allowRecorder", "clipsFolder", "theme", "allowLiveOnCpu", "dumpMinipowData", "dumpInstructions",
];

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    let s = read();
    let _ = write_json(&path(), &s); // ensureSettings
    for k in VARS {
        if *k == "clipsFolder" && s.get("clipsFolder").is_none() {
            continue; // undefined in the original
        }
        h.set("settings", k, s[*k].clone());
    }
    h.set("settings", "shortcuts", s["shortcuts"].clone());
    let (radeon, afmf) = crate::gpu::radeon();
    h.set("settings", "hasRadeonGpu", json!(radeon));
    h.set("settings", "hasAfmfCompliantGpu", json!(afmf));
    h.set("settings", "hasAdrenalinInstalled", json!(crate::gpu::adrenalin_path().is_some()));

    // simple setters: setX(value) -> settings.x = value
    for (f, key, folder) in [
        ("setRunOnStartup", "runOnStartup", false),
        ("setAllowNvidiaAcceleration", "allowNvidiaAcceleration", false),
        ("setAllowIntelAcceleration", "allowIntelAcceleration", false),
        ("setAllowAmdAcceleration", "allowAmdAcceleration", false),
        ("setDownloadFolder", "downloadFolder", true),
        ("setAutoMontagesFolder", "autoMontagesFolder", true),
        ("setDumpInstructions", "dumpInstructions", false),
        ("setAllowLiveOnCpu", "allowLiveOnCpu", false),
        ("setDumpMinipowData", "dumpMinipowData", false),
        ("setRecorderFolder", "recorderFolder", true),
        ("setRecorderScreenDevice", "recorderScreenDevice", false),
        ("setRecorderAudioInputEnabled", "recorderAudioInputEnabled", false),
        ("setRecorderAudioInputDevice", "recorderAudioInputDevice", false),
        ("setRecorderAudioDiscordEnabled", "recorderAudioDiscordEnabled", false),
        ("setRecorderAudioForegroundEnabled", "recorderAudioForegroundEnabled", false),
        ("setRecorderSizeHeight", "recorderSizeHeight", false),
        ("setRecorderFramerate", "recorderFramerate", false),
        ("setRecorderQuality", "recorderQuality", false),
        ("setAllowRecorder", "allowRecorder", false),
        ("setTheme", "theme", false),
    ] {
        h.handle("settings", f, move |h, a| {
            let v = a.into_iter().next().unwrap_or(Value::Null);
            if folder {
                // the original refuses folders it cannot write to
                let p = PathBuf::from(v.as_str().unwrap_or(""));
                if std::fs::create_dir_all(&p).is_err() || std::fs::write(p.join(".powder-write-test"), b"").is_err() {
                    return Ok(json!(false));
                }
                let _ = std::fs::remove_file(p.join(".powder-write-test"));
            }
            update(json!({ key: v.clone() }))?;
            if key == "allowLiveOnCpu" {
                h.set("settings", key, v.clone());
                super::core::npu_update();
            }
            if key == "runOnStartup" {
                crate::gpu::set_run_on_startup(v.as_bool().unwrap_or(false));
            }
            h.set("settings", key, v);
            Ok(if folder { json!(true) } else { Value::Null })
        });
    }
    h.handle("settings", "setShortcutByType", |h, a| {
        let shortcut = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ty = a.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut sc = read()["shortcuts"].clone();
        sc[&ty] = json!({ "registered": true, "shortcut": shortcut });
        update(json!({ "shortcuts": sc.clone() }))?;
        for (t, ok) in crate::hotkeys::reload() {
            sc[&t]["registered"] = json!(ok);
        }
        h.set("settings", "shortcuts", sc);
        Ok(Value::Null)
    });

    h.handle("settings", "getRecorderScreenDeviceList", |_, _| {
        crate::gpu::enumerate();
        Ok(Value::Array(crate::gpu::screens().iter().map(|s| json!({"label": s.name, "value": s.index})).collect()))
    });
    h.handle("settings", "getRecorderAudioInputDeviceList", |_, _| {
        let real = crate::gpu::enumerate().is_some();
        let mut v = if real { vec![] } else { vec![json!({"label": "Default", "value": "CAPTURE default"})] };
        v.extend(crate::gpu::audio_inputs().into_iter().map(|(name, id)| json!({"label": name, "value": id})));
        Ok(Value::Array(v))
    });
    h.handle("settings", "getRecorderSizeHeightList", |h, _| {
        crate::gpu::enumerate();
        let dev = h.get("settings", "recorderScreenDevice").and_then(|v| v.as_i64()).unwrap_or(0);
        let height = crate::gpu::screens().iter().find(|s| s.index == dev).map(|s| s.height).unwrap_or(1080);
        Ok(json!([
            {"label": "2160p 4K", "value": 2160}, {"label": "1440p QHD", "value": 1440},
            {"label": "1080p FHD", "value": 1080}, {"label": "720p HD", "value": 720},
            {"label": format!("Native ({height}p)"), "value": -1}
        ]))
    });
    h.handle("settings", "getRecorderFramerateList", |_, _| Ok(json!([{"label": "120 fps", "value": 120}, {"label": "60 fps", "value": 60}, {"label": "30 fps", "value": 30}])));
    h.handle("settings", "getRecorderQualityList", |_, _| Ok(json!([{"label": "High", "value": "HIGH"}, {"label": "Medium", "value": "MEDIUM"}, {"label": "Low", "value": "LOW"}])));
    h.handle("settings", "launchAdrenalin", |_, _| {
        if let Some(p) = crate::gpu::adrenalin_path() {
            let _ = std::process::Command::new(p).spawn();
        }
        Ok(Value::Null)
    });
}
