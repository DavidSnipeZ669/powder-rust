//! First-run setup, so Rewind can be shared as a single Rewind.exe.
//!
//! Everything Rewind needs ends up in `%LOCALAPPDATA%\Rewind`:
//!   base\    Powder's own files (interface, recorder/AI engine, ai-configs, game art), copied from the
//!            user's own Powder install (the standalone installer's folder or the free Steam version)
//!   engine\  the custom-game engine (unpacked from this exe) + ONNX Runtime/DirectML (Microsoft's NuGet
//!            packages) + PaddleOCR text models (Hugging Face)
//!   music\   the user's own tracks
//! The exe copies itself to `%LOCALAPPDATA%\Programs\Rewind`, adds Start menu/desktop shortcuts and an
//! "Apps & features" uninstall entry. Rewind never redistributes Powder's files: each user's copy comes
//! from their own Powder install.
use crate::http::Request;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const POWDER_STEAM_APP: &str = "1794970";
const ORT_VERSION: &str = "1.24.4";
const DML_VERSION: &str = "1.15.4";
const OCR_BASE: &str = "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4";

/// The engine pack (powder-rs.exe, lua, games, OCR dictionary), zipped by build.rs.
static ENGINE_PACK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/engine-pack.zip"));

/// Where the user chose to install Rewind is remembered in `%LOCALAPPDATA%\Rewind.location`;
/// without it, `%LOCALAPPDATA%\Rewind`.
fn location_file() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Rewind.location")
}
pub fn default_home() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Rewind")
}
pub fn home() -> PathBuf {
    std::fs::read_to_string(location_file()).ok().map(|s| PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(default_home)
}
pub fn base_dir() -> PathBuf {
    home().join("base")
}
pub fn engine_dir() -> PathBuf {
    home().join("engine")
}
pub fn install_dir() -> PathBuf {
    home()
}
fn installed_exe() -> PathBuf {
    install_dir().join("Rewind.exe")
}

fn pack_version() -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(ENGINE_PACK)[..8])
}

fn base_ready() -> bool {
    base_dir().join("resources").is_dir() && crate::dist::Dist::locate(Some(&base_dir())).is_ok()
}
fn engine_ready() -> bool {
    let e = engine_dir();
    let dll = if cfg!(windows) { "onnxruntime.dll" } else { "libonnxruntime.so" };
    std::fs::read_to_string(e.join(".pack-version")).map(|v| v.trim() == pack_version()).unwrap_or(false)
        && (e.join(dll).exists() || !cfg!(windows))
        && e.join("models").join("ocr").join("det.onnx").exists()
        && e.join("models").join("ocr").join("rec.onnx").exists()
}

/// True when this run should show the setup window instead of the app.
pub fn needed() -> bool {
    !(base_ready() && engine_ready())
}

/// After an update (new exe, same data): refresh the engine pack quietly without a setup window.
pub fn refresh_engine_pack() {
    let e = engine_dir();
    if base_ready() && std::fs::read_to_string(e.join(".pack-version")).map(|v| v.trim() != pack_version()).unwrap_or(false) {
        if let Err(err) = unpack_engine() {
            crate::log(&format!("engine refresh: {err:#}"));
        }
    }
}

// ------------------------------------------------------------------------------------------------
// State shown by the setup page

#[derive(Default, Clone, serde::Serialize)]
struct State {
    phase: String, // "choose" | "working" | "done" | "error"
    step: String,
    detail: String,
    progress: f64, // 0..1 of the current step, -1 = unknown
    found: Vec<Value>,
    selected: Option<String>,
    error: Option<String>,
    installed_to: Option<String>,
    /// set after "Get Powder via Steam": setup continues by itself once Steam has installed it
    auto_start: bool,
    steam_installed: bool,
    steam_msg: String,
    #[serde(rename = "baseReady")]
    base_ready: bool,
    scanning: bool,
    browse_msg: String,
    /// install location shown on the page
    location: String,
    /// Powder files package on GitHub (used when no Powder install is found on this PC)
    release_name: String,
    release_size: u64,
    release_url: String,
    release_error: String,
    /// this is Rewind-Full.exe (Powder's files are inside it)
    bundled: bool,
    /// "Download instead" was chosen: don't auto-pick a local Powder install
    prefer_download: bool,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { phase: "choose".into(), location: home().display().to_string(), bundled: has_bundle(), ..Default::default() }))
}
fn set_step(step: &str, detail: &str, progress: f64) {
    let mut s = state().lock();
    s.step = step.into();
    s.detail = detail.into();
    s.progress = progress;
}

// ------------------------------------------------------------------------------------------------
// Folder picker: opened by the window's own thread so it appears in front of the setup window

static WIN: OnceLock<crate::window::WindowCtl> = OnceLock::new();
static PICK: Mutex<Option<std::sync::mpsc::Sender<Option<PathBuf>>>> = Mutex::new(None);

pub fn set_window(w: crate::window::WindowCtl) {
    let _ = WIN.set(w);
}
#[allow(dead_code)]
fn picked(p: Option<PathBuf>) {
    if let Some(tx) = PICK.lock().take() {
        let _ = tx.send(p);
    }
}
fn pick_folder() -> Option<PathBuf> {
    let w = WIN.get()?;
    let (tx, rx) = std::sync::mpsc::channel();
    *PICK.lock() = Some(tx);
    w.send("pick-folder");
    rx.recv_timeout(std::time::Duration::from_secs(1800)).ok().flatten()
}

// ------------------------------------------------------------------------------------------------
// Finding Powder

fn is_powder_dir(p: &Path) -> bool {
    let r = p.join("resources");
    (r.join("app.asar").metadata().map(|m| m.len() > 1000).unwrap_or(false) || r.join("app-unpacked").join("dist").join("renderer").join("index.html").exists())
        && (p.join("PowderRecorderCli.exe").exists() || r.join("app.asar.unpacked").join("dist").join("PowderRecorderCli.exe").exists() || r.join("app-unpacked").join("dist").join("PowderRecorderCli.exe").exists() || !cfg!(windows))
}

/// Steam library folders, from Steam's own config.
fn steam_libraries() -> Vec<PathBuf> {
    #[allow(unused_mut)]
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Some(s) = run_output("reg", &["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"]) {
            if let Some(l) = s.lines().find(|l| l.contains("SteamPath")) {
                if let Some(i) = l.find("REG_SZ") {
                    roots.push(PathBuf::from(l[i + 6..].trim().replace('/', "\\")));
                }
            }
        }
        for v in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Ok(p) = std::env::var(v) {
                roots.push(Path::new(&p).join("Steam"));
            }
        }
    }
    let mut libs = Vec::new();
    for r in roots {
        if !libs.contains(&r) && r.join("steamapps").is_dir() {
            libs.push(r.clone());
        }
        if let Ok(vdf) = std::fs::read_to_string(r.join("steamapps").join("libraryfolders.vdf")) {
            for l in vdf.lines() {
                let t = l.trim();
                if t.starts_with("\"path\"") {
                    let p = t["\"path\"".len()..].trim().trim_matches('"').replace("\\\\", "\\");
                    let p = PathBuf::from(p);
                    if !libs.contains(&p) {
                        libs.push(p);
                    }
                }
            }
        }
    }
    libs
}

fn steam_powder() -> Option<PathBuf> {
    for lib in steam_libraries() {
        let apps = lib.join("steamapps");
        let dir = std::fs::read_to_string(apps.join(format!("appmanifest_{POWDER_STEAM_APP}.acf")))
            .ok()
            .and_then(|acf| acf.lines().find(|l| l.trim().starts_with("\"installdir\"")).map(|l| l.trim()["\"installdir\"".len()..].trim().trim_matches('"').to_string()));
        for name in dir.into_iter().chain(["Powder".to_string()]) {
            let p = apps.join("common").join(name);
            if is_powder_dir(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn find_powder() -> Vec<Value> {
    let mut out = Vec::new();
    let mut add = |kind: &str, p: PathBuf| {
        if is_powder_dir(&p) && !out.iter().any(|v: &Value| v["path"] == p.display().to_string()) {
            out.push(json!({"kind": kind, "path": p.display().to_string()}));
        }
    };
    if let Ok(p) = std::env::var("POWDER_DIR") {
        add("folder", PathBuf::from(p));
    }
    if let Some(l) = dirs::data_local_dir() {
        add("installed", l.join("Programs").join("powder-desktop"));
        add("installed", l.join("Programs").join("Powder"));
    }
    if let Some(p) = steam_powder() {
        add("steam", p);
    }
    for p in extra_candidates() {
        add("folder", p.clone());
    }
    // a Powder folder next to Rewind.exe (e.g. someone unzipped both together)
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        add("folder", d.clone());
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    add("folder", e.path());
                }
            }
        }
    }
    out
}

/// Run a small command and return its stdout, giving up after a few seconds.
#[cfg(windows)]
fn run_output(prog: &str, args: &[&str]) -> Option<String> {
    use std::io::Read as _;
    let mut child = std::process::Command::new(prog).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).hidden().spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed().as_secs() < 8 => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                crate::log(&format!("setup: {prog} {args:?} timed out"));
                return None;
            }
        }
    }
    reader.join().ok().map(|b| String::from_utf8_lossy(&b).to_string())
}

fn steam_exe() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let s = run_output("reg", &["query", r"HKCU\Software\Valve\Steam", "/v", "SteamExe"])?;
        let l = s.lines().find(|l| l.contains("SteamExe"))?;
        let p = PathBuf::from(l[l.find("REG_SZ")? + 6..].trim().replace('/', "\\"));
        return p.exists().then_some(p);
    }
    #[allow(unreachable_code)]
    None
}

/// Powder is free on Steam: ask Steam to install it (installing Steam itself first if needed, from
/// Valve's own download). Setup carries on by itself once Powder appears.
fn get_powder_via_steam() {
    state().lock().auto_start = true;
    if steam_exe().is_some() {
        crate::features::core::open_path(&format!("steam://install/{POWDER_STEAM_APP}"));
        state().lock().steam_msg = "In the Steam window, click Install. Setup continues by itself when Powder has downloaded.".into();
        return;
    }
    std::thread::spawn(|| {
        state().lock().steam_msg = "Steam isn't installed, so Valve's Steam installer is downloading…".into();
        let to = home().join("SteamSetup.exe");
        let _ = std::fs::create_dir_all(home());
        let r = download_quiet("https://cdn.akamai.steamstatic.com/client/installer/SteamSetup.exe", &to);
        if r.is_err() {
            state().lock().steam_msg = "Couldn't download Steam. Install it from store.steampowered.com, then click the button again.".into();
            crate::features::core::open_path("https://store.steampowered.com/about/");
            return;
        }
        state().lock().steam_msg = "Install Steam and sign in (a free account works). Powder's install page then opens in Steam.".into();
        let _ = std::process::Command::new(&to).status();
        // wait for Steam to be set up and signed in, then ask it to install Powder
        for _ in 0..600 {
            if steam_exe().is_some() {
                std::thread::sleep(std::time::Duration::from_secs(5));
                crate::features::core::open_path(&format!("steam://install/{POWDER_STEAM_APP}"));
                state().lock().steam_msg = "In the Steam window, click Install. Setup continues by itself when Powder has downloaded.".into();
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

fn download_quiet(url: &str, to: &Path) -> Result<()> {
    let mut r = agent().get(url).set("User-Agent", "Rewind").call()?.into_reader();
    let mut w = std::fs::File::create(to)?;
    std::io::copy(&mut r, &mut w)?;
    Ok(())
}

/// Slower places, looked at once: uninstall entries in the registry, and folders with "powder" in
/// their name at the top of every drive and in Downloads / Desktop / Documents (portable copies).
fn extra_candidates() -> &'static Vec<PathBuf> {
    static C: OnceLock<Vec<PathBuf>> = OnceLock::new();
    C.get_or_init(|| {
        let mut out: Vec<PathBuf> = Vec::new();
        #[cfg(windows)]
        for root in [r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall", r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall", r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"] {
            if let Some(o) = run_output("reg", &["query", root, "/s", "/v", "InstallLocation"]) {
                for l in o.lines() {
                    if let Some(i) = l.find("REG_SZ") {
                        let v = l[i + 6..].trim().trim_matches('"');
                        if v.to_lowercase().contains("powder") {
                            out.push(PathBuf::from(v));
                        }
                    }
                }
            }
        }
        let mut roots: Vec<PathBuf> = Vec::new();
        #[cfg(windows)]
        for d in b'A'..=b'Z' {
            let root = format!("{}:\\", d as char);
            let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
            // DRIVE_FIXED = 3
            if unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(wide.as_ptr()) } == 3 {
                roots.push(PathBuf::from(root));
            }
        }
        for d in [dirs::download_dir(), dirs::desktop_dir(), dirs::document_dir(), dirs::home_dir()].into_iter().flatten() {
            roots.push(d);
        }
        for r in roots {
            let Ok(rd) = std::fs::read_dir(&r) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if !name.contains("powder") || !e.path().is_dir() {
                    continue;
                }
                out.push(e.path());
                // one level down too (e.g. D:\Powder backup\Powder_v8.0.2)
                if let Ok(rd2) = std::fs::read_dir(e.path()) {
                    for e2 in rd2.flatten() {
                        if e2.path().is_dir() {
                            out.push(e2.path());
                        }
                    }
                }
            }
        }
        out
    })
}

fn rescan() {
    let found = find_powder();
    let steam = steam_exe().is_some();
    let mut s = state().lock();
    s.steam_installed = steam;
    if !s.prefer_download && s.selected.as_deref().map(|p| !is_powder_dir(Path::new(p))).unwrap_or(true) {
        s.selected = found.first().and_then(|f| f["path"].as_str().map(String::from));
    }
    s.found = found;
}

// ------------------------------------------------------------------------------------------------
// Copying Powder's files (only what Rewind uses; Electron's own runtime is left behind)

const SKIP_FILES: &[&str] = &[
    "powder.exe", "uninstall powder.exe", "powderinstaller.exe", "chrome_100_percent.pak", "chrome_200_percent.pak", "resources.pak", "icudtl.dat", "snapshot_blob.bin",
    "v8_context_snapshot.bin", "libegl.dll", "libglesv2.dll", "vk_swiftshader.dll", "vk_swiftshader_icd.json", "d3dcompiler_47.dll", "licenses.chromium.html", "license.electron.txt",
    "ffmpeg.dll", "vulkan-1.dll", "launch_game_manager.bat", "steam_api64.dll",
];
const SKIP_DIRS: &[&str] = &["locales", "node_modules", ".git", "game-manager", "swiftshader"];

fn wanted(rel: &Path, is_dir: bool) -> bool {
    let name = rel.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if is_dir {
        return !SKIP_DIRS.contains(&name.as_str());
    }
    if name.ends_with(".pdb") {
        return false;
    }
    // Electron runtime files only matter at the top level
    !(rel.components().count() == 1 && SKIP_FILES.contains(&name.as_str()))
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(rd) = std::fs::read_dir(root.join(rel)) else { return };
    for e in rd.flatten() {
        let r = rel.join(e.file_name());
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if wanted(&r, true) {
                walk(root, &r, out);
            }
        } else if ft.is_file() && wanted(&r, false) {
            out.push((r, e.metadata().map(|m| m.len()).unwrap_or(0)));
        }
    }
}

fn copy_powder(src: &Path) -> Result<()> {
    set_step("Copying Powder's recorder and AI files", "Scanning…", -1.0);
    let mut files = Vec::new();
    walk(src, Path::new(""), &mut files);
    if files.is_empty() {
        bail!("No files found in {}", src.display());
    }
    let total: u64 = files.iter().map(|f| f.1).sum::<u64>().max(1);
    let tmp = home().join("base.partial");
    let _ = std::fs::remove_dir_all(&tmp);
    let mut done = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    for (rel, size) in &files {
        let to = tmp.join(rel);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut r = std::fs::File::open(src.join(rel)).with_context(|| format!("reading {}", src.join(rel).display()))?;
        let mut w = std::fs::File::create(&to).with_context(|| format!("writing {}", to.display()))?;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
            done += n as u64;
            set_step("Copying Powder's recorder and AI files", &format!("{} of {} MB", done >> 20, total >> 20), done as f64 / total as f64);
        }
        let _ = size;
    }
    finish_base(&tmp)
}

/// Swap a freshly copied base.partial in, keeping custom games added earlier.
fn finish_base(tmp: &Path) -> Result<()> {
    // keep custom games the user already added if setup runs again
    let old = base_dir();
    if old.exists() {
        for keep in ["ai-configs", "assets"] {
            let _ = merge_missing(&old.join(keep), &tmp.join(keep));
        }
        std::fs::remove_dir_all(&old).context("removing the old copy of Powder's files")?;
    }
    std::fs::rename(tmp, &old)?;
    Ok(())
}

/// Copy files from `from` that `to` doesn't have (custom games added earlier).
fn merge_missing(from: &Path, to: &Path) -> Result<()> {
    for e in std::fs::read_dir(from)?.flatten() {
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            std::fs::create_dir_all(&t)?;
            merge_missing(&e.path(), &t)?;
        } else if !t.exists() || e.file_name() == "whitelist.json" {
            if e.file_name() == "whitelist.json" && t.exists() {
                // union of both whitelists
                let a: Value = serde_json::from_slice(&std::fs::read(e.path())?).unwrap_or(json!([]));
                let mut b: Value = serde_json::from_slice(&std::fs::read(&t)?).unwrap_or(json!([]));
                if let (Some(a), Some(bv)) = (a.as_array(), b.as_array_mut()) {
                    for x in a {
                        if !bv.contains(x) {
                            bv.push(x.clone());
                        }
                    }
                }
                std::fs::write(&t, serde_json::to_vec_pretty(&b)?)?;
            } else {
                std::fs::copy(e.path(), &t)?;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Powder's files from the community release on GitHub (redistribution allowed by Powder's staff,
// see the Powder Discord). Used when this PC has no Powder install.

const RELEASE_API: &str = "https://api.github.com/repos/DavidSnipeZ669/Powder-custom-game-support/releases/tags/Official";

fn fetch_release() -> Result<(String, String, u64)> {
    // for testing a package before publishing it
    if let Ok(u) = std::env::var("REWIND_POWDER_ZIP_URL") {
        return Ok((u.rsplit('/').next().unwrap_or("powder.zip").to_string(), u, 0));
    }
    let v: Value = agent().get(RELEASE_API).set("User-Agent", "Rewind").set("Accept", "application/vnd.github+json").call()?.into_json()?;
    let assets = v["assets"].as_array().context("the release has no files")?;
    let score = |a: &Value| {
        let n = a["name"].as_str().unwrap_or("").to_lowercase();
        (n.ends_with(".zip") as i32) * 10 + (n.contains("powder") as i32) * 2 + (n.contains("rewind") as i32) * 3
    };
    let best = assets.iter().filter(|a| a["name"].as_str().unwrap_or("").to_lowercase().ends_with(".zip")).max_by_key(|a| score(a)).context("the release has no .zip file of Powder's files")?;
    Ok((best["name"].as_str().unwrap_or("").to_string(), best["browser_download_url"].as_str().unwrap_or("").to_string(), best["size"].as_u64().unwrap_or(0)))
}

fn refresh_release() {
    match fetch_release() {
        Ok((n, u, sz)) => {
            let mut s = state().lock();
            s.release_name = n;
            s.release_url = u;
            s.release_size = sz;
            s.release_error.clear();
        }
        Err(e) => {
            crate::log(&format!("setup: release lookup: {e:#}"));
            state().lock().release_error = format!("{e:#}");
        }
    }
}

/// Download the release zip and unpack only what Rewind uses straight into base\ (Electron's own
/// files are skipped), then delete the download.
fn download_powder() -> Result<()> {
    let url = {
        let s = state().lock();
        s.release_url.clone()
    };
    if url.is_empty() {
        refresh_release();
    }
    let (name, url) = {
        let s = state().lock();
        (s.release_name.clone(), s.release_url.clone())
    };
    if url.is_empty() {
        bail!("Couldn't reach the Powder files on GitHub ({}). Check your internet connection and try again, or use \"Other options\".", state().lock().release_error);
    }
    let dl = home().join("downloads");
    std::fs::create_dir_all(&dl)?;
    let zip_path = dl.join(&name);
    if !zip_path.exists() {
        download(&url, &zip_path, "Downloading Powder's recorder and AI files")?;
    }
    let z = zip::ZipArchive::new(std::fs::File::open(&zip_path)?).context("the downloaded package is damaged; try again")?;
    extract_powder_zip(z)?;
    let _ = std::fs::remove_file(&zip_path);
    Ok(())
}

/// Rewind-Full.exe: Powder's files are appended to the exe as a zip (see `make_full`).
fn bundled_powder() -> Option<zip::ZipArchive<std::fs::File>> {
    let me = std::env::current_exe().ok()?;
    zip::ZipArchive::new(std::fs::File::open(me).ok()?).ok().filter(|z| z.len() > 10)
}
pub fn has_bundle() -> bool {
    static B: OnceLock<bool> = OnceLock::new();
    *B.get_or_init(|| bundled_powder().is_some())
}

fn extract_powder_zip(mut z: zip::ZipArchive<std::fs::File>) -> Result<()> {
    set_step("Unpacking Powder's files", "Reading the package…", -1.0);
    // the Powder folder may be at the top of the zip or inside a folder
    let mut prefix: Option<String> = None;
    for i in 0..z.len() {
        let n = z.by_index_raw(i)?.name().replace('\\', "/");
        for marker in ["resources/app-unpacked/dist/renderer/index.html", "resources/app.asar"] {
            if let Some(p) = n.strip_suffix(marker) {
                if prefix.as_ref().map(|q| p.len() < q.len()).unwrap_or(true) {
                    prefix = Some(p.to_string());
                }
            }
        }
    }
    let prefix = prefix.context("the package doesn't contain Powder's files")?;
    let tmp = home().join("base.partial");
    let _ = std::fs::remove_dir_all(&tmp);
    let total = z.len().max(1);
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let n = f.name().replace('\\', "/");
        let Some(rel) = n.strip_prefix(&prefix) else { continue };
        if rel.is_empty() || f.is_dir() {
            continue;
        }
        let relp = Path::new(rel);
        if !relp.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
            continue;
        }
        // same filter as copying from an install: skip Electron and debug files
        let mut ok = true;
        let mut acc = PathBuf::new();
        let comps: Vec<_> = relp.components().collect();
        for (k, c) in comps.iter().enumerate() {
            acc.push(c);
            if !wanted(&acc, k + 1 < comps.len()) {
                ok = false;
                break;
            }
        }
        if !ok {
            continue;
        }
        let to = tmp.join(relp);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&to)?;
        std::io::copy(&mut f, &mut w)?;
        if i % 20 == 0 {
            set_step("Unpacking Powder's files", &format!("{} of {} files", i + 1, total), (i + 1) as f64 / total as f64);
        }
    }
    finish_base(&tmp)
}

/// `Rewind.exe --make-full <Powder folder> <out.exe>`: writes a copy of this exe with Powder's files
/// (only the parts Rewind uses) appended as a zip. Setup then installs without downloading anything.
pub fn make_full(src: &Path, out: &Path) -> Result<()> {
    if !is_powder_dir(src) {
        bail!("{} isn't a Powder folder", src.display());
    }
    let me = std::env::current_exe()?;
    let mut f = std::fs::File::create(out)?;
    std::io::copy(&mut std::fs::File::open(&me)?, &mut f)?;
    let mut files = Vec::new();
    walk(src, Path::new(""), &mut files);
    let mut z = zip::ZipWriter::new(f);
    // fast compression: most of the size is DLLs and models that barely shrink anyway
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(1)).large_file(true);
    let mut buf = vec![0u8; 1 << 20];
    for (rel, _) in &files {
        let name = format!("Powder/{}", rel.to_string_lossy().replace('\\', "/"));
        z.start_file(name, opt)?;
        let mut r = std::fs::File::open(src.join(rel))?;
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            z.write_all(&buf[..n])?;
        }
    }
    // the AI downloads too, when this PC already has them (so Rewind-Full needs no internet at all)
    let eng = engine_dir();
    for rel in ["onnxruntime.dll", "onnxruntime_providers_shared.dll", "DirectML.dll", "models/ocr/det.onnx", "models/ocr/rec.onnx"] {
        if let Ok(b) = std::fs::read(eng.join(rel)) {
            z.start_file(format!("Engine/{rel}"), opt)?;
            z.write_all(&b)?;
        }
    }
    z.finish()?;
    Ok(())
}

/// Engine files (ONNX Runtime, DirectML, OCR models) carried by Rewind-Full.exe.
fn unpack_bundled_engine() {
    let Some(mut z) = bundled_powder() else { return };
    let dir = engine_dir();
    for i in 0..z.len() {
        let Ok(mut f) = z.by_index(i) else { continue };
        let Some(rel) = f.name().strip_prefix("Engine/").map(String::from) else { continue };
        if rel.contains("..") {
            continue;
        }
        let to = dir.join(&rel);
        if to.exists() {
            continue;
        }
        if let Some(p) = to.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(mut w) = std::fs::File::create(&to) {
            let _ = std::io::copy(&mut f, &mut w);
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Engine: unpack the embedded pack, download ONNX Runtime / DirectML / OCR models

fn unpack_engine() -> Result<()> {
    let dir = engine_dir();
    std::fs::create_dir_all(&dir)?;
    if ENGINE_PACK.len() < 100 {
        bail!("this Rewind.exe was built without the custom-game engine (build with build-app.bat)");
    }
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(ENGINE_PACK))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { continue };
        let to = dir.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&to)?;
            continue;
        }
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&to).with_context(|| format!("writing {} (is Rewind already running?)", to.display()))?;
        std::io::copy(&mut f, &mut w)?;
    }
    std::fs::write(dir.join(".pack-version"), pack_version())?;
    Ok(())
}

fn agent() -> ureq::Agent {
    #[cfg(windows)]
    {
        if let Ok(tls) = native_tls::TlsConnector::new() {
            return ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls)).timeout_connect(std::time::Duration::from_secs(20)).build();
        }
    }
    ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(20)).build()
}

fn download(url: &str, to: &Path, label: &str) -> Result<()> {
    let mut last = None;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        match download_once(url, to, label) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap()).with_context(|| format!("downloading {url}"))
}

fn download_once(url: &str, to: &Path, label: &str) -> Result<()> {
    set_step(label, "Connecting…", -1.0);
    let resp = agent().get(url).set("User-Agent", "Rewind").call()?;
    let total: u64 = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut r = resp.into_reader();
    let part = to.with_extension("part");
    if let Some(p) = part.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut w = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        got += n as u64;
        let p = if total > 0 { got as f64 / total as f64 } else { -1.0 };
        set_step(label, &format!("{:.1} MB", got as f64 / 1048576.0), p);
    }
    drop(w);
    if total > 0 && got < total {
        bail!("download cut off ({got} of {total} bytes)");
    }
    std::fs::rename(&part, to)?;
    Ok(())
}

/// Pull single files out of a NuGet package (a zip).
fn extract_from_nupkg(pkg: &Path, wanted: &[(&str, &str)], dir: &Path) -> Result<()> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(pkg)?)?;
    for (inner, name) in wanted {
        let mut f = z.by_name(inner).with_context(|| format!("{inner} not in {}", pkg.display()))?;
        let mut w = std::fs::File::create(dir.join(name))?;
        std::io::copy(&mut f, &mut w)?;
    }
    Ok(())
}

fn engine_downloads() -> Result<()> {
    let dir = engine_dir();
    let tmp = home().join("downloads");
    std::fs::create_dir_all(&tmp)?;
    if cfg!(windows) {
        if !dir.join("onnxruntime.dll").exists() || !dir.join("onnxruntime_providers_shared.dll").exists() {
            let p = tmp.join(format!("ort-{ORT_VERSION}.nupkg"));
            if !p.exists() {
                download(&format!("https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/{ORT_VERSION}/microsoft.ml.onnxruntime.directml.{ORT_VERSION}.nupkg"), &p, "Downloading ONNX Runtime (Microsoft)")?;
            }
            extract_from_nupkg(&p, &[("runtimes/win-x64/native/onnxruntime.dll", "onnxruntime.dll"), ("runtimes/win-x64/native/onnxruntime_providers_shared.dll", "onnxruntime_providers_shared.dll")], &dir)?;
        }
        if !dir.join("DirectML.dll").exists() {
            let p = tmp.join(format!("dml-{DML_VERSION}.nupkg"));
            if !p.exists() {
                download(&format!("https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/{DML_VERSION}/microsoft.ai.directml.{DML_VERSION}.nupkg"), &p, "Downloading DirectML (Microsoft)")?;
            }
            extract_from_nupkg(&p, &[("bin/x64-win/DirectML.dll", "DirectML.dll")], &dir)?;
        }
    }
    let ocr = dir.join("models").join("ocr");
    std::fs::create_dir_all(&ocr)?;
    for (remote, local) in [("ch_PP-OCRv4_det_infer.onnx", "det.onnx"), ("ch_PP-OCRv4_rec_infer.onnx", "rec.onnx")] {
        if !ocr.join(local).exists() {
            download(&format!("{OCR_BASE}/{remote}"), &ocr.join(local), "Downloading text-detection models (PaddleOCR)")?;
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Installing Rewind itself

fn install_self() -> Result<PathBuf> {
    let me = std::env::current_exe()?;
    let target = installed_exe();
    if !cfg!(windows) {
        return Ok(me);
    }
    std::fs::create_dir_all(install_dir())?;
    let same = me.canonicalize().ok() == target.canonicalize().ok();
    if !same {
        // an older Rewind may still be running from there; rename it out of the way
        if target.exists() && std::fs::copy(&me, &target).is_err() {
            let old = install_dir().join("Rewind.old.exe");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&target, &old)?;
            std::fs::copy(&me, &target)?;
        } else if !target.exists() {
            std::fs::copy(&me, &target)?;
        }
    }
    #[cfg(windows)]
    {
        let t = target.display().to_string();
        let ps = format!(
            "$s=New-Object -ComObject WScript.Shell;foreach($d in @([Environment]::GetFolderPath('Programs'),[Environment]::GetFolderPath('Desktop'))){{$l=$s.CreateShortcut((Join-Path $d 'Rewind.lnk'));$l.TargetPath='{t}';$l.WorkingDirectory='{w}';$l.IconLocation='{t},0';$l.Description='Rewind';$l.Save()}}",
            t = t.replace('\'', "''"),
            w = install_dir().display().to_string().replace('\'', "''")
        );
        let _ = std::process::Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps]).hidden().status();
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Rewind";
        for (name, ty, val) in [
            ("DisplayName", "REG_SZ", "Rewind".to_string()),
            ("DisplayIcon", "REG_SZ", format!("{t},0")),
            ("Publisher", "REG_SZ", "Rewind (community)".to_string()),
            ("DisplayVersion", "REG_SZ", env!("CARGO_PKG_VERSION").to_string()),
            ("InstallLocation", "REG_SZ", install_dir().display().to_string()),
            ("UninstallString", "REG_SZ", format!("\"{t}\" --uninstall")),
            ("NoModify", "REG_DWORD", "1".to_string()),
            ("NoRepair", "REG_DWORD", "1".to_string()),
        ] {
            let _ = std::process::Command::new("reg").args(["add", key, "/v", name, "/t", ty, "/d", &val, "/f"]).hidden().status();
        }
    }
    Ok(target)
}

/// `Rewind.exe --uninstall` (from Apps & features).
pub fn uninstall() {
    #[cfg(windows)]
    {
        let keep = rfd::MessageDialog::new()
            .set_title("Uninstall Rewind")
            .set_description("Remove Rewind?\n\nYour recordings, clips and montages are not touched.\nChoose Yes to remove Rewind, or No to cancel.")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if keep != rfd::MessageDialogResult::Yes {
            return;
        }
        let ps = "$s=New-Object -ComObject WScript.Shell;foreach($d in @([Environment]::GetFolderPath('Programs'),[Environment]::GetFolderPath('Desktop'))){Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $d 'Rewind.lnk')}";
        let _ = std::process::Command::new("powershell").args(["-NoProfile", "-Command", ps]).hidden().status();
        let _ = std::process::Command::new("reg").args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Rewind", "/f"]).hidden().status();
        let h = home();
        let _ = std::fs::remove_file(location_file());
        let _ = std::fs::remove_dir_all(&h);
        // the running exe can't delete itself: a short-lived cmd removes the folder once we've exited
        let dir = install_dir().display().to_string();
        let _ = std::process::Command::new("cmd").args(["/C", &format!("ping 127.0.0.1 -n 3 >nul & rmdir /s /q \"{dir}\"")]).hidden().spawn();
        rfd::MessageDialog::new().set_title("Rewind").set_description("Rewind was removed.").show();
    }
}

// ------------------------------------------------------------------------------------------------

fn run(src: PathBuf) {
    let r = (|| -> Result<PathBuf> {
        std::fs::create_dir_all(home())?;
        if !base_ready() {
            if src.as_os_str().is_empty() {
                match bundled_powder() {
                    Some(z) => extract_powder_zip(z)?,
                    None => download_powder()?,
                }
            } else {
                copy_powder(&src)?;
            }
        }
        set_step("Unpacking the custom-game engine", "", -1.0);
        unpack_engine()?;
        unpack_bundled_engine();
        engine_downloads()?;
        let _ = std::fs::create_dir_all(home().join("music"));
        set_step("Installing Rewind", "Start menu and desktop shortcuts", -1.0);
        install_self()
    })();
    let mut s = state().lock();
    match r {
        Ok(exe) => {
            s.phase = "done".into();
            s.installed_to = Some(exe.display().to_string());
        }
        Err(e) => {
            s.phase = "error".into();
            s.error = Some(format!("{e:#}"));
        }
    }
}

fn start_setup() -> Value {
    let src = {
        let mut s = state().lock();
        if s.phase == "working" {
            return json!({"ok": false});
        }
        // a Powder install on this PC is copied; otherwise Powder's files are downloaded
        let src = s.selected.clone().map(PathBuf::from);
        s.phase = "working".into();
        s.error = None;
        let loc = PathBuf::from(s.location.clone());
        drop(s);
        // remember the chosen location (default location needs no pointer file)
        if loc == default_home() {
            let _ = std::fs::remove_file(location_file());
        } else {
            let _ = std::fs::write(location_file(), loc.display().to_string());
        }
        src.unwrap_or_default()
    };
    std::thread::spawn(move || run(src));
    json!({"ok": true})
}

/// Looks for Powder in the background (every few seconds while the user is choosing), so the page
/// never waits on disk or registry lookups.
fn start_scanner() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        state().lock().scanning = true;
        std::thread::spawn(refresh_release);
        std::thread::spawn(|| loop {
            if state().lock().phase == "choose" {
                crate::log("setup: looking for Powder");
                let t0 = std::time::Instant::now();
                rescan();
                let br = base_ready();
                let go = {
                    let mut st = state().lock();
                    st.base_ready = br;
                    st.scanning = false;
                    st.auto_start && st.selected.is_some()
                };
                crate::log(&format!("setup: scan took {} ms", t0.elapsed().as_millis()));
                if go {
                    state().lock().auto_start = false;
                    start_setup();
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
    });
}

/// Current state for the page, as JSON.
pub fn state_json() -> String {
    start_scanner();
    serde_json::to_value(state().lock().clone()).unwrap_or_default().to_string()
}

fn use_picked(picked: Option<PathBuf>) {
    let Some(mut p) = picked else { return };
    crate::log(&format!("setup: picked {}", p.display()));
    // accept the resources folder or a file inside the Powder folder too
    for _ in 0..2 {
        if !is_powder_dir(&p) {
            if let Some(par) = p.parent() {
                p = par.to_path_buf();
            }
        }
    }
    let mut st = state().lock();
    if is_powder_dir(&p) {
        st.selected = Some(p.display().to_string());
        st.prefer_download = false;
        st.browse_msg.clear();
    } else {
        st.browse_msg = format!("{} doesn't look like Powder (it needs the resources folder and PowderRecorderCli.exe).", p.display());
    }
}

/// The install-location picker's result: installs into a "Rewind" folder inside the chosen folder
/// (unless it already is one).
pub fn picked_location(p: Option<PathBuf>) {
    let Some(mut p) = p else { return };
    if p.file_name().map(|n| !n.to_string_lossy().eq_ignore_ascii_case("rewind")).unwrap_or(true) {
        p = p.join("Rewind");
    }
    state().lock().location = p.display().to_string();
}

/// The folder picker's result (it runs on the window's thread).
pub fn picked_folder(p: Option<PathBuf>) {
    use_picked(p);
}

/// A button on the setup page (sent through the webview's message channel). "browse", "close" and
/// "launch" are handled by the window itself.
pub fn command(cmd: &str, arg: &str) {
    crate::log(&format!("setup: {cmd}"));
    match cmd {
        "select" => {
            let mut st = state().lock();
            if arg.is_empty() {
                st.selected = None;
                st.prefer_download = true;
            } else if is_powder_dir(Path::new(arg)) {
                st.selected = Some(arg.to_string());
                st.prefer_download = false;
            }
        }
        "steam" => get_powder_via_steam(),
        "start" => {
            let r = start_setup();
            if let Some(e) = r["error"].as_str() {
                state().lock().browse_msg = e.to_string();
            }
        }
        _ => {}
    }
}

pub fn launch_target() -> Option<PathBuf> {
    state().lock().installed_to.clone().map(PathBuf::from)
}

/// HTTP handler for the setup page (serves the page; buttons use the message channel, with these
/// routes as a fallback for testing in a browser).
pub fn handle(req: Request, quit: &(dyn Fn(Option<PathBuf>) + Sync)) {
    let path = req.url.split('?').next().unwrap_or("/").to_string();
    crate::log(&format!("setup: {} {}", req.method, path));
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/") => req.respond(200, "text/html; charset=utf-8", include_str!("../assets/setup.html").replace("__LOGO__", include_str!("../assets/branding/logo.svg")).as_bytes()),
        ("GET", "/state") => req.respond(200, "application/json", state_json().as_bytes()),
        ("POST", "/cmd") => {
            let v: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let cmd = v["cmd"].as_str().unwrap_or("").to_string();
            let arg = v["arg"].as_str().unwrap_or("").to_string();
            match cmd.as_str() {
                "browse" => {
                    let p = pick_folder();
                    use_picked(p);
                    req.respond(200, "application/json", b"{}")
                }
                "launch" => {
                    req.respond(200, "application/json", b"{}");
                    quit(launch_target());
                }
                "close" => {
                    req.respond(200, "application/json", b"{}");
                    quit(None);
                }
                _ => {
                    command(&cmd, &arg);
                    req.respond(200, "application/json", b"{}")
                }
            }
        }
        _ => req.status(404),
    }
}

#[cfg(windows)]
trait Hidden {
    fn hidden(&mut self) -> &mut Self;
}
#[cfg(windows)]
impl Hidden for std::process::Command {
    fn hidden(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}
