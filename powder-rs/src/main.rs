#![allow(linker_messages)] // a harmless LIBCMT notice from the ONNX/C++ libraries on Windows
//! powder-rs: an open, customisable highlight-detection engine compatible with Powder's
//! game configs (events.json + game_postprocess.lua), plus tools to add new games.

mod analyze;
mod audio;
mod download;
mod app_api;
mod recorder;
mod encode;
mod jobs;
mod library;
mod montage;
mod music;
mod settings;
mod autogame;
mod discover;
mod game;
mod lua_engine;
mod ocr;
mod paths;
mod patcher;
mod report;
mod runtime;
mod studio;
mod tools;
mod video;
mod vision;
mod rwdmdl;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "powder-rs", version, about = "Open game-highlight detection engine (Powder-compatible) + custom game tooling")]
struct Cli {
    #[command(flatten)]
    common: Common,
    /// (no command = open the studio in your browser)
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Args, Clone)]
struct Common {
    /// Extra folder(s) containing game packs (repeatable). Defaults to ./games next to the exe.
    #[arg(long = "games", global = true)]
    games: Vec<PathBuf>,
    /// ffmpeg executable (Powder's ffmpeg.exe works)
    #[arg(long, global = true)]
    ffmpeg: Option<PathBuf>,
    /// onnxruntime library (onnxruntime.dll)
    #[arg(long, global = true)]
    onnxruntime: Option<PathBuf>,
    /// folder with det.onnx / rec.onnx / dict.txt
    #[arg(long = "ocr-models", global = true)]
    ocr_models: Option<PathBuf>,
    /// folder containing postprocess/*.lua (defaults to the bundled open implementation)
    #[arg(long = "lua-lib", global = true)]
    lua_lib: Option<PathBuf>,
    /// run models on the CPU instead of DirectML (Windows GPU)
    #[arg(long, global = true)]
    cpu: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// MODEL CREATION: save labelled frames from a video (events come from the game's test run on
    /// it, see add-game) into datasets/<ID>/<ID>_<event>/, then (re)train the game's .rwdmdl model
    ModelTrain {
        /// gameplay video already added with add-game / add-game --add-to (optional: retrain only)
        video: Option<PathBuf>,
        #[arg(long)]
        id: String,
        /// games folder (default: first --games folder or ./games)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// find events in a video with a game's .rwdmdl model (prints JSON)
    ModelDetect {
        video: PathBuf,
        #[arg(long)]
        model: PathBuf,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
    },
    /// Detect events in a video
    Analyze {
        /// game id (folder name) or path to a game folder
        game: String,
        video: PathBuf,
        /// write results JSON here (default: <video>.<GAME>.events.json)
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// also write an HTML report with the per-frame OCR timeline
        #[arg(long)]
        report: Option<PathBuf>,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
        #[command(flatten)]
        rate: RateArgs,
        /// downscale frames to this height before processing (default 1440)
        #[arg(long, default_value_t = 1440)]
        max_height: u32,
        /// save crop images (with detected text boxes) for crops marked debug=true
        #[arg(long)]
        debug_dir: Option<PathBuf>,
        /// with --debug-dir: save every crop, not only debug=true ones
        #[arg(long)]
        debug_all: bool,
        /// parallel workers (default: auto)
        #[arg(long)]
        workers: Option<usize>,
        /// OCR every crop on every frame even if its pixels did not change
        #[arg(long)]
        no_skip: bool,
    },
    /// ONE STEP: add support for a new game from a title + a gameplay video (fully automatic)
    AddGame {
        /// gameplay video that shows some kills / wins / other highlights on screen
        video: PathBuf,
        /// game title, e.g. "Delta Force" (optional if --steam is given)
        #[arg(long, short, default_value = "")]
        title: String,
        /// Steam store link or app id, e.g. https://store.steampowered.com/app/2507950/Delta_Force/
        #[arg(long)]
        steam: Option<String>,
        /// optional game install folder - its text/localisation files are searched for event names
        #[arg(long = "game-files")]
        game_files: Option<PathBuf>,
        #[command(flatten)]
        rate: RateArgs,
        /// scanning rate for finding text (default 2 fps; analysis uses --fps)
        #[arg(long = "scan-fps", default_value_t = 2.0)]
        scan_fps: f64,
        /// don't install the result into the original Powder app (by default it is installed when Powder is found)
        #[arg(long = "no-install")]
        no_install: bool,
        /// Powder folder (default: %LOCALAPPDATA%\Programs\powder-desktop)
        #[arg(long)]
        powder: Option<PathBuf>,
        /// don't contact Steam for the store id / artwork
        #[arg(long)]
        offline: bool,
        /// where to create the game (default: first --games folder or ./games)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// replace an existing game with the same id
        #[arg(long)]
        overwrite: bool,
        /// reuse the previous text scan of the same video (fast re-runs)
        #[arg(long)]
        reuse_scan: bool,
        /// improve an existing game (its id) with this video instead of creating a new game
        #[arg(long = "add-to")]
        add_to: Option<String>,
    },
    /// Auto mode step 1: read ALL on-screen text in a video and group repeats
    Discover {
        video: PathBuf,
        /// output folder (frames + discovery.json)
        #[arg(long, short, default_value = "discovery")]
        out: PathBuf,
        #[command(flatten)]
        rate: RateArgs,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
        /// how many groups to print
        #[arg(long, default_value_t = 30)]
        top: usize,
    },
    /// Auto mode step 2: build a game pack from discovered text, e.g. --rule kill=ELIMINATED --rule victory=VICTORY
    AutoBuild {
        /// discovery.json (or its folder)
        discovery: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
        /// EVENT=WORD[,WORD...]  (words are matched against discovered groups).
        /// Without any: decide automatically, like add-game (rebuilds a game from a saved scan)
        #[arg(long = "rule")]
        rules: Vec<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        overwrite: bool,
    },
    /// Re-run a game's Lua on the OCR/cue data saved by `analyze` (fast iteration on detection logic)
    Replay {
        game: String,
        /// a *.events.json written by analyze
        results: PathBuf,
        #[arg(long, short)]
        out: Option<PathBuf>,
        #[arg(long)]
        report: Option<PathBuf>,
        /// print events as JSON only
        #[arg(long)]
        json: bool,
    },
    /// Make an auto-montage from analysed videos (Powder-style effects, music, vertical)
    Montage {
        /// videos (analysed with `analyze`, or Powder recordings with events)
        #[arg(required = true)]
        videos: Vec<PathBuf>,
        #[arg(long, short)]
        out: PathBuf,
        /// target length in seconds (default: music length or 60)
        #[arg(long)]
        target: Option<f64>,
        /// music file, or "powder:N" for Powder's bundled track N (see `music`)
        #[arg(long)]
        music: Option<String>,
        #[arg(long, default_value_t = 0.0)]
        music_start: f64,
        /// 9:16 vertical output (TikTok / Shorts)
        #[arg(long)]
        vertical: bool,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// no slow-mo / zoom / shake effects
        #[arg(long)]
        no_effects: bool,
        #[arg(long)]
        outro: Option<PathBuf>,
        /// effect bundles file (Powder's automatic_montage/config.json format)
        #[arg(long)]
        effects_file: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// only print the plan
        #[arg(long)]
        dry_run: bool,
    },
    /// Export a clip (frame-accurate) from a video
    Clip {
        video: PathBuf,
        #[arg(long)]
        start: f64,
        #[arg(long)]
        end: f64,
        #[arg(long, short)]
        out: PathBuf,
        #[arg(long)]
        vertical: bool,
    },
    /// Find loudness spikes (shouts, explosions) in a video's audio
    AudioPeaks {
        video: PathBuf,
        /// dB above the surrounding level (lower = more events)
        #[arg(long, default_value_t = 10.0)]
        sensitivity: f32,
    },
    /// Transcribe a video (needs a build with --features transcribe) and optionally find keywords
    Transcribe {
        video: PathBuf,
        #[arg(long, default_value = "auto")]
        language: String,
        /// comma-separated keywords to turn into events
        #[arg(long)]
        keywords: Option<String>,
        /// whisper ggml model (default: models/whisper/*.bin or Powder's ggml-base.bin)
        #[arg(long)]
        model: Option<PathBuf>,
    },
    /// Download a VOD/clip (Twitch, YouTube, Kick ...) with yt-dlp, plus chat spikes when available
    Download { url: String, #[arg(long, short)] out: Option<PathBuf> },
    /// List montage music (Powder's bundled tracks + your music folders)
    Music,
    /// Draw a game's crops on a screenshot/frame and OCR them (for tuning cropCoords)
    Calibrate {
        game: String,
        /// screenshot (png/jpg) or video
        input: PathBuf,
        /// time in seconds when input is a video
        #[arg(long, short, default_value_t = 0.0)]
        time: f64,
        #[arg(long, short, default_value = "calibration")]
        out: PathBuf,
    },
    /// OCR one region of a screenshot/frame without writing any game files
    OcrTest {
        input: PathBuf,
        /// left,top,right,bottom as fractions, e.g. 0.35,0.62,0.65,0.70
        #[arg(long)]
        crop: String,
        #[arg(long, short, default_value_t = 0.0)]
        time: f64,
        #[arg(long, default_value_t = 5)]
        dilate: i32,
        #[arg(long, default_value_t = 50)]
        min_area: i32,
        #[arg(long, default_value_t = 10)]
        margin: i32,
        #[arg(long, default_value = "")]
        chars: String,
        /// save the crop with text boxes to this png
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Create a new game pack from the template
    NewGame {
        /// short id, e.g. DF, ARCRDR, MARVELR
        id: String,
        /// display name, e.g. "Delta Force"
        #[arg(long)]
        name: String,
        #[arg(long)]
        steam_id: Option<String>,
        #[arg(long)]
        twitch_id: Option<String>,
        /// where to create it (default: first --games folder or ./games)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Check a game pack for common mistakes
    Validate { game: String },
    /// List available games
    List,
    /// Save frames (or one crop of them) as images, for labelling / model training
    ExtractFrames {
        video: PathBuf,
        #[arg(long, short)]
        out: PathBuf,
        #[arg(long, default_value_t = 2.0)]
        fps: f64,
        /// optional l,t,r,b crop
        #[arg(long)]
        crop: Option<String>,
        /// resize to WxH, e.g. 224x224
        #[arg(long)]
        size: Option<String>,
        #[arg(long)]
        start: Option<f64>,
        #[arg(long)]
        end: Option<f64>,
    },
    /// Open the app: library, recorder, montage, games, settings (same as running with no command)
    Studio {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        /// open this game in the studio
        game: Option<String>,
    },
    /// Install / remove custom games in the original Powder desktop app
    Powder {
        #[command(subcommand)]
        action: patcher::PowderCmd,
    },
}

/// Frame-rate selection shared by analyze / discover / studio.
#[derive(Args, Clone, Debug)]
struct RateArgs {
    /// analysis rate: a number (1, 4, 10, 30, 60, 120...) or "native" for every recorded frame.
    /// Default: the game's own rate (analyze) or 2 (discover).
    #[arg(long)]
    fps: Option<String>,
    /// recording frame rate (overrides what the file reports), e.g. 60, 120, 144, 240
    #[arg(long = "recording-fps")]
    recording_fps: Option<f64>,
    /// average in-game fps; sampling never goes above it (higher frames are duplicates)
    #[arg(long = "game-fps")]
    game_fps: Option<f64>,
}

impl RateArgs {
    fn settings(&self) -> Result<(video::FpsSettings, bool)> {
        let (sample, native) = match &self.fps {
            Some(s) => {
                let v = video::FpsSettings::parse_sample(s)?;
                (v, v.is_none())
            }
            None => (None, false),
        };
        Ok((video::FpsSettings { sample, recording: self.recording_fps, game: self.game_fps }, native))
    }
}

struct Ctx {
    c: Common,
}

impl Ctx {
    fn game_dirs(&self) -> Vec<PathBuf> {
        let mut v = self.c.games.clone();
        v.extend(paths::default_game_dirs());
        v
    }
    fn ffmpeg(&self) -> Result<PathBuf> {
        paths::ffmpeg(self.c.ffmpeg.as_deref())
    }
    fn lua_libs(&self) -> Result<Vec<PathBuf>> {
        Ok(vec![paths::lua_lib_dir(self.c.lua_lib.as_deref())?])
    }
    fn init_ort(&self) -> Result<()> {
        let p = paths::onnxruntime(self.c.onnxruntime.as_deref())?;
        runtime::init(&p, !self.c.cpu)
    }
    fn ocr(&self) -> Result<ocr::Ocr> {
        self.init_ort()?;
        ocr::Ocr::load(&paths::ocr_models_dir(self.c.ocr_models.as_deref())?)
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx { c: cli.common.clone() };
    let cmd = cli.cmd.unwrap_or(Cmd::Studio { port: 7878, game: None });
    match cmd {
        Cmd::Analyze { game, video, out, report, start, end, rate, max_height, debug_dir, debug_all, workers, no_skip } => {
            let (fps, native) = rate.settings()?;
            let g = game::find_game(&game, &ctx.game_dirs())?;
            ctx.init_ort()?;
            let opt = analyze::AnalyzeOptions {
                ffmpeg: ctx.ffmpeg()?,
                lua_libs: ctx.lua_libs()?,
                ocr_models: paths::ocr_models_dir(ctx.c.ocr_models.as_deref())?,
                start,
                end,
                fps,
                fps_native_requested: native,
                skip_unchanged: !no_skip,
                max_height: Some(max_height),
                debug_dir,
                debug_all,
                quiet: false,
                workers,
                progress: None,
            };
            let a = analyze::analyze(&g, &video, &opt)?;
            for w in &a.warnings {
                eprintln!("warning: {w}");
            }
            println!("{} event(s):", a.events.len());
            for e in &a.events {
                println!("  {:>10}  {}{}", e.timestamp, e.display_name, if e.known { "" } else { "  (not in events.json!)" });
            }
            let out = out.unwrap_or_else(|| {
                let mut p = video.clone().into_os_string();
                p.push(format!(".{}.events.json", g.id));
                PathBuf::from(p)
            });
            std::fs::write(&out, serde_json::to_string_pretty(&a)?)?;
            let _ = library::store_analysis(&video, &a);
            println!("results: {}", out.display());
            if let Some(r) = report {
                std::fs::write(&r, report::html(&a))?;
                println!("report:  {}", r.display());
            }
        }
        Cmd::ModelTrain { video, id, dir } => {
            let games_dir = dir.or_else(|| ctx.c.games.first().cloned()).unwrap_or_else(|| paths::exe_dir().join("games"));
            let gdir = games_dir.join(&id);
            if !gdir.join("game.json").exists() {
                bail!("game {id} doesn't exist yet - add it with add-game first");
            }
            let ds = rwdmdl::dataset_dir(&games_dir, &id);
            std::fs::create_dir_all(&ds)?;
            let test_path = games_dir.join("_work").join(&id).join("test.events.json");
            if let Some(v) = &video {
                let test: serde_json::Value = serde_json::from_slice(&std::fs::read(&test_path).context("no test run for this game - run add-game on the video first")?)?;
                let same = test["video"].as_str().map(|p| Path::new(p).file_name() == v.file_name()).unwrap_or(false);
                if !same {
                    bail!("the game's last test run was on another video - run add-game --add-to {id} on this video first");
                }
                let evs: Vec<(String, f64)> = test["events"].as_array().into_iter().flatten().filter(|e| e["from_model"] != true).filter_map(|e| Some((e["name"].as_str()?.to_string(), e["time_sec"].as_f64()?))).collect();
                eprintln!("[model] saving pictures of {} event(s) from the video", evs.len());
                let (p, n) = rwdmdl::extract(&ctx.ffmpeg()?, v, &id, &evs, &ds)?;
                println!("saved {p} event picture(s) and {n} \"nothing\" picture(s) to {}", ds.display());
            }
            let m = rwdmdl::train(&id, &ds)?;
            let out = gdir.join(format!("{id}.rwdmdl"));
            m.save(&out)?;
            println!("model trained on {} picture(s): {}", m.meta.trained_images, out.display());
            for (c, a) in &m.meta.accuracy {
                println!("  {c:<18} {:.0}% right on held-back pictures", a * 100.0);
            }
            // add what the model finds in this video to its test run (used to mark the video analysed)
            if let Some(v) = &video {
                let found = rwdmdl::detect(&ctx.ffmpeg()?, v, rwdmdl::Model::load(&out)?, None, None)?;
                let mut test: serde_json::Value = serde_json::from_slice(&std::fs::read(&test_path)?)?;
                let mut evs = test["events"].as_array().cloned().unwrap_or_default();
                let mut added = 0;
                for (name, t) in found {
                    if evs.iter().any(|e| e["name"] == name.as_str() && (e["time_sec"].as_f64().unwrap_or(-99.0) - t).abs() < 3.0) {
                        continue;
                    }
                    added += 1;
                    evs.push(serde_json::json!({"name": name, "display_name": name, "time_sec": t, "timestamp": analyze::fmt_ts(t), "known": true, "from_model": true}));
                }
                evs.sort_by(|a, b| a["time_sec"].as_f64().unwrap_or(0.0).total_cmp(&b["time_sec"].as_f64().unwrap_or(0.0)));
                test["events"] = serde_json::json!(evs);
                std::fs::write(&test_path, serde_json::to_vec_pretty(&test)?)?;
                println!("the model found {added} extra event(s) the text reading missed in this video");
            }
        }
        Cmd::ModelDetect { video, model, start, end } => {
            let found = rwdmdl::detect(&ctx.ffmpeg()?, &video, rwdmdl::Model::load(&model)?, start, end)?;
            let v: Vec<_> = found.iter().map(|(n, t)| serde_json::json!({"name": n, "time_sec": t})).collect();
            println!("{}", serde_json::to_string(&v)?);
        }
        Cmd::AddGame { title, steam, video, game_files, rate, scan_fps, no_install, powder, offline, dir, overwrite, reuse_scan, add_to } => {
            ctx.init_ort()?;
            let (fps, _) = rate.settings()?;
            let games_dir = dir.or_else(|| ctx.c.games.first().cloned()).unwrap_or_else(|| paths::exe_dir().join("games"));
            std::fs::create_dir_all(&games_dir)?;
            if title.trim().is_empty() && steam.is_none() && add_to.is_none() {
                bail!("give --title \"Game Name\" and/or --steam <store link>");
            }
            let o = autogame::AddGameOptions {
                title,
                steam,
                video,
                games_dir: games_dir.clone(),
                game_files,
                ffmpeg: ctx.ffmpeg()?,
                ocr_models: paths::ocr_models_dir(ctx.c.ocr_models.as_deref())?,
                lua_libs: ctx.lua_libs()?,
                fps,
                scan_fps_default: scan_fps,
                online: !offline,
                install_powder: if no_install {
                    None
                } else {
                    match patcher::powder_dir(powder.clone()) {
                        Ok(p) => Some(p),
                        Err(_) => {
                            eprintln!("note: Powder not found, so the game won't be installed into it (use --powder <folder> to point at it)");
                            None
                        }
                    }
                },
                work_dir: games_dir.join("_work"),
                overwrite: overwrite || add_to.is_some(),
                reuse_scan,
                add_to,
            };
            let r = autogame::add_game(&o, None)?;
            println!("\nadded {} ({}) -> {}", r.title, r.id, r.dir);
            println!("events it will detect:");
            for d in &r.decisions {
                println!("  {:<16} from \"{}\" ({}x)  - {}", d.event, d.word, d.count, d.reason);
            }
            println!("test run on your video found {} event(s):", r.test_events.len());
            for e in &r.test_events {
                println!("  {:>10}  {}", e.timestamp, e.display_name);
            }
            for w in &r.warnings { println!("  note: {w}"); }
            if let Some(p) = &r.installed_into_powder { println!("installed into Powder at {p} - restart Powder."); }
        }
        Cmd::Discover { video, out, rate, start, end, top } => {
            ctx.init_ort()?;
            let (mut fps, _) = rate.settings()?;
            if rate.fps.is_none() { fps.sample = Some(2.0); }
            let ff = ctx.ffmpeg()?;
            let info = video::probe(&ff, &video)?;
            let (eff, why) = fps.effective(&info, 2.0);
            eprintln!("[powder-rs] discover {} | {}", video.display(), why);
            let opt = discover::DiscoverOptions { ffmpeg: ff, ocr_models: paths::ocr_models_dir(ctx.c.ocr_models.as_deref())?, fps: eff, start, end, out_dir: out.clone() };
            let prog: discover::SharedProgress = Default::default();
            let p2 = prog.clone();
            let h = std::thread::spawn(move || discover::discover(&video, &opt, Some(p2)));
            while !h.is_finished() {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let p = prog.lock().unwrap().clone();
                eprint!("\r  {} {}/{}   ", p.state, p.done, p.total);
            }
            let d = h.join().map_err(|_| anyhow::anyhow!("discover thread panicked"))??;
            eprintln!();
            println!("{} frames, {} text detections, {} groups (static HUD marked *):", d.frame_times.len(), d.detections.len(), d.groups.len());
            for g in d.groups.iter().take(top) {
                println!(
                    "  #{:<3} {:<16} x{:<4} at ({:.2},{:.2})-({:.2},{:.2}){}{}   e.g. \"{}\"",
                    g.id, g.word, g.count, g.bbox[0], g.bbox[1], g.bbox[2], g.bbox[3],
                    if g.is_static { " *" } else { "" },
                    g.suggestion.as_ref().map(|s| format!("  -> {s}?")).unwrap_or_default(),
                    g.examples.first().cloned().unwrap_or_default()
                );
            }
            println!("saved {}", out.join("discovery.json").display());
        }
        Cmd::AutoBuild { discovery, id, name, rules, dir, overwrite } => {
            let p = if discovery.is_dir() { discovery.join("discovery.json") } else { discovery };
            let d = discover::load(&p)?;
            let mut rs = Vec::new();
            if rules.is_empty() {
                let all = autogame::classify(&d, &std::collections::BTreeMap::new(), 6);
                let (known, custom): (Vec<_>, Vec<_>) = all.into_iter().partition(|x: &autogame::Decision| !x.reason.starts_with("highlight-like"));
                for x in &known {
                    println!("  {:<14} {:<14} {:>4}x  {}", x.event, x.word, x.count, x.reason);
                }
                println!("  suggestions only: {}", custom.iter().map(|x| x.word.clone()).collect::<Vec<_>>().join(", "));
                rs = autogame::rules_from(&known, &d);
            }
            for r in rules {
                let (ev, words) = r.split_once('=').context("rule must be EVENT=WORD[,WORD]")?;
                let words: Vec<String> = words.split(',').map(|w| w.trim().to_uppercase()).filter(|w| !w.is_empty()).collect();
                let groups: Vec<usize> = d.groups.iter().filter(|g| !g.is_static && words.iter().any(|w| w.split_whitespace().any(|p| p == g.word))).map(|g| g.id).collect();
                if groups.is_empty() {
                    bail!("no discovered (non-static) text matches {:?} for event {ev}", words);
                }
                rs.push(discover::Rule { event: ev.trim().to_string(), display_name: None, groups, words });
            }
            let dir = dir.or_else(|| ctx.c.games.first().cloned()).unwrap_or_else(|| PathBuf::from("games"));
            let g = discover::build_game(&d, &rs, &dir, &id, &name, overwrite)?;
            println!("built {}", g.display());
            let libs = ctx.lua_libs()?;
            let libs: Vec<&Path> = libs.iter().map(|p| p.as_path()).collect();
            let rep = tools::validate(&game::GamePack::load(&g)?, &libs);
            for e in &rep.errors { println!("  ERROR: {e}"); }
            for w in &rep.warnings { println!("  warning: {w}"); }
        }
        Cmd::Montage { videos, out, target, music, music_start, vertical, height, fps, no_effects, outro, effects_file, seed, dry_run } => {
            let ff = ctx.ffmpeg()?;
            let st = settings::Settings::load();
            let mut sources = Vec::new();
            for v in &videos {
                let mut sc = library::load_sidecar(v);
                // fall back to a results file written by `analyze`
                if sc.events.is_empty() {
                    let dir = v.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
                    let stem = v.file_name().unwrap().to_string_lossy().to_string();
                    if let Ok(rd) = std::fs::read_dir(dir) {
                        for e in rd.flatten() {
                            let n = e.file_name().to_string_lossy().to_string();
                            if n.starts_with(&stem) && n.ends_with(".events.json") {
                                let j = library::read_json(&e.path())?;
                                sc.game = j["game"].as_str().map(String::from);
                                for ev in j["events"].as_array().cloned().unwrap_or_default() {
                                    sc.events.push(library::LibEvent { name: ev["name"].as_str().unwrap_or("").into(), display_name: ev["display_name"].as_str().unwrap_or("").into(), time_sec: ev["time_sec"].as_f64().unwrap_or(0.0), source: "video".into() });
                                }
                            }
                        }
                    }
                }
                for b in &sc.bookmarks {
                    sc.events.push(library::LibEvent { name: "bookmark".into(), display_name: "Bookmark".into(), time_sec: *b, source: "bookmark".into() });
                }
                let events_json = sc.game.as_ref().and_then(|g| game::find_game(g, &ctx.game_dirs()).ok()).map(|g| g.events);
                let (duration, has_audio, w, h) = montage::probe_source(&ff, v)?;
                println!("{}: {} event(s){}", v.display(), sc.events.len(), sc.game.as_ref().map(|g| format!(" [{g}]")).unwrap_or_default());
                sources.push(montage::Source { video: v.clone(), events: sc.events, events_json, duration, has_audio, width: w, height: h });
            }
            let tracks = music::list(&ff, st.powder_dir.as_deref().or(patcher::powder_dir(None).ok().as_deref()), &st.music_folders);
            let music_path = match music.as_deref() {
                Some(m) if m.starts_with("powder:") => {
                    let i: usize = m[7..].parse().context("powder:N")?;
                    Some(tracks.iter().filter(|t| t.source == "powder").nth(i).context("no such Powder track (see `powder-rs music`)")?.path.clone())
                }
                Some(m) => Some(PathBuf::from(m)),
                None => None,
            };
            let music_len = music_path.as_ref().and_then(|m| montage::probe_source(&ff, m).ok()).map(|x| x.0 - music_start);
            let opt = montage::MontageOptions {
                target_sec: target.or(music_len).unwrap_or(60.0),
                music: music_path,
                music_start,
                vertical,
                height,
                fps,
                effects: !no_effects,
                outro,
                seed,
                ..Default::default()
            };
            let bundles = montage::load_effects(effects_file.as_deref());
            let plan = montage::plan(&sources, &opt, &bundles);
            println!("plan ({:.0}s target):", opt.target_sec);
            for c in &plan {
                println!("  {} {:>9}-{:<9} {:<14} score {:>4.0} {}", c.source, analyze::fmt_ts(c.start), analyze::fmt_ts(c.end), c.event_name, c.score, c.effect.clone().unwrap_or_default());
            }
            if !dry_run {
                let r = montage::make(&ff, &sources, &opt, &bundles, &out, &|p, m| eprint!("\r  {:>3.0}% {m}                    ", p * 100.0))?;
                eprintln!();
                println!("montage: {} ({:.1}s, encoder {})", out.display(), r["duration"].as_f64().unwrap_or(0.0), r["encoder"].as_str().unwrap_or(""));
            }
        }
        Cmd::Clip { video, start, end, out, vertical } => {
            let ff = ctx.ffmpeg()?;
            let st = settings::Settings::load();
            montage::export_clip(&ff, &video, start, end, &out, &st.recorder.encoder, "high", vertical.then_some([0.34197530864, 0.0, 0.31604938271, 1.0]))?;
            println!("clip: {}", out.display());
        }
        Cmd::AudioPeaks { video, sensitivity } => {
            let s = audio::decode(&ctx.ffmpeg()?, &video, 16000)?;
            let p = audio::loudness_peaks(&s, 16000, sensitivity);
            let items: Vec<(f64, String)> = p.iter().map(|t| (*t, String::new())).collect();
            library::set_events(&video, "audio", "loudness-peak", "Loudness spike", &items)?;
            println!("{} loudness spike(s):", p.len());
            for t in p { println!("  {}", analyze::fmt_ts(t)); }
        }
        Cmd::Transcribe { video, language, keywords, model } => {
            let st = settings::Settings::load();
            let pd = st.powder_dir.clone().or(patcher::powder_dir(None).ok());
            let model = model.or_else(|| audio::find_model(pd.as_deref())).context("no whisper model found (pass --model)")?;
            let lines = audio::transcribe(&ctx.ffmpeg()?, &video, &model, &language, &|p| eprint!("\r  {:.0}%   ", p * 100.0))?;
            eprintln!();
            for l in &lines { println!("[{} -> {}] {}", analyze::fmt_ts(l.from), analyze::fmt_ts(l.to), l.text); }
            let mut sc = library::load_sidecar(&video);
            sc.transcript = lines.clone();
            library::save_sidecar(&video, &sc)?;
            if let Some(k) = keywords {
                let kws: Vec<String> = k.split(',').map(|s| s.trim().to_string()).collect();
                let hits = audio::keyword_hits(&lines, &kws);
                library::set_events(&video, "keyword", "keyword", "Keyword", &hits)?;
                println!("{} keyword hit(s)", hits.len());
            }
        }
        Cmd::Download { url, out } => {
            let st = settings::Settings::load();
            let pd = st.powder_dir.clone().or(patcher::powder_dir(None).ok());
            let yt = download::ytdlp(pd.as_deref())?;
            let folder = out.unwrap_or_else(|| st.library_folders.first().cloned().unwrap_or(st.recordings_folder.clone()).join("Downloads"));
            let (v, chat) = download::download(&yt, &ctx.ffmpeg()?, &url, &folder, &|p, _| eprint!("\r  {:.0}%   ", p * 100.0))?;
            eprintln!();
            println!("saved {}", v.display());
            if let Some(c) = chat {
                let sp = download::chat_spikes(&download::chat_times(&c)?);
                let items: Vec<(f64, String)> = sp.iter().map(|t| (*t, String::new())).collect();
                library::set_events(&v, "chat", "chat-event", "Chat spike", &items)?;
                println!("{} chat spike(s)", sp.len());
            }
        }
        Cmd::Music => {
            let ff = ctx.ffmpeg()?;
            let st = settings::Settings::load();
            let tracks = music::list(&ff, st.powder_dir.as_deref().or(patcher::powder_dir(None).ok().as_deref()), &st.music_folders);
            let mut pi = 0;
            for t in &tracks {
                let id = if t.source == "powder" { pi += 1; format!("powder:{}", pi - 1) } else { t.path.display().to_string() };
                println!("  {:<30} {:<24} {:<20} {:>5.0}s", id, t.title, t.artist, t.duration);
            }
            if tracks.is_empty() { println!("no music found (set music folders in settings, or point POWDER_DIR at your Powder install)"); }
        }
        Cmd::Replay { game, results, out, report, json } => {
            let g = game::find_game(&game, &ctx.game_dirs())?;
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&results)?).context("reading results json")?;
            let a = analyze::replay(&g, &v, &ctx.lua_libs()?)?;
            if json {
                let ev: Vec<_> = a.events.iter().map(|e| serde_json::json!({"name": e.name, "time_sec": e.time_sec})).collect();
                println!("{}", serde_json::to_string(&ev)?);
            } else {
                println!("{} event(s):", a.events.len());
                for e in &a.events {
                    println!("  {:>10}  {}{}", e.timestamp, e.display_name, if e.known { "" } else { "  (not in events.json!)" });
                }
            }
            if let Some(o) = out { std::fs::write(&o, serde_json::to_string_pretty(&a)?)?; }
            if let Some(r) = report { std::fs::write(&r, report::html(&a))?; }
        }
        Cmd::Calibrate { game, input, time, out } => {
            let g = game::find_game(&game, &ctx.game_dirs())?;
            let libs = ctx.lua_libs()?;
            let libs: Vec<&Path> = libs.iter().map(|p| p.as_path()).collect();
            let script = lua_engine::GameScript::load(&g.dir, &libs)?;
            let frame = video::load_image_or_frame(ctx.c.ffmpeg.as_ref(), &input, time)?;
            let mut o = ctx.ocr().ok();
            if o.is_none() {
                eprintln!("note: OCR unavailable, only saving crop images");
            }
            let res = tools::calibrate(&script, o.as_mut(), &frame, &out)?;
            println!("frame {}x{}  ->  {}", frame.width(), frame.height(), out.display());
            for (n, t) in res {
                println!("  {n:<20} {}", if t.is_empty() { "(no text)".into() } else { format!("\"{t}\"") });
            }
            println!("open {} to see every crop drawn on the frame", out.join("_overview.png").display());
        }
        Cmd::OcrTest { input, crop, time, dilate, min_area, margin, chars, save } => {
            let frame = video::load_image_or_frame(ctx.c.ffmpeg.as_ref(), &input, time)?;
            let spec = tools::adhoc_crop("test", tools::parse_crop(&crop)?, dilate, min_area, margin, &chars);
            let mut o = ctx.ocr()?;
            let r = o.run_crop(&frame, &spec)?;
            println!("text: \"{}\"", r.text);
            for b in &r.boxes {
                println!("  box ({},{})-({},{}) \"{}\"", b.x0, b.y0, b.x1, b.y1, b.text);
            }
            if let Some(s) = save {
                ocr::debug_image(&frame, &spec, &r).save(&s)?;
                println!("saved {}", s.display());
            }
        }
        Cmd::NewGame { id, name, steam_id, twitch_id, dir } => {
            let dir = dir.or_else(|| ctx.c.games.first().cloned()).unwrap_or_else(|| PathBuf::from("games"));
            let g = tools::new_game(&dir, &id, &name, steam_id, twitch_id)?;
            println!("created {}", g.display());
            println!("next: 1) take a screenshot showing a kill/victory  2) powder-rs studio {id}  (or edit cropCoords by hand)");
            println!("      3) powder-rs analyze {id} some_gameplay.mp4 --report report.html");
        }
        Cmd::Validate { game } => {
            let g = game::find_game(&game, &ctx.game_dirs())?;
            let libs = ctx.lua_libs()?;
            let libs: Vec<&Path> = libs.iter().map(|p| p.as_path()).collect();
            let r = tools::validate(&g, &libs);
            for i in &r.info { println!("  info:    {i}"); }
            for w in &r.warnings { println!("  warning: {w}"); }
            for e in &r.errors { println!("  ERROR:   {e}"); }
            if r.errors.is_empty() {
                println!("{}: OK ({} warning(s))", g.id, r.warnings.len());
            } else {
                bail!("{}: {} error(s)", g.id, r.errors.len());
            }
        }
        Cmd::List => {
            let dirs = ctx.game_dirs();
            let games = game::list_games(&dirs);
            if games.is_empty() {
                println!("no games found in {:?}", dirs);
            }
            for g in games {
                let model = if g.dir.join("model.json").exists() { "model+ocr" } else if g.dir.join("model.pwdmdl").exists() { "ocr (encrypted model ignored)" } else { "ocr" };
                println!("  {:<10} {:<28} {:>2} events  {}  [{}]", g.id, g.meta.name, g.event_names().len(), model, g.dir.display());
            }
        }
        Cmd::ExtractFrames { video, out, fps, crop, size, start, end } => {
            let crop = crop.map(|c| tools::parse_crop(&c)).transpose()?;
            let size = size
                .map(|s| -> Result<(u32, u32)> {
                    let (a, b) = s.split_once('x').context("size must be WxH")?;
                    Ok((a.parse()?, b.parse()?))
                })
                .transpose()?;
            let n = tools::extract_frames(&ctx.ffmpeg()?, &video, &out, fps, crop, size, start, end)?;
            println!("saved {n} frames to {}", out.display());
        }
        Cmd::Studio { port, game } => {
            let dirs = ctx.game_dirs();
            let ocr = ctx.ocr().map_err(|e| eprintln!("note: OCR unavailable in studio: {e:#}")).ok();
            let games_dir = ctx.c.games.first().cloned().unwrap_or_else(|| paths::exe_dir().join("games"));
            std::fs::create_dir_all(&games_dir)?;
            let cfg = studio::StudioConfig { dirs, libs: ctx.lua_libs()?, ffmpeg: ctx.c.ffmpeg.clone(), ocr_models: ctx.c.ocr_models.clone(), games_dir };
            studio::serve(port, cfg, ocr, game)?;
        }
        Cmd::Powder { action } => patcher::run(action, &ctx.game_dirs(), &ctx.lua_libs()?)?,
    }
    Ok(())
}
