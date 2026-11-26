//! HTTP API for the app UI (library, player, analysis, clips, montage, recorder, settings).

use crate::library::{self, LibEvent};
use crate::studio::{json_resp, qparam, State};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;
use tiny_http::{Header, Method, Request, Response, StatusCode};

const APP: &str = include_str!("app.html");

type Resp = Response<Cursor<Vec<u8>>>;

fn ff(st: &State) -> Result<PathBuf> {
    crate::paths::ffmpeg(st.ffmpeg.as_deref())
}

fn video_path(st: &State, id: &str) -> Result<PathBuf> {
    library::find(&st.settings, id).map(|v| v.path).context("video not found in the library")
}

/// Serve a video (or any file under the library / data folders) with Range support.
pub fn stream_video(st: &State, q: &str, req: &Request) -> Result<Response<std::io::Take<std::fs::File>>> {
    let path = match qparam(q, "id") {
        Some(id) => video_path(st, &id)?,
        None => {
            let p = PathBuf::from(qparam(q, "path").context("path")?);
            // only files we produced / manage
            let allowed = [crate::settings::data_dir(), st.settings.clips_folder.clone(), st.settings.montages_folder.clone(), st.settings.recordings_folder.clone(), st.games_dir.clone()];
            anyhow::ensure!(allowed.iter().any(|a| p.starts_with(a)), "not allowed");
            p
        }
    };
    let mut f = std::fs::File::open(&path)?;
    let len = f.metadata()?.len();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let ctype = match ext.as_str() { "mp4" | "m4v" | "mov" => "video/mp4", "webm" => "video/webm", "mkv" => "video/x-matroska", "jpg" | "jpeg" => "image/jpeg", "png" => "image/png", "mp3" => "audio/mpeg", _ => "application/octet-stream" };
    let range = req.headers().iter().find(|h| h.field.equiv("Range")).map(|h| h.value.as_str().to_string());
    let (start, end) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap_or((r, ""));
            let a: u64 = a.parse().unwrap_or(0);
            let b: u64 = b.parse().unwrap_or(len.saturating_sub(1)).min(len.saturating_sub(1));
            (a.min(len), b)
        }
        None => (0, len.saturating_sub(1)),
    };
    let n = if len == 0 { 0 } else { end + 1 - start };
    f.seek(SeekFrom::Start(start))?;
    let mut headers = vec![Header::from_bytes("Content-Type", ctype).unwrap(), Header::from_bytes("Accept-Ranges", "bytes").unwrap()];
    let status = if range.is_some() {
        headers.push(Header::from_bytes("Content-Range", format!("bytes {start}-{end}/{len}")).unwrap());
        206
    } else {
        200
    };
    Ok(Response::new(StatusCode(status), headers, f.take(n), Some(n as usize), None))
}

fn analyze_job(st: &State, id: String, game: String, fps: String) -> Result<u64> {
    let video = video_path(st, &id)?;
    let ffmpeg = ff(st)?;
    let dirs = st.dirs.clone();
    let libs = st.libs.clone();
    let models = crate::paths::ocr_models_dir(st.ocr_models.as_deref())?;
    let title = format!("Analyse {}", video.file_name().unwrap().to_string_lossy());
    Ok(st.jobs.spawn("analyze", &title, move |h| {
        let g = crate::game::find_game(&game, &dirs)?;
        let sample = if fps.is_empty() || fps == "default" { None } else { crate::video::FpsSettings::parse_sample(&fps)? };
        let prog: crate::discover::SharedProgress = Default::default();
        let opt = crate::analyze::AnalyzeOptions {
            ffmpeg,
            lua_libs: libs,
            ocr_models: models,
            start: None,
            end: None,
            fps: crate::video::FpsSettings { sample, recording: None, game: None },
            fps_native_requested: fps == "native",
            skip_unchanged: true,
            max_height: Some(1440),
            debug_dir: None,
            debug_all: false,
            quiet: true,
            workers: None,
            progress: Some(prog.clone()),
        };
        let p2 = prog.clone();
        let h2 = h.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let t = std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                let p = p2.lock().unwrap().clone();
                if p.total > 0 { h2.progress(p.done as f64 / p.total as f64, format!("frame {}/{}", p.done, p.total)); }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
        let a = crate::analyze::analyze(&g, &video, &opt);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = t.join();
        let a = a?;
        library::store_analysis(&video, &a)?;
        Ok(json!({ "events": a.events.len(), "game": a.game }))
    }))
}

pub fn handle(st: &mut State, m: &Method, path: &str, q: &str, body: &[u8]) -> Result<Option<Resp>> {
    let v = || -> Value { serde_json::from_slice(body).unwrap_or(Value::Null) };
    let r = match (m, path) {
        (Method::Get, "/") => Response::from_data(APP.as_bytes().to_vec()).with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap()),
        (Method::Get, "/api/settings") => json_resp(serde_json::to_value(&st.settings)?),
        (Method::Post, "/api/settings") => {
            st.settings = serde_json::from_slice(body).context("invalid settings")?;
            st.settings.save()?;
            crate::recorder::restart_hotkeys(st);
            json_resp(json!({"ok": true}))
        }
        (Method::Get, "/api/jobs") => json_resp(serde_json::to_value(st.jobs.list())?),
        (Method::Get, "/api/encoders") => {
            let f = ff(st)?;
            let rep = crate::encode::report(&f);
            json_resp(json!({ "encoders": rep.iter().map(|(e, ok)| json!({"name": e, "works": ok})).collect::<Vec<_>>(), "auto": crate::encode::pick(&f, "auto") }))
        }
        (Method::Get, "/api/library") => {
            let vids = library::scan(&st.settings);
            json_resp(serde_json::to_value(vids)?)
        }
        (Method::Get, "/api/library/thumb") => {
            let id = qparam(q, "id").context("id")?;
            let t = qparam(q, "t").and_then(|x| x.parse().ok());
            let vp = video_path(st, &id)?;
            let p = library::thumbnail(&ff(st)?, &vp, t)?;
            Response::from_data(std::fs::read(p)?).with_header(Header::from_bytes("Content-Type", "image/jpeg").unwrap())
        }
        (Method::Post, "/api/library/set-game") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let mut sc = library::load_sidecar(&vp);
            sc.game = v["game"].as_str().filter(|s| !s.is_empty()).map(String::from);
            library::save_sidecar(&vp, &sc)?;
            json_resp(json!({"ok": true}))
        }
        (Method::Post, "/api/library/analyze") => {
            let v = v();
            let id = v["id"].as_str().context("id")?.to_string();
            let vp = video_path(st, &id)?;
            let game = v["game"].as_str().filter(|s| !s.is_empty()).map(String::from).or(library::load_sidecar(&vp).game).context("choose the game first")?;
            let fps = v["fps"].as_str().unwrap_or(&st.settings.analysis_fps).to_string();
            let j = analyze_job(st, id, game, fps)?;
            json_resp(json!({"job": j}))
        }
        (Method::Post, "/api/library/bookmark") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let mut sc = library::load_sidecar(&vp);
            let t = v["time"].as_f64().context("time")?;
            if v["remove"].as_bool().unwrap_or(false) {
                sc.bookmarks.retain(|b| (b - t).abs() > 0.05);
            } else {
                sc.bookmarks.push(t);
                sc.bookmarks.sort_by(|a, b| a.total_cmp(b));
            }
            library::save_sidecar(&vp, &sc)?;
            json_resp(json!({"ok": true}))
        }
        (Method::Post, "/api/library/remove-event") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let mut sc = library::load_sidecar(&vp);
            let t = v["time"].as_f64().context("time")?;
            let n = v["name"].as_str().unwrap_or("");
            sc.events.retain(|e| !(e.name == n && (e.time_sec - t).abs() < 0.05));
            library::save_sidecar(&vp, &sc)?;
            json_resp(json!({"ok": true}))
        }
        (Method::Post, "/api/clip") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let start = v["start"].as_f64().context("start")?.max(0.0);
            let end = v["end"].as_f64().context("end")?;
            let vertical = v["vertical"].as_bool().unwrap_or(false);
            let ffm = ff(st)?;
            let enc = st.settings.recorder.encoder.clone();
            let stem = vp.file_stem().unwrap().to_string_lossy().to_string();
            let label = v["label"].as_str().unwrap_or("clip").replace(|c: char| !c.is_alphanumeric(), "_");
            let out = st.settings.clips_folder.join(format!("{stem}_{label}_{:.0}{}.mp4", start, if vertical { "_vertical" } else { "" }));
            let game = library::load_sidecar(&vp).game;
            let j = st.jobs.spawn("clip", &format!("Clip {label}"), move |h| {
                h.progress(-1.0, "encoding");
                crate::montage::export_clip(&ffm, &vp, start, end, &out, &enc, "high", vertical.then_some([0.34197530864, 0.0, 0.31604938271, 1.0]))?;
                let mut sc = library::load_sidecar(&out);
                sc.game = game;
                sc.source = Some("clip".into());
                library::save_sidecar(&out, &sc)?;
                Ok(json!({ "output": out }))
            });
            json_resp(json!({"job": j}))
        }
        (Method::Get, "/api/music") => {
            let f = ff(st)?;
            let pd = st.settings.powder_dir.clone().or(crate::patcher::powder_dir(None).ok());
            json_resp(serde_json::to_value(crate::music::list(&f, pd.as_deref(), &st.settings.music_folders))?)
        }
        (Method::Post, "/api/montage") => {
            let v = v();
            let ids: Vec<String> = serde_json::from_value(v["ids"].clone()).context("ids")?;
            let ffm = ff(st)?;
            let mut sources = Vec::new();
            for id in &ids {
                let vp = video_path(st, id)?;
                let sc = library::load_sidecar(&vp);
                let mut events = sc.events.clone();
                for b in &sc.bookmarks {
                    events.push(LibEvent { name: "bookmark".into(), display_name: "Bookmark".into(), time_sec: *b, source: "bookmark".into() });
                }
                let ej = sc.game.as_ref().and_then(|g| crate::game::find_game(g, &st.dirs).ok()).map(|g| g.events);
                let (duration, has_audio, w, h) = crate::montage::probe_source(&ffm, &vp)?;
                sources.push(crate::montage::Source { video: vp, events, events_json: ej, duration, has_audio, width: w, height: h });
            }
            let music = v["music"].as_str().filter(|s| !s.is_empty()).map(PathBuf::from);
            let music_len = music.as_ref().and_then(|m| crate::montage::probe_source(&ffm, m).ok()).map(|x| x.0);
            let opt = crate::montage::MontageOptions {
                target_sec: v["target"].as_f64().filter(|x| *x > 0.0).or(music_len).unwrap_or(60.0),
                music,
                vertical: v["vertical"].as_bool().unwrap_or(false),
                height: v["height"].as_u64().unwrap_or(1080) as u32,
                fps: v["fps"].as_u64().unwrap_or(60) as u32,
                effects: v["effects"].as_bool().unwrap_or(true),
                music_volume: v["musicVolume"].as_f64().unwrap_or(0.8),
                game_volume: v["gameVolume"].as_f64().unwrap_or(0.6),
                seed: v["seed"].as_u64().unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                encoder: st.settings.recorder.encoder.clone(),
                ..Default::default()
            };
            let pd = st.settings.powder_dir.clone().or(crate::patcher::powder_dir(None).ok());
            let effects_file = pd.map(|p| p.join("ai-configs").join("automatic_montage").join("config.json")).filter(|p| p.exists());
            let bundles = crate::montage::load_effects(effects_file.as_deref());
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            let out = st.settings.montages_folder.join(format!("Montage_{stamp}{}.mp4", if opt.vertical { "_vertical" } else { "" }));
            let j = st.jobs.spawn("montage", "Auto-montage", move |h| {
                let r = crate::montage::make(&ffm, &sources, &opt, &bundles, &out, &|p, m| h.progress(p, m))?;
                let mut sc = library::load_sidecar(&out);
                sc.source = Some("montage".into());
                library::save_sidecar(&out, &sc)?;
                Ok(r)
            });
            json_resp(json!({"job": j}))
        }
        (Method::Post, "/api/reveal") => {
            let v = v();
            let p = PathBuf::from(v["path"].as_str().context("path")?);
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
            }
            #[cfg(not(windows))]
            {
                let _ = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(&p)).spawn();
            }
            json_resp(json!({"ok": true}))
        }
        (Method::Post, "/api/library/audio-peaks") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let sens = v["sensitivity"].as_f64().unwrap_or(10.0) as f32;
            let ffm = ff(st)?;
            let j = st.jobs.spawn("audio", "Find loudness spikes", move |h| {
                h.progress(-1.0, "decoding audio");
                let s = crate::audio::decode(&ffm, &vp, 16000)?;
                let peaks = crate::audio::loudness_peaks(&s, 16000, sens);
                let items: Vec<(f64, String)> = peaks.iter().map(|t| (*t, String::new())).collect();
                library::set_events(&vp, "audio", "loudness-peak", "Loudness spike", &items)?;
                Ok(json!({ "events": peaks.len() }))
            });
            json_resp(json!({"job": j}))
        }
        (Method::Post, "/api/library/transcribe") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let lang = v["language"].as_str().unwrap_or("auto").to_string();
            let ffm = ff(st)?;
            let pd = st.settings.powder_dir.clone().or(crate::patcher::powder_dir(None).ok());
            let model = crate::audio::find_model(pd.as_deref()).context("no whisper model: put a ggml model (e.g. Powder's ai-configs/whisper/ggml-base.bin) in models/whisper/")?;
            let j = st.jobs.spawn("transcribe", "Transcribe", move |h| {
                h.progress(0.0, "transcribing (this takes a while)");
                let lines = crate::audio::transcribe(&ffm, &vp, &model, &lang, &|p| h.progress(p, "transcribing"))?;
                let mut sc = library::load_sidecar(&vp);
                sc.transcript = lines;
                library::save_sidecar(&vp, &sc)?;
                Ok(json!({ "lines": sc.transcript.len() }))
            });
            json_resp(json!({"job": j}))
        }
        (Method::Post, "/api/library/keywords") => {
            let v = v();
            let vp = video_path(st, v["id"].as_str().context("id")?)?;
            let kws: Vec<String> = v["keywords"].as_str().unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let sc = library::load_sidecar(&vp);
            anyhow::ensure!(!sc.transcript.is_empty(), "transcribe the video first");
            let hits = crate::audio::keyword_hits(&sc.transcript, &kws);
            library::set_events(&vp, "keyword", "keyword", "Keyword", &hits)?;
            json_resp(json!({ "events": hits.len() }))
        }
        (Method::Get, "/api/library/transcript") => {
            let vp = video_path(st, &qparam(q, "id").context("id")?)?;
            json_resp(serde_json::to_value(library::load_sidecar(&vp).transcript)?)
        }
        (Method::Post, "/api/download") => {
            let v = v();
            let url = v["url"].as_str().context("url")?.trim().to_string();
            anyhow::ensure!(url.starts_with("http"), "paste a video URL (Twitch VOD/clip, YouTube, Kick ...)");
            let ffm = ff(st)?;
            let pd = st.settings.powder_dir.clone().or(crate::patcher::powder_dir(None).ok());
            let yt = crate::download::ytdlp(pd.as_deref())?;
            let folder = st.settings.library_folders.first().cloned().unwrap_or(st.settings.recordings_folder.clone()).join("Downloads");
            let j = st.jobs.spawn("download", "Download VOD", move |h| {
                let (video, chat) = crate::download::download(&yt, &ffm, &url, &folder, &|p, m| h.progress(p, m.to_string()))?;
                let mut n = 0;
                let mut sc = library::load_sidecar(&video);
                sc.source = Some("download".into());
                sc.chat_file = chat.clone();
                library::save_sidecar(&video, &sc)?;
                if let Some(c) = &chat {
                    let spikes = crate::download::chat_spikes(&crate::download::chat_times(c)?);
                    n = spikes.len();
                    let items: Vec<(f64, String)> = spikes.iter().map(|t| (*t, String::new())).collect();
                    library::set_events(&video, "chat", "chat-event", "Chat spike", &items)?;
                }
                Ok(json!({ "output": video, "events": n }))
            });
            json_resp(json!({"job": j}))
        }
        (Method::Get, "/api/recorder") => json_resp(crate::recorder::status(st)),
        (Method::Post, "/api/recorder") => {
            let v = v();
            let r = crate::recorder::command(st, v["action"].as_str().unwrap_or(""), &v)?;
            json_resp(r)
        }
        _ => return Ok(None),
    };
    Ok(Some(r))
}
