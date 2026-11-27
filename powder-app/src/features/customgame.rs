//! "Add Custom Game Support" inside Powder: runs the powder-rs engine (engine\powder-rs.exe) on a
//! gameplay video + title/Steam link, which builds the game (text detection, events, artwork) and
//! installs it where Powder's analyzer and this app read games. The game list refreshes when done.
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Default)]
struct Status {
    running: bool,
    log: Vec<String>,
    done: bool,
    error: Option<String>,
    game_id: Option<String>,
}

fn status() -> &'static Mutex<Status> {
    static S: OnceLock<Mutex<Status>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Status::default()))
}

pub fn engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let name = if cfg!(windows) { "powder-rs.exe" } else { "powder-rs" };
    [crate::setup::engine_dir().join(name), dir.join("engine").join(name), dir.join(name), dir.join("..").join("dist").join(name), dir.join("..").join("..").join("..").join("powder-rs").join("target").join("release").join(name)]
        .into_iter()
        .find(|p| p.exists())
}

fn push(line: &str) {
    let mut s = status().lock();
    s.log.push(line.to_string());
    if s.log.len() > 400 {
        s.log.remove(0);
    }
}

pub fn start(title: &str, video: &str, add_to: Option<&str>) -> Value {
    {
        let mut s = status().lock();
        if s.running {
            return json!({"error": "already running"});
        }
        *s = Status { running: true, ..Default::default() };
    }
    let Some(eng) = engine() else {
        let mut s = status().lock();
        s.running = false;
        s.done = true;
        s.error = Some("The game engine (engine\\powder-rs.exe) is missing next to Rewind.exe - run build-app.bat again.".into());
        return json!({"ok": false});
    };
    let ctx = super::ctx();
    let root = ctx.dist.root.clone();
    let ffmpeg = crate::media::ffmpeg();
    let t = title.trim().to_string();
    let v = video.to_string();
    let add_to = add_to.filter(|s| !s.is_empty()).map(String::from);
    std::thread::spawn(move || {
        let mut c = Command::new(&eng);
        c.current_dir(eng.parent().unwrap()).arg("add-game").arg(&v).arg("--powder").arg(&root);
        if let Some(a) = &add_to {
            c.arg("--add-to").arg(a);
            status().lock().game_id = Some(a.clone());
        } else if t.contains("store.steampowered.com") || t.chars().all(|ch| ch.is_ascii_digit()) && !t.is_empty() {
            c.arg("--steam").arg(&t);
        } else {
            c.arg("--title").arg(&t);
        }
        if let Some(f) = &ffmpeg {
            c.arg("--ffmpeg").arg(f);
        }
        c.arg("--overwrite").stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
        push(&format!("Adding \"{t}\" from {}", std::path::Path::new(&v).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()));
        let r = c.spawn().and_then(|mut child| {
            let out = child.stdout.take().unwrap();
            let err = child.stderr.take().unwrap();
            let h = std::thread::spawn(move || {
                for l in BufReader::new(err).lines().map_while(Result::ok) {
                    let l = l.trim().to_string();
                    if !l.is_empty() {
                        push(&l);
                    }
                }
            });
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let l = l.trim_end().to_string();
                if l.is_empty() {
                    continue;
                }
                // "created game pack XYZ" / "game id: XYZ"
                for key in ["game id: ", "created game ", "installing "] {
                    if let Some(rest) = l.split(key).nth(1) {
                        if let Some(id) = rest.split_whitespace().next() {
                            if id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && id.len() >= 2 {
                                status().lock().game_id = Some(id.to_string());
                            }
                        }
                    }
                }
                push(&l);
            }
            let _ = h.join();
            child.wait()
        });
        let ok = matches!(&r, Ok(s) if s.success());
        if ok {
            let gid = status().lock().game_id.clone();
            if let Some(gid) = gid {
                if add_to.is_some() {
                    match super::gameedit::reapply(&gid) {
                        Ok(true) => push("kept your earlier event edits"),
                        Ok(false) => {}
                        Err(e) => push(&format!("couldn't re-apply your earlier edits: {e:#}")),
                    }
                }
                match mark_analysed(&gid, &v) {
                    Ok(true) => push("saved the analysis of this video: its highlights are ready (no need to run AI analysis again)"),
                    Ok(false) => {}
                    Err(e) => push(&format!("couldn't save the analysis for this video: {e:#}")),
                }
            }
        }
        // pick up the new game everywhere
        super::games::refresh(&super::ctx().host, super::ctx());
        let mut s = status().lock();
        s.running = false;
        s.done = true;
        if !ok {
            s.error = Some(match r {
                Err(e) => format!("could not start the engine: {e}"),
                Ok(_) => s.log.iter().rev().find(|l| l.to_lowercase().contains("error")).cloned().unwrap_or_else(|| "the engine stopped with an error (see the log)".into()),
            });
        }
    });
    json!({"ok": true})
}

/// The engine already analysed the video while testing the game: store that as the video's
/// analysis (like "Run AI analysis" does), so its highlights and auto-montage work straight away.
fn mark_analysed(id: &str, video: &str) -> anyhow::Result<bool> {
    use anyhow::Context;
    let games = super::community::games_dir();
    let test: Value = serde_json::from_slice(&std::fs::read(games.join("_work").join(id).join("test.events.json")).context("no test run")?)?;
    let norm = |p: &str| p.replace('/', "\\").to_lowercase();
    let store = super::projects::get();
    let Some(project) = store.all().into_iter().find(|p| p["videoPath"].as_str().map(|v| norm(v) == norm(video)).unwrap_or(false)) else {
        return Ok(false); // video isn't in Rewind's library (picked from elsewhere)
    };
    let pid = project["id"].as_str().context("project id")?.to_string();
    let evj: Value = serde_json::from_slice(&std::fs::read(games.join(id).join("events.json")).unwrap_or_default()).unwrap_or(json!({}));
    let spec = |name: &str| evj["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == name)).cloned().unwrap_or(json!({}));
    let mut video_events = Vec::new();
    for e in test["events"].as_array().cloned().unwrap_or_default() {
        let name = e["name"].as_str().unwrap_or("").to_string();
        let t = e["time_sec"].as_f64().unwrap_or(0.0) * 1000.0;
        let s = spec(&name);
        let before = s["automontage"]["offsetBefore"].as_f64().unwrap_or(2.0) * 1000.0;
        let after = s["automontage"]["offsetAfter"].as_f64().unwrap_or(2.0) * 1000.0;
        video_events.push(json!({"eventName": name, "startTimeMs": t, "suggestedStartContextMs": (t - before).max(0.0), "suggestedEndContextMs": t + after, "score": s["eventScore"].as_f64().unwrap_or(10.0)}));
    }
    // keep whatever else an earlier analysis found (audio, transcript), replace the game events
    let mut raw = project["rawResult"].clone();
    if !raw.is_object() {
        raw = json!({});
    }
    raw["videoEvents"] = json!(video_events);
    let mut filter = project["keywordsAndFilter"].clone();
    if !filter.is_object() {
        filter = json!({});
    }
    let mut evs: Vec<Value> = filter["events"].as_array().cloned().unwrap_or_default();
    for e in evj["events"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = e["name"].as_str() {
            if !evs.iter().any(|x| x["id"] == n) {
                evs.push(json!({"id": n, "checked": e["default"].as_bool().unwrap_or(true)}));
            }
        }
    }
    filter["events"] = json!(evs);
    store.update(&pid, json!({"rawResult": raw, "powderGameId": id, "status": "ANALYZED", "keywordsAndFilter": filter}), false)?;
    // what auto-edits / auto-montage need, as after a normal analysis
    std::thread::spawn(move || {
        if let Err(e) = super::autoedits::start_for(&pid) {
            crate::log(&format!("auto-edits after add-game: {e:#}"));
        }
    });
    Ok(true)
}

pub fn log_snapshot() -> (Option<String>, Vec<String>) {
    let s = status().lock();
    (s.game_id.clone(), s.log.clone())
}

pub fn status_json() -> Value {
    let s = status().lock();
    json!({"running": s.running, "done": s.done, "error": s.error, "gameId": s.game_id, "log": s.log})
}
