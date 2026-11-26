//! Recorder: screen + audio capture with an instant-replay buffer and full sessions.
//!
//! * Video: one ffmpeg process captures the screen (Windows: Desktop Duplication `ddagrab`,
//!   falling back to `gdigrab`) and writes 2-second MPEG-TS segments with the best working
//!   encoder (NVENC / AMF / QSV / Media Foundation / x264).
//! * Audio (Windows): system sound via WASAPI loopback and the microphone via `cpal`, written to
//!   WAV next to the segments and muxed in when a recording or replay is saved.
//! * Replay buffer keeps only the last N seconds of segments; a session keeps everything from
//!   "start" to "stop". Bookmarks (hotkey) are stored as events on the saved recording.
//! * When a session stops it is analysed automatically if its game is known.

use crate::library::{self, Sidecar};
use crate::settings::Settings;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const SEG: f64 = 2.0;

#[derive(Default)]
struct Session {
    /// seconds on the capture timeline when the session started
    start_t: f64,
    bookmarks: Vec<f64>,
    game: Option<String>,
    started_wall: u64,
}

struct Capture {
    child: Child,
    dir: PathBuf,
    started: Instant,
    encoder: String,
    source: String,
    audio: Option<audio::AudioCapture>,
}

#[derive(Default)]
pub struct Inner {
    capture: Option<Capture>,
    session: Option<Session>,
    last_saved: Vec<String>,
    last_error: Option<String>,
    detected_game: Option<String>,
    // config snapshot for background work (hotkeys run outside the HTTP thread)
    settings: Option<Settings>,
    ffmpeg: Option<PathBuf>,
    dirs: Vec<PathBuf>,
    libs: Vec<PathBuf>,
    models: Option<PathBuf>,
    jobs: Option<crate::jobs::Jobs>,
}

#[derive(Clone, Default)]
pub struct RecorderHandle(pub Arc<Mutex<Inner>>);

fn now_wall() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn sync_config(st: &crate::studio::State) {
    let mut g = st.recorder.0.lock().unwrap();
    g.settings = Some(st.settings.clone());
    g.ffmpeg = crate::paths::ffmpeg(st.ffmpeg.as_deref()).ok();
    g.dirs = st.dirs.clone();
    g.libs = st.libs.clone();
    g.models = crate::paths::ocr_models_dir(st.ocr_models.as_deref()).ok();
    g.jobs = Some(st.jobs.clone());
}

fn capture_input_args(s: &Settings, ffmpeg: &Path) -> (Vec<String>, String) {
    let fps = s.recorder.fps.max(10);
    let filters = {
        let out = Command::new(ffmpeg).args(["-hide_banner", "-filters"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        out
    };
    if std::env::var("POWDER_RS_TEST_CAPTURE").is_ok() {
        return (vec!["-re".into(), "-f".into(), "lavfi".into(), "-i".into(), format!("testsrc2=size=1280x720:rate={fps}")], "test pattern".into());
    }
    if cfg!(windows) {
        if filters.contains(" ddagrab ") {
            return (
                vec!["-f".into(), "lavfi".into(), "-i".into(), format!("ddagrab=output_idx={}:framerate={fps}:draw_mouse=1,hwdownload,format=bgra", s.recorder.monitor)],
                "Desktop Duplication (ddagrab)".into(),
            );
        }
        return (vec!["-f".into(), "gdigrab".into(), "-framerate".into(), fps.to_string(), "-i".into(), "desktop".into()], "GDI (gdigrab)".into());
    }
    if let Ok(d) = std::env::var("DISPLAY") {
        return (vec!["-f".into(), "x11grab".into(), "-framerate".into(), fps.to_string(), "-i".into(), d], "X11".into());
    }
    (vec!["-re".into(), "-f".into(), "lavfi".into(), "-i".into(), format!("testsrc2=size=1280x720:rate={fps}")], "test pattern (no display)".into())
}

fn start_capture(g: &mut Inner) -> Result<()> {
    if g.capture.is_some() {
        return Ok(());
    }
    let s = g.settings.clone().unwrap_or_default();
    let ff = g.ffmpeg.clone().context("ffmpeg not found")?;
    let dir = crate::settings::data_dir().join("capture").join(now_wall().to_string());
    std::fs::create_dir_all(&dir)?;
    let enc = crate::encode::pick(&ff, &s.recorder.encoder);
    let (input, source) = capture_input_args(&s, &ff);
    let h = if s.recorder.height == 0 { 2160 } else { s.recorder.height };
    let fps = s.recorder.fps.max(10);
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-v".into(), "error".into(), "-y".into()];
    args.extend(input);
    args.extend(["-vf".into(), format!("scale=-2:'min({h},ih)':flags=bicubic,fps={fps}")]);
    args.extend(crate::encode::args(&enc, &s.recorder.quality, h));
    args.extend([
        "-g".into(), ((fps as f64 * SEG) as u32).to_string(),
        "-force_key_frames".into(), format!("expr:gte(t,n_forced*{SEG})"),
        "-an".into(),
        "-f".into(), "segment".into(), "-segment_time".into(), SEG.to_string(), "-reset_timestamps".into(), "1".into(),
        "-segment_format".into(), "mpegts".into(),
        dir.join("seg_%06d.ts").display().to_string(),
    ]);
    let child = Command::new(&ff).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().context("starting ffmpeg capture")?;
    let started = Instant::now();
    let audio = audio::AudioCapture::start(&dir, s.recorder.capture_system_audio, s.recorder.capture_microphone, started).map_err(|e| g.last_error = Some(format!("audio: {e:#}"))).ok();
    g.capture = Some(Capture { child, dir, started, encoder: enc, source, audio });
    g.last_error = None;
    Ok(())
}

fn stop_capture(g: &mut Inner) {
    if let Some(mut c) = g.capture.take() {
        if let Some(stdin) = c.child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(b"q");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = c.child.try_wait() { break; }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = c.child.kill();
        if let Some(a) = c.audio.take() { a.stop(); }
        let dir = c.dir.clone();
        std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(2)); let _ = std::fs::remove_dir_all(dir); });
    }
}

fn segments(dir: &Path) -> Vec<(usize, PathBuf)> {
    let mut v: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.strip_prefix("seg_").and_then(|x| x.strip_suffix(".ts")).and_then(|x| x.parse().ok()).map(|i| (i, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Delete segments that are older than the replay window (unless a session needs them).
fn prune(g: &Inner) {
    let Some(c) = &g.capture else { return };
    let keep_from_t = match &g.session {
        Some(s) => s.start_t - SEG,
        None => c.started.elapsed().as_secs_f64() - g.settings.as_ref().map(|s| s.recorder.replay_seconds as f64).unwrap_or(60.0) - 2.0 * SEG,
    };
    for (i, p) in segments(&c.dir) {
        if ((i + 1) as f64) * SEG < keep_from_t {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Join segments covering [from_t, to_t] (capture timeline) plus audio into `out`.
/// Returns the capture time of the first frame in the output.
fn assemble(g: &Inner, from_t: f64, to_t: f64, out: &Path) -> Result<f64> {
    let c = g.capture.as_ref().context("not capturing")?;
    let ff = g.ffmpeg.clone().context("ffmpeg")?;
    // the newest segment is still being written: only use complete ones
    let segs = segments(&c.dir);
    let complete: Vec<&(usize, PathBuf)> = segs.iter().take(segs.len().saturating_sub(1)).filter(|(i, _)| ((*i + 1) as f64) * SEG > from_t && (*i as f64) * SEG < to_t).collect();
    if complete.is_empty() {
        bail!("nothing recorded yet - wait a few seconds");
    }
    let first_t = complete[0].0 as f64 * SEG;
    let last_t = (complete[complete.len() - 1].0 + 1) as f64 * SEG;
    let tmp = c.dir.join(format!("join_{}", now_wall()));
    std::fs::create_dir_all(&tmp)?;
    let list: String = complete.iter().map(|(_, p)| format!("file '{}'\n", p.display().to_string().replace('\'', "'\\''"))).collect();
    std::fs::write(tmp.join("list.txt"), list)?;
    let video_only = tmp.join("video.mp4");
    run(&ff, &["-f", "concat", "-safe", "0", "-i", &tmp.join("list.txt").display().to_string(), "-c", "copy", &video_only.display().to_string()])?;
    if let Some(p) = out.parent() { std::fs::create_dir_all(p)?; }
    let wavs: Vec<(PathBuf, f64)> = c.audio.as_ref().map(|a| a.files()).unwrap_or_default();
    let sync = g.settings.as_ref().map(|s| s.recorder.audio_sync_ms as f64 / 1000.0).unwrap_or(0.0);
    if wavs.is_empty() {
        run(&ff, &["-i", &video_only.display().to_string(), "-c", "copy", "-movflags", "+faststart", &out.display().to_string()])?;
    } else {
        let mut args: Vec<String> = vec!["-i".into(), video_only.display().to_string()];
        for (w, offset) in &wavs {
            // wav time 0 corresponds to capture time `offset`
            let ss = (first_t - offset + sync).max(0.0);
            args.extend(["-ss".into(), format!("{ss:.3}"), "-t".into(), format!("{:.3}", last_t - first_t), "-i".into(), w.display().to_string()]);
        }
        let n = wavs.len();
        let mix = if n == 1 { "[1:a]anull[a]".to_string() } else { format!("{}amix=inputs={n}:normalize=0[a]", (1..=n).map(|i| format!("[{i}:a]")).collect::<String>()) };
        args.extend(["-filter_complex".into(), mix, "-map".into(), "0:v".into(), "-map".into(), "[a]".into(), "-c:v".into(), "copy".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-shortest".into(), "-movflags".into(), "+faststart".into(), out.display().to_string()]);
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run(&ff, &a)?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(first_t)
}

fn run(ff: &Path, args: &[&str]) -> Result<()> {
    let o = Command::new(ff).args(["-hide_banner", "-v", "error", "-y"]).args(args).output()?;
    if !o.status.success() {
        bail!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""));
    }
    Ok(())
}

fn out_name(g: &Inner, kind: &str, game: &Option<String>) -> PathBuf {
    let s = g.settings.clone().unwrap_or_default();
    let ts = chrono_like(now_wall());
    s.recordings_folder.join(format!("{kind}_{}{ts}.mp4", game.as_ref().map(|x| format!("{x}_")).unwrap_or_default()))
}

fn chrono_like(secs: u64) -> String {
    // yyyy-mm-dd_hh-mm-ss (UTC) without a date crate
    let days = secs / 86400;
    let rem = secs % 86400;
    let (mut y, mut d) = (1970i64, days as i64);
    loop {
        let len = if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
        if d < len { break; }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let ml = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while d >= ml[m] { d -= ml[m]; m += 1; }
    format!("{y:04}-{:02}-{:02}_{:02}-{:02}-{:02}", m + 1, d + 1, rem / 3600, rem % 3600 / 60, rem % 60)
}

fn finish_recording(h: &RecorderHandle, out: PathBuf, game: Option<String>, bookmarks: Vec<f64>, kind: &str) {
    let mut sc: Sidecar = library::load_sidecar(&out);
    sc.game = game.clone();
    sc.bookmarks = bookmarks;
    sc.source = Some(kind.into());
    sc.imported_powder_metadata = true;
    let _ = library::save_sidecar(&out, &sc);
    let mut g = h.0.lock().unwrap();
    g.last_saved.insert(0, out.display().to_string());
    g.last_saved.truncate(10);
    // auto-analyse
    let auto = g.settings.as_ref().map(|s| s.auto_analyze).unwrap_or(false);
    if let (true, Some(game), Some(jobs), Some(ff), Some(models)) = (auto, game, g.jobs.clone(), g.ffmpeg.clone(), g.models.clone()) {
        let dirs = g.dirs.clone();
        let libs = g.libs.clone();
        jobs.spawn("analyze", &format!("Analyse {}", out.file_name().unwrap().to_string_lossy()), move |jh| {
            jh.progress(-1.0, "analysing new recording");
            let gp = crate::game::find_game(&game, &dirs)?;
            let opt = crate::analyze::AnalyzeOptions {
                ffmpeg: ff, lua_libs: libs, ocr_models: models, start: None, end: None,
                fps: Default::default(), fps_native_requested: false, skip_unchanged: true, max_height: Some(1440),
                debug_dir: None, debug_all: false, quiet: true, workers: None, progress: None,
            };
            let a = crate::analyze::analyze(&gp, &out, &opt)?;
            library::store_analysis(&out, &a)?;
            Ok(json!({"events": a.events.len()}))
        });
    }
}

/// Recorder actions (shared by the UI and hotkeys).
pub fn act(h: &RecorderHandle, action: &str, v: &Value) -> Result<Value> {
    match action {
        "buffer-start" => {
            let mut g = h.0.lock().unwrap();
            start_capture(&mut g)?;
        }
        "buffer-stop" => {
            let mut g = h.0.lock().unwrap();
            if g.session.is_some() { bail!("stop the recording first"); }
            stop_capture(&mut g);
        }
        "start" => {
            let mut g = h.0.lock().unwrap();
            if g.session.is_some() { return Ok(json!({"ok": true})); }
            start_capture(&mut g)?;
            let t = g.capture.as_ref().unwrap().started.elapsed().as_secs_f64();
            let game = v["game"].as_str().filter(|s| !s.is_empty()).map(String::from).or(g.detected_game.clone());
            g.session = Some(Session { start_t: t, bookmarks: vec![], game, started_wall: now_wall() });
        }
        "toggle" => {
            let recording = h.0.lock().unwrap().session.is_some();
            return act(h, if recording { "stop" } else { "start" }, v);
        }
        "bookmark" => {
            let mut g = h.0.lock().unwrap();
            let t = g.capture.as_ref().map(|c| c.started.elapsed().as_secs_f64()).context("not recording")?;
            match g.session.as_mut() {
                Some(s) => s.bookmarks.push(t),
                None => bail!("bookmarks need a running recording (or use Save replay)"),
            }
        }
        "stop" => {
            let (out, game, bms) = {
                let mut g = h.0.lock().unwrap();
                let s = g.session.take().context("not recording")?;
                let now = g.capture.as_ref().unwrap().started.elapsed().as_secs_f64();
                // wait for the current segment to close
                drop(g);
                std::thread::sleep(Duration::from_secs_f64(SEG + 0.3));
                let g = h.0.lock().unwrap();
                let out = out_name(&g, "Recording", &s.game);
                let first = assemble(&g, s.start_t, now, &out)?;
                let bms: Vec<f64> = s.bookmarks.iter().map(|b| b - first).collect();
                let replay_on = g.settings.as_ref().map(|x| x.recorder.replay_seconds > 0).unwrap_or(false);
                drop(g);
                if !replay_on {
                    stop_capture(&mut h.0.lock().unwrap());
                }
                (out, s.game, bms)
            };
            let o = out.clone();
            finish_recording(h, out, game, bms, "recorder");
            return Ok(json!({"saved": o}));
        }
        "save-replay" => {
            let secs = v["seconds"].as_f64();
            let g = h.0.lock().unwrap();
            let c = g.capture.as_ref().context("the replay buffer is not running")?;
            let now = c.started.elapsed().as_secs_f64();
            let n = secs.unwrap_or_else(|| g.settings.as_ref().map(|s| s.recorder.replay_seconds as f64).unwrap_or(60.0));
            drop(g);
            std::thread::sleep(Duration::from_secs_f64(SEG + 0.3));
            let g = h.0.lock().unwrap();
            let game = g.session.as_ref().and_then(|s| s.game.clone()).or(g.detected_game.clone());
            let out = out_name(&g, "Replay", &game);
            let first = assemble(&g, (now - n).max(0.0), now, &out)?;
            // bookmark the moment the replay was requested
            let bm = vec![(now - first - 1.0).max(0.0)];
            drop(g);
            let o = out.clone();
            finish_recording(h, out, game, bm, "replay");
            return Ok(json!({"saved": o}));
        }
        "set-game" => {
            let mut g = h.0.lock().unwrap();
            let game = v["game"].as_str().filter(|s| !s.is_empty()).map(String::from);
            if let Some(s) = g.session.as_mut() { s.game = game.clone(); }
            g.detected_game = game;
        }
        other => bail!("unknown recorder action '{other}'"),
    }
    Ok(json!({"ok": true}))
}

pub fn command(st: &mut crate::studio::State, action: &str, v: &Value) -> Result<Value> {
    sync_config(st);
    act(&st.recorder, action, v)
}

pub fn status(st: &crate::studio::State) -> Value {
    sync_config(st);
    let mut g = st.recorder.0.lock().unwrap();
    // ffmpeg died? surface its error
    let mut died = None;
    if let Some(c) = g.capture.as_mut() {
        if let Ok(Some(code)) = c.child.try_wait() {
            let mut err = String::new();
            if let Some(e) = c.child.stderr.as_mut() { use std::io::Read; let _ = e.read_to_string(&mut err); }
            died = Some(format!("capture stopped ({code}): {}", err.lines().last().unwrap_or("")));
        }
    }
    if let Some(d) = died {
        g.capture = None;
        g.session = None;
        g.last_error = Some(d);
    }
    prune(&g);
    let cap = g.capture.as_ref();
    json!({
        "capturing": cap.is_some(),
        "encoder": cap.map(|c| c.encoder.clone()),
        "source": cap.map(|c| c.source.clone()),
        "audio": cap.and_then(|c| c.audio.as_ref().map(|a| a.describe())),
        "bufferSeconds": cap.map(|c| c.started.elapsed().as_secs_f64().min(g.settings.as_ref().map(|s| s.recorder.replay_seconds as f64).unwrap_or(0.0))),
        "recording": g.session.is_some(),
        "recordingSeconds": g.session.as_ref().zip(cap).map(|(s, c)| c.started.elapsed().as_secs_f64() - s.start_t),
        "bookmarks": g.session.as_ref().map(|s| s.bookmarks.len()).unwrap_or(0),
        "game": g.session.as_ref().and_then(|s| s.game.clone()).or(g.detected_game.clone()),
        "startedAt": g.session.as_ref().map(|s| s.started_wall),
        "lastSaved": g.last_saved,
        "error": g.last_error,
    })
}

// ------------------------------------------------------------------ game detection

/// Running game, matched by executable name against each game's game.json `processNames`.
pub fn detect_game(dirs: &[PathBuf]) -> Option<String> {
    let games = crate::game::list_games(dirs);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let names: Vec<String> = sys.processes().values().map(|p| p.name().to_string_lossy().to_lowercase()).collect();
    for g in games {
        for pn in &g.meta.process_names {
            let pn = pn.to_lowercase();
            if names.iter().any(|n| *n == pn || n.trim_end_matches(".exe") == pn.trim_end_matches(".exe")) {
                return Some(g.id.clone());
            }
        }
    }
    None
}

/// Background thread: game detection + replay buffer autostart + segment pruning + hotkeys.
pub fn start_hotkeys(st: &crate::studio::State) {
    sync_config(st);
    let h = st.recorder.clone();
    let autostart = st.settings.recorder.replay_seconds > 0 && st.settings.recorder.replay_autostart;
    if autostart {
        let _ = act(&h, "buffer-start", &Value::Null);
    }
    let h2 = h.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let dirs = h2.0.lock().unwrap().dirs.clone();
        let det = detect_game(&dirs);
        let mut g = h2.0.lock().unwrap();
        if det.is_some() { g.detected_game = det; }
        prune(&g);
    });
    hotkeys::install(h, &st.settings.recorder);
}

pub fn restart_hotkeys(st: &crate::studio::State) {
    sync_config(st);
    hotkeys::install(st.recorder.clone(), &st.settings.recorder);
}

// ------------------------------------------------------------------ audio

#[cfg(windows)]
mod audio {
    use anyhow::{Context, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    type Writer = Arc<Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>>;

    struct Track {
        path: PathBuf,
        /// capture-timeline second at which the wav starts
        offset: Arc<Mutex<Option<f64>>>,
        writer: Writer,
        name: String,
    }

    pub struct AudioCapture {
        tracks: Vec<Track>,
        streams: Vec<cpal::Stream>,
    }

    // cpal::Stream is !Send on some platforms; we only move AudioCapture between threads while holding the recorder lock.
    unsafe impl Send for AudioCapture {}

    fn open(dev: &cpal::Device, cfg: cpal::SupportedStreamConfig, path: &Path, started: Instant, name: &str) -> Result<(Track, cpal::Stream)> {
        let spec = hound::WavSpec { channels: cfg.channels(), sample_rate: cfg.sample_rate().0, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let writer: Writer = Arc::new(Mutex::new(Some(hound::WavWriter::create(path, spec)?)));
        let offset = Arc::new(Mutex::new(None));
        let (w2, o2) = (writer.clone(), offset.clone());
        let err = |e| eprintln!("audio stream error: {e}");
        let sc: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_input_stream(&sc, move |d: &[f32], _: &_| {
                let mut o = o2.lock().unwrap();
                if o.is_none() { *o = Some(started.elapsed().as_secs_f64()); }
                if let Some(w) = w2.lock().unwrap().as_mut() { for s in d { let _ = w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16); } }
            }, err, None)?,
            cpal::SampleFormat::I16 => dev.build_input_stream(&sc, move |d: &[i16], _: &_| {
                let mut o = o2.lock().unwrap();
                if o.is_none() { *o = Some(started.elapsed().as_secs_f64()); }
                if let Some(w) = w2.lock().unwrap().as_mut() { for s in d { let _ = w.write_sample(*s); } }
            }, err, None)?,
            f => anyhow::bail!("unsupported audio sample format {f:?}"),
        };
        stream.play()?;
        Ok((Track { path: path.to_path_buf(), offset, writer, name: name.into() }, stream))
    }

    impl AudioCapture {
        pub fn start(dir: &Path, system: bool, mic: bool, started: Instant) -> Result<Self> {
            let host = cpal::default_host();
            let mut tracks = Vec::new();
            let mut streams = Vec::new();
            if system {
                // WASAPI loopback: an input stream opened on the default OUTPUT device
                let dev = host.default_output_device().context("no output device")?;
                let cfg = dev.default_output_config()?;
                let (t, s) = open(&dev, cfg, &dir.join("system.wav"), started, "system")?;
                tracks.push(t);
                streams.push(s);
            }
            if mic {
                if let Some(dev) = host.default_input_device() {
                    let cfg = dev.default_input_config()?;
                    let (t, s) = open(&dev, cfg, &dir.join("mic.wav"), started, "microphone")?;
                    tracks.push(t);
                    streams.push(s);
                }
            }
            Ok(Self { tracks, streams })
        }
        /// (wav path, capture-time offset) - flushes headers so ffmpeg can read the growing file
        pub fn files(&self) -> Vec<(PathBuf, f64)> {
            self.tracks
                .iter()
                .filter_map(|t| {
                    if let Some(w) = t.writer.lock().unwrap().as_mut() { let _ = w.flush(); }
                    t.offset.lock().unwrap().map(|o| (t.path.clone(), o))
                })
                .collect()
        }
        pub fn describe(&self) -> String {
            self.tracks.iter().map(|t| t.name.clone()).collect::<Vec<_>>().join(" + ")
        }
        pub fn stop(self) {
            drop(self.streams);
            for t in &self.tracks {
                if let Some(w) = t.writer.lock().unwrap().take() { let _ = w.finalize(); }
            }
        }
    }
}

#[cfg(not(windows))]
mod audio {
    use anyhow::Result;
    use std::path::{Path, PathBuf};
    use std::time::Instant;
    /// Audio capture is implemented for Windows (WASAPI). Elsewhere recordings are video-only.
    pub struct AudioCapture;
    impl AudioCapture {
        pub fn start(_dir: &Path, _system: bool, _mic: bool, _started: Instant) -> Result<Self> {
            anyhow::bail!("audio capture is only implemented on Windows")
        }
        pub fn files(&self) -> Vec<(PathBuf, f64)> { vec![] }
        pub fn describe(&self) -> String { String::new() }
        pub fn stop(self) {}
    }
}

// ------------------------------------------------------------------ hotkeys

#[cfg(windows)]
mod hotkeys {
    use super::RecorderHandle;
    use crate::settings::RecorderSettings;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static THREAD: AtomicU32 = AtomicU32::new(0);

    fn parse(s: &str) -> Option<(u32, u32)> {
        let mut mods = MOD_NOREPEAT;
        let mut vk = 0u32;
        for p in s.split('+').map(|p| p.trim().to_ascii_uppercase()) {
            match p.as_str() {
                "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
                "ALT" => mods |= MOD_ALT,
                "SHIFT" => mods |= MOD_SHIFT,
                "WIN" | "SUPER" => mods |= MOD_WIN,
                "INSERT" | "INS" => vk = VK_INSERT as u32,
                "HOME" => vk = VK_HOME as u32,
                "END" => vk = VK_END as u32,
                "PAGEUP" => vk = VK_PRIOR as u32,
                "PAGEDOWN" => vk = VK_NEXT as u32,
                "PAUSE" => vk = VK_PAUSE as u32,
                "SPACE" => vk = VK_SPACE as u32,
                k if k.len() >= 2 && k.starts_with('F') && k[1..].parse::<u32>().map(|n| (1..=24).contains(&n)).unwrap_or(false) => {
                    vk = VK_F1 as u32 + k[1..].parse::<u32>().unwrap() - 1
                }
                k if k.len() == 1 => vk = k.chars().next().unwrap() as u32,
                _ => return None,
            }
        }
        (vk != 0).then_some((mods, vk))
    }

    pub fn install(h: RecorderHandle, s: &RecorderSettings) {
        let old = THREAD.swap(0, Ordering::SeqCst);
        if old != 0 {
            unsafe { PostThreadMessageW(old, WM_QUIT, 0, 0) };
        }
        let keys = vec![(1i32, s.hotkey_toggle_record.clone(), "toggle"), (2, s.hotkey_bookmark.clone(), "bookmark"), (3, s.hotkey_save_replay.clone(), "save-replay")];
        std::thread::spawn(move || unsafe {
            THREAD.store(windows_sys::Win32::System::Threading::GetCurrentThreadId(), Ordering::SeqCst);
            for (id, k, _) in &keys {
                if let Some((m, vk)) = parse(k) {
                    if RegisterHotKey(std::ptr::null_mut(), *id, m, vk) == 0 {
                        eprintln!("hotkey {k} is already used by another program");
                    }
                }
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == WM_HOTKEY {
                    if let Some((_, _, action)) = keys.iter().find(|(id, _, _)| *id as usize == msg.wParam) {
                        let h2 = h.clone();
                        let a = action.to_string();
                        std::thread::spawn(move || {
                            if let Err(e) = super::act(&h2, &a, &serde_json::Value::Null) {
                                h2.0.lock().unwrap().last_error = Some(format!("{a}: {e:#}"));
                            }
                        });
                    }
                }
            }
            for (id, _, _) in &keys {
                UnregisterHotKey(std::ptr::null_mut(), *id);
            }
        });
    }
}

#[cfg(not(windows))]
mod hotkeys {
    use super::RecorderHandle;
    use crate::settings::RecorderSettings;
    /// Global hotkeys are implemented for Windows; elsewhere use the UI buttons.
    pub fn install(_h: RecorderHandle, _s: &RecorderSettings) {}
}
