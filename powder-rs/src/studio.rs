//! `powder-rs studio`: a local web page for building game packs without guessing coordinates.
//! Drop a screenshot (or load a video frame), drag rectangles over HUD text, test OCR live,
//! and write the crop coordinates straight back into game_postprocess.lua.

use crate::game::{find_game, list_games};
use crate::lua_engine::GameScript;
use crate::ocr::Ocr;
use anyhow::{anyhow, Context, Result};
use image::RgbImage;
use serde_json::{json, Value};
use std::io::Cursor;
#[allow(unused_imports)]
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Response, Server};

const PAGE: &str = include_str!("studio.html");

pub struct StudioConfig {
    pub dirs: Vec<PathBuf>,
    pub libs: Vec<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ocr_models: Option<PathBuf>,
    /// where new games are created
    pub games_dir: PathBuf,
}

type Job = std::sync::Arc<std::sync::Mutex<Option<std::result::Result<crate::autogame::AddGameReport, String>>>>;

pub(crate) struct State {
    pub(crate) dirs: Vec<PathBuf>,
    pub(crate) libs: Vec<PathBuf>,
    pub(crate) ffmpeg: Option<PathBuf>,
    pub(crate) ocr: Option<Ocr>,
    pub(crate) frame: Option<RgbImage>,
    pub(crate) ocr_models: Option<PathBuf>,
    pub(crate) games_dir: PathBuf,
    pub(crate) job_progress: crate::discover::SharedProgress,
    pub(crate) job: Job,
    pub(crate) job_running: bool,
    pub(crate) settings: crate::settings::Settings,
    pub(crate) jobs: crate::jobs::Jobs,
    pub(crate) recorder: crate::recorder::RecorderHandle,
}

pub(crate) fn json_resp(v: Value) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(v.to_string().into_bytes()).with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

pub fn serve(port: u16, cfg: StudioConfig, ocr: Option<Ocr>, game: Option<String>) -> Result<()> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("cannot listen on port {port}: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/{}", game.map(|g| format!("#edit:{g}")).unwrap_or_default());
    println!("powder-rs studio running at {url}  (Ctrl+C to stop)");
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    let mut dirs = cfg.dirs;
    if !dirs.contains(&cfg.games_dir) {
        dirs.insert(0, cfg.games_dir.clone());
    }
    let mut st = State {
        dirs,
        libs: cfg.libs,
        ffmpeg: cfg.ffmpeg,
        ocr,
        frame: None,
        ocr_models: cfg.ocr_models,
        games_dir: cfg.games_dir,
        job_progress: Default::default(),
        job: Default::default(),
        job_running: false,
        settings: crate::settings::Settings::load(),
        jobs: Default::default(),
        recorder: Default::default(),
    };
    crate::recorder::start_hotkeys(&st);
    for mut req in server.incoming_requests() {
        let path = req.url().split('?').next().unwrap_or("/").to_string();
        let query = req.url().split_once('?').map(|x| x.1.to_string()).unwrap_or_default();
        // large video uploads are streamed straight to disk
        if path == "/api/video/upload" {
            let res = (|| -> Result<Value> {
                let name = qparam(&query, "name").unwrap_or_else(|| "upload.mp4".into());
                let safe: String = name.chars().map(|c| if c.is_alphanumeric() || ".-_ ".contains(c) { c } else { '_' }).collect();
                let dir = st.games_dir.join("_work").join("uploads");
                std::fs::create_dir_all(&dir)?;
                let dst = dir.join(safe);
                let mut f = std::fs::File::create(&dst)?;
                let n = std::io::copy(req.as_reader(), &mut f)?;
                Ok(json!({ "path": dst, "bytes": n }))
            })();
            let resp = match res { Ok(v) => json_resp(v), Err(e) => json_resp(json!({"error": format!("{e:#}")})).with_status_code(400) };
            let _ = req.respond(resp);
            continue;
        }
        // video streaming with HTTP Range (seeking in the player)
        if path == "/api/library/video" || path == "/api/file" {
            let res = crate::app_api::stream_video(&st, &query, &req);
            match res {
                Ok(r) => { let _ = req.respond(r); }
                Err(e) => { let _ = req.respond(json_resp(json!({"error": format!("{e:#}")})).with_status_code(404)); }
            }
            continue;
        }
        let mut body = Vec::new();
        let _ = req.as_reader().read_to_end(&mut body);
        let res = match crate::app_api::handle(&mut st, req.method(), &path, &query, &body) {
            Ok(Some(r)) => Ok(r),
            Ok(None) => handle(&mut st, req.method(), &path, &query, &body),
            Err(e) => Err(e),
        };
        let resp = match res {
            Ok(r) => r,
            Err(e) => json_resp(json!({ "error": format!("{e:#}") })).with_status_code(400),
        };
        let _ = req.respond(resp);
    }
    Ok(())
}

pub(crate) fn qparam(q: &str, k: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (a, b) = kv.split_once('=')?;
        (a == k).then(|| urldecode(b))
    })
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 2;
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle(st: &mut State, m: &Method, path: &str, q: &str, body: &[u8]) -> Result<Response<Cursor<Vec<u8>>>> {
    let libs: Vec<&Path> = st.libs.iter().map(|p| p.as_path()).collect();
    match (m, path) {
        (Method::Get, "/games") => Ok(Response::from_data(PAGE.as_bytes().to_vec()).with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap())),
        (Method::Get, "/api/games") => {
            let g: Vec<Value> = list_games(&st.dirs).iter().map(|g| json!({"id": g.id, "name": g.meta.name, "dir": g.dir})).collect();
            Ok(json_resp(json!({ "games": g, "ocr": st.ocr.is_some() })))
        }
        (Method::Get, "/api/game") => {
            let id = qparam(q, "id").context("id")?;
            let g = find_game(&id, &st.dirs)?;
            let s = GameScript::load(&g.dir, &libs)?;
            Ok(json_resp(json!({ "id": g.id, "name": g.meta.name, "fps": s.fps()?, "crops": s.ocr_crops()?, "events": g.event_names() })))
        }
        (Method::Post, "/api/upload") => {
            let img = image::load_from_memory(body).context("not an image")?.to_rgb8();
            let (w, h) = (img.width(), img.height());
            st.frame = Some(img);
            Ok(json_resp(json!({ "width": w, "height": h })))
        }
        (Method::Post, "/api/load") => {
            let v: Value = serde_json::from_slice(body)?;
            let p = PathBuf::from(v["path"].as_str().context("path")?.trim().trim_matches('"'));
            let t = v["time"].as_f64().unwrap_or(0.0);
            let img = crate::video::load_image_or_frame(st.ffmpeg.as_ref(), &p, t)?;
            let (w, h) = (img.width(), img.height());
            st.frame = Some(img);
            Ok(json_resp(json!({ "width": w, "height": h })))
        }
        (Method::Get, "/api/frame.png") => {
            let f = st.frame.as_ref().context("no frame loaded")?;
            let mut png = Vec::new();
            f.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
            Ok(Response::from_data(png).with_header(Header::from_bytes("Content-Type", "image/png").unwrap()))
        }
        (Method::Post, "/api/ocr") => {
            let v: Value = serde_json::from_slice(body)?;
            let c = v["coords"].as_array().context("coords")?;
            let coords = [0, 1, 2, 3].map(|i| c[i].as_f64().unwrap_or(0.0) as f32);
            let spec = crate::tools::adhoc_crop(
                v["name"].as_str().unwrap_or("crop"),
                coords,
                v["dilate"].as_i64().unwrap_or(5) as i32,
                v["minArea"].as_i64().unwrap_or(50) as i32,
                v["margin"].as_i64().unwrap_or(10) as i32,
                v["chars"].as_str().unwrap_or(""),
            );
            let frame = st.frame.as_ref().context("load a screenshot first")?;
            let o = st.ocr.as_mut().context("OCR models/onnxruntime not available - see README setup")?;
            let r = o.run_crop(frame, &spec)?;
            let boxes: Vec<Value> = r.boxes.iter().map(|b| json!([b.x0, b.y0, b.x1, b.y1, b.text])).collect();
            Ok(json_resp(json!({ "text": r.text, "boxes": boxes })))
        }
        (Method::Post, "/api/save-crop") => {
            let v: Value = serde_json::from_slice(body)?;
            let id = v["game"].as_str().context("game")?;
            let g = find_game(id, &st.dirs)?;
            let name = v["name"].as_str().context("name")?;
            let c: Vec<f64> = serde_json::from_value(v["coords"].clone())?;
            let lua_path = g.dir.join("game_postprocess.lua");
            let src = std::fs::read_to_string(&lua_path)?;
            let new = set_crop_in_lua(&src, name, &c, &v)?;
            std::fs::copy(&lua_path, g.dir.join("game_postprocess.lua.bak"))?;
            std::fs::write(&lua_path, new)?;
            GameScript::load(&g.dir, &libs).context("saved file no longer loads (restored .bak is next to it)")?;
            Ok(json_resp(json!({ "ok": true })))
        }
        (Method::Get, "/api/powder") => {
            let p = crate::patcher::powder_dir(None).ok();
            Ok(json_resp(json!({ "powder": p })))
        }
        (Method::Post, "/api/addgame") => {
            if st.job_running && st.job.lock().unwrap().is_none() {
                anyhow::bail!("a game is already being added - wait for it to finish");
            }
            let v: Value = serde_json::from_slice(body)?;
            let ff = crate::paths::ffmpeg(st.ffmpeg.as_deref())?;
            let models = crate::paths::ocr_models_dir(st.ocr_models.as_deref())?;
            let num = |k: &str| v[k].as_f64().filter(|x| *x > 0.0);
            let sample = match v["fps"].as_str() {
                Some(s) if !s.is_empty() && s != "default" => crate::video::FpsSettings::parse_sample(s)?,
                _ => None,
            };
            let native = v["fps"].as_str() == Some("native");
            let powder = if v["install"].as_bool().unwrap_or(false) {
                Some(PathBuf::from(v["powder"].as_str().unwrap_or("")))
            } else {
                None
            };
            let mut o = crate::autogame::AddGameOptions {
                title: v["title"].as_str().unwrap_or("").to_string(),
                steam: v["steam"].as_str().filter(|s| !s.trim().is_empty()).map(String::from),
                video: PathBuf::from(v["video"].as_str().context("choose a video")?.trim().trim_matches('"')),
                games_dir: st.games_dir.clone(),
                game_files: v["gameFiles"].as_str().filter(|s| !s.trim().is_empty()).map(|s| PathBuf::from(s.trim().trim_matches('"'))),
                ffmpeg: ff,
                ocr_models: models,
                lua_libs: st.libs.clone(),
                fps: crate::video::FpsSettings { sample, recording: num("recordingFps"), game: num("gameFps") },
                scan_fps_default: num("scanFps").unwrap_or(2.0),
                online: v["online"].as_bool().unwrap_or(true),
                install_powder: powder,
                work_dir: st.games_dir.join("_work"),
                overwrite: true,
                reuse_scan: v["reuseScan"].as_bool().unwrap_or(false),
            };
            if o.title.trim().is_empty() && o.steam.is_none() {
                anyhow::bail!("enter the game title (or a Steam store link)");
            }
            if !o.video.exists() {
                anyhow::bail!("video not found: {}", o.video.display());
            }
            let _ = native;
            if o.fps.sample.is_none() && native {
                o.fps.sample = None;
            }
            *st.job.lock().unwrap() = None;
            *st.job_progress.lock().unwrap() = Default::default();
            st.job_running = true;
            let job = st.job.clone();
            let prog = st.job_progress.clone();
            std::thread::spawn(move || {
                let r = crate::autogame::add_game(&o, Some(prog)).map_err(|e| format!("{e:#}"));
                *job.lock().unwrap() = Some(r);
            });
            Ok(json_resp(json!({ "started": true })))
        }
        (Method::Get, "/api/addgame/status") => {
            let p = st.job_progress.lock().unwrap().clone();
            let r = st.job.lock().unwrap().clone();
            Ok(json_resp(match r {
                None => json!({ "running": st.job_running, "progress": p }),
                Some(Ok(rep)) => {
                    st.job_running = false;
                    json!({ "done": true, "report": rep })
                }
                Some(Err(e)) => {
                    st.job_running = false;
                    json!({ "done": true, "error": e })
                }
            }))
        }
        (Method::Get, "/api/thumb") => {
            // frame from a game's scan nearest to time t
            let id = qparam(q, "id").context("id")?;
            let t: f64 = qparam(q, "t").and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let d = crate::discover::load(&st.games_dir.join("_work").join(&id).join("discovery.json"))?;
            let i = d.frame_times.iter().enumerate().min_by(|a, b| (a.1 - t).abs().total_cmp(&(b.1 - t).abs())).map(|x| x.0).unwrap_or(0);
            let bytes = std::fs::read(Path::new(&d.frames_dir).join(format!("frame_{i:06}.jpg")))?;
            Ok(Response::from_data(bytes).with_header(Header::from_bytes("Content-Type", "image/jpeg").unwrap()))
        }
        (Method::Post, "/api/game/remove-event") => {
            let v: Value = serde_json::from_slice(body)?;
            let id = v["id"].as_str().context("id")?;
            let ev = v["event"].as_str().context("event")?;
            let g = find_game(id, &st.dirs)?;
            remove_event(&g.dir, ev)?;
            let mut reinstalled = Value::Null;
            if let Some(p) = v["powder"].as_str().filter(|p| !p.is_empty()) {
                let g = find_game(id, &st.dirs)?;
                let pd = crate::patcher::powder_dir(Some(PathBuf::from(p)))?;
                crate::patcher::install(&g, &pd, &st.libs)?;
                reinstalled = json!(pd);
            }
            Ok(json_resp(json!({ "ok": true, "reinstalled": reinstalled })))
        }
        _ => Ok(Response::from_string("not found").with_status_code(404)),
    }
}

/// Rewrite cropCoords (and optional detector params) of the crop block named `name`.
pub fn set_crop_in_lua(src: &str, name: &str, c: &[f64], v: &Value) -> Result<String> {
    let name_re = regex::Regex::new(&format!(r#"cropName\s*=\s*["']{}["']"#, regex::escape(name))).unwrap();
    let pos = name_re.find(src).with_context(|| format!("crop '{name}' not found in game_postprocess.lua - add it there first (copy the snippet)"))?;
    // the crop block is the innermost {...} around cropName
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut start = pos.start();
    while start > 0 {
        start -= 1;
        match bytes[start] {
            b'}' => depth += 1,
            b'{' => {
                if depth == 0 { break; }
                depth -= 1;
            }
            _ => {}
        }
    }
    let mut end = pos.end();
    depth = 0;
    while end < bytes.len() {
        match bytes[end] {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 { break; }
                depth -= 1;
            }
            _ => {}
        }
        end += 1;
    }
    let mut block = src[start..=end].to_string();
    let coords = format!("cropCoords = {{ {:.3}, {:.3}, {:.3}, {:.3} }}", c[0], c[1], c[2], c[3]);
    let cc = regex::Regex::new(r"cropCoords\s*=\s*\{[^}]*\}").unwrap();
    if cc.is_match(&block) {
        block = cc.replace(&block, coords.as_str()).to_string();
    } else {
        block = name_re.replace(&block, format!("{}, {coords}", &src[pos.start()..pos.end()])).to_string();
    }
    for (key, field) in [("dilate", "detectorDilateDiameter"), ("minArea", "detectorMinimumArea"), ("margin", "detectorMargin")] {
        if let Some(n) = v[key].as_i64() {
            let re = regex::Regex::new(&format!(r"{field}\s*=\s*-?\d+")).unwrap();
            block = re.replace(&block, format!("{field} = {n}")).to_string();
        }
    }
    Ok(format!("{}{}{}", &src[..start], block, &src[end + 1..]))
}

/// Remove one event from a generated game (detector lines, merge entry, events.json entry).
pub fn remove_event(dir: &Path, ev: &str) -> Result<()> {
    let lua_p = dir.join("game_postprocess.lua");
    let src = std::fs::read_to_string(&lua_p)?;
    let det = regex::Regex::new(&format!(r#"event\s*=\s*"{}""#, regex::escape(ev))).unwrap();
    let merge = regex::Regex::new(&format!(r"^\s*{}\s*=\s*[0-9.]+,\s*$", regex::escape(ev))).unwrap();
    let out: Vec<&str> = src.lines().filter(|l| !det.is_match(l) && !merge.is_match(l)).collect();
    std::fs::write(&lua_p, out.join("\n") + "\n")?;
    let ev_p = dir.join("events.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&ev_p)?)?;
    if let Some(a) = v["events"].as_array_mut() {
        a.retain(|e| e["name"].as_str() != Some(ev));
    }
    std::fs::write(&ev_p, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}
