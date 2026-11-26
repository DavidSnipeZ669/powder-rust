//! Small features: application, window, login (Powder's own offline stub), flags, store, updater
//! (disabled - the service is gone), onboarding, splash, announcements, analytics/logger/error (local only).

use crate::host::Host;
use crate::server::Ctx;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;

fn read_json(p: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn write_json(p: &std::path::Path, v: &Value) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(v)?)?;
    Ok(())
}

pub fn open_path(p: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(p).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    }
}

pub fn reveal(p: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{p}")).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(std::path::Path::new(p).parent().unwrap_or(std::path::Path::new("."))).spawn();
    }
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    let _ = ctx;
    // login: Powder 8.0.2 already ships an offline stub user
    let auth = json!({
        "user": { "id": "a682ee57-66cb-4155-a48a-6ff6d8c56d8b", "username": "username", "email": "user@email.gg", "isNew": false },
        "email": "user@email.gg", "groups": ["basic", "pro"]
    });
    h.set("login", "authorization", auth.clone());
    h.set("login", "user", auth["user"].clone());
    let a2 = auth.clone();
    h.handle("login", "refreshToken", move |_, _| Ok(a2.clone()));

    h.set("application", "isOnline", json!(true));
    h.handle("application", "openExternal", |_, a| {
        if let Some(u) = a.first().and_then(|v| v.as_str()) {
            if u.starts_with("http") {
                open_path(u);
            }
        }
        Ok(Value::Null)
    });
    h.handle("application", "revealUserDataFolder", |_, _| {
        open_path(&super::user_data().to_string_lossy());
        Ok(Value::Null)
    });

    h.set("flags", "flags", json!({}));
    h.set("store", "store", Value::Null);
    npu_update();
    std::thread::spawn(|| {
        let has = crate::gpu::enumerate().map(|v| v["hasUsableNpu"] == true).unwrap_or(false);
        HAS_NPU.store(has, std::sync::atomic::Ordering::SeqCst);
        npu_update();
    });

    // updater: Powder's update servers are gone
    h.set("updater", "enabled", json!(false));
    h.set("updater", "currentVersion", json!("8.0.2"));
    h.set("updater", "status", json!("UPDATE_NOT_AVAILABLE"));
    h.handle("updater", "checkForUpdates", |_, _| Ok(Value::Null));
    h.handle("updater", "quitAndInstall", |_, _| Ok(Value::Null));

    // onboarding steps (persisted like the original's storage)
    let ob = super::user_data().join("features").join("onboarding.json");
    let steps = read_json(&ob).unwrap_or(json!({
        "videoAnalyzerStarted": false, "clipBookmarked": false, "clipVerticalizedSaved": false,
        "videoEditorProjectCreated": false, "videoEditorProjectExported": false
    }));
    h.set("onboarding", "steps", steps);
    h.handle("onboarding", "setOnboardingSteps", move |h, a| {
        let mut cur = h.get("onboarding", "steps").unwrap_or(json!({}));
        if let (Some(c), Some(Value::Object(n))) = (cur.as_object_mut(), a.first()) {
            for (k, v) in n {
                c.insert(k.clone(), v.clone());
            }
        }
        write_json(&ob, &cur)?;
        h.set("onboarding", "steps", cur.clone());
        Ok(cur)
    });

    // announcements: no remote messages any more; keep local "shown" stats
    let an = super::user_data().join("features").join("announcements.json");
    h.handle("announcements", "readStats", {
        let an = an.clone();
        move |_, _| Ok(read_json(&an).unwrap_or(json!({})))
    });
    h.handle("announcements", "writeAsShown", {
        let an = an.clone();
        move |_, a| {
            let mut s = read_json(&an).unwrap_or(json!({}));
            if let Some(k) = a.first().and_then(|v| v.as_str()) {
                let e = &mut s[k];
                let n = e["count"].as_u64().unwrap_or(0) + 1;
                *e = json!({"count": n, "lastShownAt": now_iso()});
            }
            write_json(&an, &s)?;
            Ok(Value::Null)
        }
    });
    for f in ["shouldShowOnce", "shouldShowOncePerDay", "shouldShowOncePerDayNTimes", "getIfAnnouncementIsValidated"] {
        h.handle("announcements", f, |_, _| Ok(json!(false)));
    }
    h.handle("announcements", "validateAnnouncement", |_, _| Ok(Value::Null));

    // analytics went to Amplitude - dropped. Logs go to the local log file.
    h.handle("analytics", "track", |_, _| Ok(Value::Null));
    h.handle("logger", "log", |_, a| {
        crate::log(&format!("[renderer] {}", a.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ")));
        Ok(Value::Null)
    });
    h.handle("logger", "revealLogs", |_, _| {
        reveal(&crate::log_path().to_string_lossy());
        Ok(Value::Null)
    });
    h.handle("error", "forwardError", |_, a| {
        crate::log(&format!("[renderer error] {}", a.first().map(|v| v.to_string()).unwrap_or_default()));
        Ok(Value::Null)
    });

    // window
    for (f, cmd) in [("minimize", "minimize"), ("maximize", "toggle-maximize"), ("show", "show"), ("hide", "hide"), ("destroy", "close")] {
        h.handle("window", f, move |h, _| {
            h.window.send(cmd);
            Ok(Value::Null)
        });
    }
    h.handle("window", "showItemInFolder", |_, a| {
        if let Some(p) = a.first().and_then(|v| v.as_str()) {
            reveal(p);
        }
        Ok(Value::Null)
    });
    h.handle("window", "openPath", |_, a| {
        if let Some(p) = a.first().and_then(|v| v.as_str()) {
            open_path(p);
        }
        Ok(json!(""))
    });
    for (f, kind) in [
        ("openFolderPicker", "folder"),
        ("openVideoPicker", "video"),
        ("openVideosPicker", "videos"),
        ("openMusicPicker", "music"),
        ("openImagesPicker", "images"),
    ] {
        h.handle("window", f, move |_, _| Ok(crate::dialogs::pick(kind)));
    }
    h.handle("window", "showOpenDialog", |_, a| Ok(crate::dialogs::open_dialog(a.first().cloned().unwrap_or(Value::Null))));
    h.handle("window", "showSaveDialog", |_, a| Ok(crate::dialogs::save_dialog(a.first().cloned().unwrap_or(Value::Null))));
    h.handle("window", "stopRecordingAndCloseApp", |h, _| {
        h.window.send("quit");
        Ok(Value::Null)
    });
    h.handle("window", "closeAfterRecordingSave", |h, _| {
        h.window.send("quit");
        Ok(Value::Null)
    });

    h.handle("splash-screen-window", "close", |_, _| Ok(Value::Null));
    h.handle("block-sleep-mode", "preventSleepMode", |_, a| {
        crate::gpu::block_sleep(&format!("renderer-{}", a.first().map(|v| v.to_string()).unwrap_or_default()), true);
        Ok(Value::Null)
    });
    h.handle("block-sleep-mode", "stopPreventSleepMode", |_, a| {
        crate::gpu::block_sleep(&format!("renderer-{}", a.first().map(|v| v.to_string()).unwrap_or_default()), false);
        Ok(Value::Null)
    });
    h.handle("shortcuts", "playShortcutHandlerListener", |_, _| {
        crate::hotkeys::set_active(true);
        Ok(Value::Null)
    });
    h.handle("shortcuts", "pauseShortcutHandlerListener", |_, _| {
        crate::hotkeys::set_active(false);
        Ok(Value::Null)
    });
    h.handle("clipboard", "copy", |_, a| {
        crate::dialogs::clipboard_set(a.first().and_then(|v| v.as_str()).unwrap_or(""));
        Ok(Value::Null)
    });
    h.handle("clipboard", "read", |_, _| Ok(json!(crate::dialogs::clipboard_get())));
}

pub fn now_iso() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let (d, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil from days (Howard Hinnant)
    let z = d + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}.000Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

static HAS_NPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// npu feature: the original only allowed Live highlights with an AMD NPU unless the hidden
/// "allowLiveOnCpu" setting was on. Here live analysis is always allowed (runs on the GPU/CPU without an NPU).
pub fn npu_update() {
    let _ = HAS_NPU.load(std::sync::atomic::Ordering::SeqCst);
    let ready = true;
    super::ctx().host.set("npu", "npuStatus", json!(if ready { "READY" } else { "NO_USABLE_NPU" }));
}
