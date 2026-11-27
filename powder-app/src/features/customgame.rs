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

fn is_link(v: &str) -> bool {
    v.starts_with("http://") || v.starts_with("https://")
}

/// Download a YouTube link (with the up-to-date yt-dlp); returns the file.
fn download(link: &str, n: usize, total: usize) -> Result<String, String> {
    push(&format!("[{n}/{total}] downloading {link} ..."));
    let folder = super::settings::read()["downloadFolder"].as_str().map(std::path::PathBuf::from).unwrap_or_else(|| crate::setup::engine_dir().join("downloads"));
    let _ = std::fs::create_dir_all(&folder);
    let out = folder.join(format!("youtube-{}.mp4", super::montage::uuid()));
    let last = std::sync::Arc::new(parking_lot::Mutex::new(0i64));
    let l2 = last.clone();
    match super::download::youtube_download(link, &out, &move |pct, _eta| {
        let p = (pct / 10.0) as i64;
        let mut l = l2.lock();
        if p > *l {
            *l = p;
            push(&format!("[{n}/{total}] [download] {:.0}%", pct));
        }
    }) {
        Ok(r) => {
            let v = r["outputPath"].as_str().unwrap_or("").to_string();
            push(&format!("[{n}/{total}] downloaded to {v}"));
            Ok(v)
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Add a game / improve it / train its model from one or more videos (files and YouTube links).
/// They go one after another (each needs the whole GPU); the YouTube links download in the
/// background meanwhile, so the next one is usually ready when the previous one finishes.
pub fn start(title: &str, videos: &[String], add_to: Option<&str>, model: bool, template: Option<&str>, parallel: bool) -> Value {
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
    let videos: Vec<String> = videos.iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
    let mut add_to = add_to.filter(|s| !s.is_empty()).map(String::from);
    let template = template.filter(|s| !s.is_empty()).map(String::from);
    std::thread::spawn(move || {
        let total = videos.len();
        // downloads run ahead in their own thread
        let ready: std::sync::Arc<Mutex<Vec<Option<Result<String, String>>>>> =
            std::sync::Arc::new(Mutex::new(videos.iter().map(|v| if is_link(v) { None } else { Some(Ok(v.clone())) }).collect()));
        {
            let ready = ready.clone();
            let videos = videos.clone();
            std::thread::spawn(move || {
                for (i, v) in videos.iter().enumerate() {
                    if is_link(v) {
                        let r = download(v, i + 1, total);
                        ready.lock()[i] = Some(r);
                    }
                }
            });
        }
        // parallel scanning: two videos are scanned for text at once (saved), then combined and
        // trained one by one. Scans need the game to exist, so a new game's first video goes alone.
        // 0 = waiting, 1 = scanned, 2 = scan failed (the video is then scanned in the normal run)
        let scan_state: std::sync::Arc<Mutex<Vec<u8>>> = std::sync::Arc::new(Mutex::new(vec![0; total]));
        let start_scans = |gid: String, from: usize| {
            let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(from));
            for _ in 0..2 {
                let (next, ready, scan_state, eng, root, ffmpeg, gid) = (next.clone(), ready.clone(), scan_state.clone(), eng.clone(), root.clone(), ffmpeg.clone(), gid.clone());
                std::thread::spawn(move || loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if i >= total {
                        break;
                    }
                    let r = loop {
                        if let Some(r) = ready.lock()[i].clone() {
                            break r;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    };
                    let Ok(v) = r else {
                        scan_state.lock()[i] = 2;
                        continue;
                    };
                    let mut c = Command::new(&eng);
                    c.current_dir(eng.parent().unwrap()).arg("add-game").arg(&v).arg("--add-to").arg(&gid).arg("--scan-only").arg("--powder").arg(&root);
                    if let Some(f) = &ffmpeg {
                        c.arg("--ffmpeg").arg(f);
                    }
                    c.stdout(Stdio::piped()).stderr(Stdio::piped());
                    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
                    push(&format!("[scan {}/{total}] scanning {} (in parallel)", i + 1, std::path::Path::new(&v).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()));
                    let ok = matches!(run_streaming_tagged(&mut c, Some(format!("[scan {}/{total}] ", i + 1))), Ok(s) if s.success());
                    scan_state.lock()[i] = if ok { 1 } else { 2 };
                });
            }
        };
        let mut scans_started = false;
        if parallel && total > 1 {
            if let Some(a) = &add_to {
                start_scans(a.clone(), 0);
                scans_started = true;
            }
        }
        let mut done_ok = 0usize;
        let mut failed: Vec<String> = Vec::new();
        for i in 0..total {
            let v = loop {
                if let Some(r) = ready.lock()[i].clone() {
                    break r;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            };
            let v = match v {
                Ok(v) => v,
                Err(e) => {
                    push(&format!("[{}/{total}] error: {e}", i + 1));
                    failed.push(format!("{}: {e}", videos[i]));
                    continue;
                }
            };
            let name = std::path::Path::new(&v).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            // wait for this video's parallel scan
            let mut prescanned = false;
            if scans_started {
                loop {
                    let st = scan_state.lock()[i];
                    if st != 0 {
                        prescanned = st == 1;
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
            if total > 1 {
                push(&format!("===== video {}/{total}: {name}{} =====", i + 1, if prescanned { " (already scanned)" } else { "" }));
            }
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
            if prescanned {
                c.arg("--reuse-scan");
            }
            if model {
                // the model must learn from what the text reading found, not from its own guesses
                c.env("POWDER_RS_NO_MODEL", "1");
            }
            crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
            push(&format!("Adding \"{t}\" from {name}"));
            let r = run_streaming(&mut c);
            let ok = matches!(&r, Ok(s) if s.success());
            if !ok {
                let why = match r {
                    Err(e) => format!("could not start the engine: {e}"),
                    Ok(_) => status().lock().log.iter().rev().find(|l| l.to_lowercase().contains("error")).cloned().unwrap_or_else(|| "the engine stopped with an error (see the log)".into()),
                };
                push(&format!("[{}/{total}] {name} failed: {why}", i + 1));
                failed.push(format!("{name}: {why}"));
                continue;
            }
            let Some(gid) = status().lock().game_id.clone() else { continue };
            let first_of_new = add_to.is_none();
            if first_of_new {
                if let Some(tid) = &template {
                    match super::gameedit::apply_template(&gid, tid) {
                        Ok(n) => push(&format!("used {tid}'s events as a template ({n} events)")),
                        Err(e) => push(&format!("couldn't use the template: {e:#}")),
                    }
                }
            } else {
                match super::gameedit::reapply(&gid) {
                    Ok(true) => push("kept your earlier event edits"),
                    Ok(false) => {}
                    Err(e) => push(&format!("couldn't re-apply your earlier edits: {e:#}")),
                }
            }
            if model {
                push("Model Creation: saving training pictures and training the model...");
                let mut m = Command::new(&eng);
                m.current_dir(eng.parent().unwrap()).arg("model-train").arg(&v).arg("--id").arg(&gid).arg("--dir").arg(super::community::games_dir());
                if let Some(f) = &ffmpeg {
                    m.arg("--ffmpeg").arg(f);
                }
                m.stdout(Stdio::piped()).stderr(Stdio::piped());
                crate::gpu::HiddenExt::creation_flags_hidden(&mut m);
                match run_streaming(&mut m) {
                    Ok(s) if s.success() => push("model updated"),
                    Ok(_) => push("training didn't finish (see above) - the game still works with text reading"),
                    Err(e) => push(&format!("couldn't start training: {e}")),
                }
            }
            match mark_analysed(&gid, &v) {
                Ok(true) => push("saved the analysis of this video: its highlights are ready (no need to run AI analysis again)"),
                Ok(false) => {}
                Err(e) => push(&format!("couldn't save the analysis for this video: {e:#}")),
            }
            // the next videos improve the same game
            if parallel && !scans_started && i + 1 < total {
                start_scans(gid.clone(), i + 1);
                scans_started = true;
            }
            add_to = Some(gid);
            done_ok += 1;
        }
        // pick up the new game everywhere
        super::games::refresh(&super::ctx().host, super::ctx());
        if total > 1 {
            push(&format!("===== finished: {done_ok} of {total} video(s) done{} =====", if failed.is_empty() { String::new() } else { format!(", {} failed", failed.len()) }));
        }
        let mut s = status().lock();
        s.running = false;
        s.done = true;
        if done_ok == 0 {
            s.error = Some(failed.first().cloned().unwrap_or_else(|| "no video given".into()));
        } else if !failed.is_empty() {
            s.error = Some(format!("{} of {total} video(s) failed: {}", failed.len(), failed.join("; ")));
        }
    });
    json!({"ok": true})
}

/// Model Creation without a new video: check (and re-sort) the game's training pictures, then retrain.
pub fn retrain(id: &str) -> Value {
    {
        let mut s = status().lock();
        if s.running {
            return json!({"error": "already running"});
        }
        *s = Status { running: true, game_id: Some(id.to_string()), ..Default::default() };
    }
    let Some(eng) = engine() else {
        let mut s = status().lock();
        s.running = false;
        s.done = true;
        s.error = Some("The game engine is missing - run build-app.bat again.".into());
        return json!({"ok": false});
    };
    let id = id.to_string();
    std::thread::spawn(move || {
        push("Checking the training pictures and retraining the model...");
        let mut m = Command::new(&eng);
        m.current_dir(eng.parent().unwrap()).arg("model-train").arg("--id").arg(&id).arg("--dir").arg(super::community::games_dir());
        if let Some(f) = crate::media::ffmpeg() {
            m.arg("--ffmpeg").arg(f);
        }
        m.stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::gpu::HiddenExt::creation_flags_hidden(&mut m);
        let r = run_streaming(&mut m);
        let mut s = status().lock();
        s.running = false;
        s.done = true;
        if !matches!(&r, Ok(x) if x.success()) {
            s.error = Some(s.log.iter().rev().find(|l| l.to_lowercase().contains("error")).cloned().unwrap_or_else(|| "training stopped with an error (see the log)".into()));
        }
    });
    json!({"ok": true})
}

/// Undo the picture checks for a game and lock its folders as sorted by hand.
pub fn restore(id: &str) -> Value {
    let Some(eng) = engine() else { return json!({"ok": false, "error": "engine missing"}) };
    let mut c = Command::new(&eng);
    c.current_dir(eng.parent().unwrap()).arg("model-restore").arg("--id").arg(id).arg("--dir").arg(super::community::games_dir());
    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
    match c.output() {
        Ok(o) if o.status.success() => json!({"ok": true, "message": String::from_utf8_lossy(&o.stdout).trim().to_string()}),
        Ok(o) => json!({"ok": false, "error": String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("failed").to_string()}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

/// run the engine, streaming its output into the log (and picking up the game id)
fn run_streaming(c: &mut Command) -> std::io::Result<std::process::ExitStatus> {
    run_streaming_tagged(c, None)
}

/// the same, with every line marked (for scans running alongside)
fn run_streaming_tagged(c: &mut Command, tag: Option<String>) -> std::io::Result<std::process::ExitStatus> {
    let tag = tag.unwrap_or_default();
    let tag2 = tag.clone();
    c.spawn().and_then(|mut child| {
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let h = std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                let l = l.trim().to_string();
                if !l.is_empty() {
                    push(&format!("{tag2}{l}"));
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
            push(&format!("{tag}{l}"));
        }
        let _ = h.join();
        child.wait()
    })
}

/// One highlight in the analysis result's videoEvents (same shape the native analyzer gives).
pub fn video_event(events_json: &Value, name: &str, time_sec: f64) -> Value {
    let s = events_json["events"].as_array().and_then(|a| a.iter().find(|e| e["name"] == name)).cloned().unwrap_or(json!({}));
    let t = time_sec * 1000.0;
    let before = s["automontage"]["offsetBefore"].as_f64().unwrap_or(2.0) * 1000.0;
    let after = s["automontage"]["offsetAfter"].as_f64().unwrap_or(2.0) * 1000.0;
    json!({"eventName": name, "startTimeMs": t, "suggestedStartContextMs": (t - before).max(0.0), "suggestedEndContextMs": t + after, "score": s["eventScore"].as_f64().unwrap_or(10.0)})
}

/// For "Run AI analysis": Powder's own analyzer can't read .rwdmdl models, so a game made with
/// Model Creation gets the model's extra events added afterwards.
pub fn add_model_events(game: &str, video: &str, start: Option<f64>, end: Option<f64>, result: &mut Value) -> anyhow::Result<usize> {
    let dir = super::community::games_dir().join(game);
    let Some(model) = std::fs::read_dir(&dir).ok().and_then(|r| r.flatten().map(|e| e.path()).find(|p| p.extension().map(|x| x == "rwdmdl").unwrap_or(false) && !p.to_string_lossy().ends_with(".fight.rwdmdl"))) else { return Ok(0) };
    let eng = engine().ok_or_else(|| anyhow::anyhow!("engine missing"))?;
    let mut c = Command::new(&eng);
    c.current_dir(eng.parent().unwrap()).arg("model-detect").arg(video).arg("--model").arg(&model);
    if let Some(s) = start { c.arg("--start").arg(s.to_string()); }
    if let Some(e) = end { c.arg("--end").arg(e.to_string()); }
    if let Some(f) = crate::media::ffmpeg() { c.arg("--ffmpeg").arg(f); }
    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
    let out = c.stderr(Stdio::null()).output()?;
    let mut found: Vec<Value> = serde_json::from_slice(&out.stdout)?;
    // nothing past the analysed range (the model reads whole seconds)
    if let Some(e) = end {
        found.retain(|f| f["time_sec"].as_f64().map(|t| t <= e).unwrap_or(false));
    }
    let evj: Value = serde_json::from_slice(&std::fs::read(dir.join("events.json")).unwrap_or_default()).unwrap_or(json!({}));
    if !result["videoEvents"].is_array() { result["videoEvents"] = json!([]); }
    let ve = result["videoEvents"].as_array_mut().unwrap();
    let mut added = 0;
    for f in found {
        let (n, t) = (f["name"].as_str().unwrap_or(""), f["time_sec"].as_f64().unwrap_or(0.0));
        if ve.iter().any(|e| e["eventName"] == n && (e["startTimeMs"].as_f64().unwrap_or(-1e9) - t * 1000.0).abs() < 3000.0) { continue; }
        ve.push(video_event(&evj, n, t));
        added += 1;
    }
    Ok(added)
}

/// Where each event's clip should start: the fight before it, found from the ammo counter and the
/// game's "fighting" model (Model Creation). Stored on the events as fightStartMs.
pub fn add_fight_starts(game: &str, video: &str, result: &mut Value) -> anyhow::Result<usize> {
    let dir = super::community::games_dir();
    if !dir.join(game).join("fight.json").exists() {
        return Ok(0);
    }
    let Some(ve) = result["videoEvents"].as_array_mut() else { return Ok(0) };
    if ve.is_empty() {
        return Ok(0);
    }
    let times: Vec<String> = ve.iter().map(|e| format!("{:.2}", e["startTimeMs"].as_f64().unwrap_or(0.0) / 1000.0)).collect();
    let eng = engine().ok_or_else(|| anyhow::anyhow!("engine missing"))?;
    let mut c = Command::new(&eng);
    c.current_dir(eng.parent().unwrap()).arg("fight-starts").arg(video).arg("--id").arg(game).arg("--dir").arg(&dir).arg("--times").arg(times.join(","));
    if let Some(f) = crate::media::ffmpeg() { c.arg("--ffmpeg").arg(f); }
    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
    let out = c.stderr(Stdio::null()).output()?;
    let found: Vec<Value> = serde_json::from_slice(&out.stdout)?;
    let mut n = 0;
    for (e, f) in ve.iter_mut().zip(found) {
        if let Some(s) = f["start"].as_f64() {
            e["fightStartMs"] = json!((s * 1000.0).round());
            n += 1;
        }
    }
    Ok(n)
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
    let video_events: Vec<Value> = test["events"].as_array().cloned().unwrap_or_default().iter().map(|e| video_event(&evj, e["name"].as_str().unwrap_or(""), e["time_sec"].as_f64().unwrap_or(0.0))).collect();
    let _ = spec;
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
