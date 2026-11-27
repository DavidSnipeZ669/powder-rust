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

pub fn start(title: &str, video: &str) -> Value {
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
    std::thread::spawn(move || {
        let mut c = Command::new(&eng);
        c.current_dir(eng.parent().unwrap()).arg("add-game").arg(&v).arg("--powder").arg(&root);
        if t.contains("store.steampowered.com") || t.chars().all(|ch| ch.is_ascii_digit()) && !t.is_empty() {
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

pub fn log_snapshot() -> (Option<String>, Vec<String>) {
    let s = status().lock();
    (s.game_id.clone(), s.log.clone())
}

pub fn status_json() -> Value {
    let s = status().lock();
    json!({"running": s.running, "done": s.done, "error": s.error, "gameId": s.game_id, "log": s.log})
}
