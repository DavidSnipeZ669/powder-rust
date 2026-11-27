//! Community games: game support other people added, shared through the Rewind repo.
//!
//! Download: the repo's `games/` folder holds one folder per game (the same pack the engine makes:
//! game.json, events.json, game_postprocess.lua, assets/) plus `games/index.json`. A few seconds
//! after start, Rewind reads the index from GitHub (public files: no account or token needed),
//! downloads games that are new or changed and installs them with the engine, like a game added on
//! this PC. Games you made yourself are never overwritten.
//!
//! Share: after adding a game, "Share with the community" zips its pack and opens a pre-filled
//! GitHub issue; the maintainer reviews it (packs contain Lua) and adds it to `games/`.
//!
//! Maintainer: `Rewind.exe --community-index <repo>\games` rewrites index.json after adding packs.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const REPO: &str = "DavidSnipeZ669/powder-rust";
const BRANCH: &str = "main";

fn raw(path: &str) -> String {
    // REWIND_COMMUNITY_URL: test a games folder before pushing it (e.g. a local web server)
    match std::env::var("REWIND_COMMUNITY_URL") {
        Ok(base) => format!("{}/{path}", base.trim_end_matches('/')),
        Err(_) => format!("https://raw.githubusercontent.com/{REPO}/{BRANCH}/games/{path}"),
    }
}

/// Where the engine keeps game packs (games added here and community games).
pub fn games_dir() -> PathBuf {
    crate::setup::engine_dir().join("games")
}

/// Which community games are installed, and at which version: {id: version}.
fn state_path() -> PathBuf {
    super::user_data().join("community-games.json")
}
fn load_state() -> serde_json::Map<String, Value> {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn is_safe_rel(p: &str) -> bool {
    !p.is_empty() && !p.starts_with('/') && !p.contains('\\') && !p.contains(':') && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The community relay (Cloudflare Worker): its address lives in the repo's games/config.json
/// ({"worker": "https://..."}), so it can be set or moved without a new Rewind release.
fn worker_url() -> Option<String> {
    static W: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        if let Ok(u) = std::env::var("REWIND_WORKER_URL") {
            return Some(u);
        }
        let b = get_bytes(&raw("config.json")).ok()?;
        let v: Value = serde_json::from_slice(&b).ok()?;
        v["worker"].as_str().filter(|u| u.starts_with("https://")).map(|u| u.trim_end_matches('/').to_string())
    })
    .clone()
}

fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let r = super::download::agent().get(url).set("User-Agent", "Rewind").call()?;
    let mut b = Vec::new();
    r.into_reader().take(50 << 20).read_to_end(&mut b)?;
    Ok(b)
}

/// Background sync, called once at start.
pub fn start_sync() {
    if std::env::var("REWIND_NO_COMMUNITY").is_ok() {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(5));
        match sync() {
            Ok(0) => {}
            Ok(n) => crate::log(&format!("community games: {n} installed or updated")),
            Err(e) => crate::log(&format!("community games: {e:#}")),
        }
    });
}

fn sync() -> Result<usize> {
    // approved games: from the relay (built from the repo), else the repo's index.json
    let from_worker = worker_url().and_then(|w| get_bytes(&format!("{w}/index")).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(|v| v["games"].is_array());
    let idx: Value = match from_worker {
        Some(v) => v,
        None => serde_json::from_slice(&get_bytes(&raw("index.json")).context("reading the community games list")?)?,
    };
    let games = idx["games"].as_array().cloned().unwrap_or_default();
    let mut state = load_state();
    let dir = games_dir();
    std::fs::create_dir_all(&dir)?;
    let mut changed = 0;
    for g in games {
        let id = g["id"].as_str().unwrap_or("").to_string();
        let ver = g["version"].as_str().unwrap_or("").to_string();
        if !is_safe_id(&id) || ver.is_empty() {
            continue;
        }
        let local = dir.join(&id);
        let ours = state.get(&id).and_then(|v| v.as_str()).map(String::from);
        if ours.as_deref() == Some(ver.as_str()) {
            continue; // up to date
        }
        if ours.is_none() && local.exists() {
            continue; // a game made on this PC with the same id: leave it alone
        }
        match fetch_pack(&g, &id, &dir).and_then(|_| install(&id)) {
            Ok(()) => {
                state.insert(id.clone(), json!(ver));
                let _ = std::fs::write(state_path(), serde_json::to_vec_pretty(&state)?);
                changed += 1;
            }
            Err(e) => crate::log(&format!("community game {id}: {e:#}")),
        }
    }
    if changed > 0 {
        let ctx = super::ctx();
        super::games::refresh(&ctx.host, ctx);
    }
    Ok(changed)
}

fn fetch_pack(g: &Value, id: &str, dir: &Path) -> Result<()> {
    let files: Vec<String> = g["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect()).unwrap_or_default();
    if !files.iter().any(|f| f == "game_postprocess.lua") || !files.iter().any(|f| f == "events.json") {
        bail!("incomplete pack in the index");
    }
    let tmp = dir.join(format!(".{id}.partial"));
    let _ = std::fs::remove_dir_all(&tmp);
    for f in &files {
        if !is_safe_rel(f) {
            bail!("bad file name {f}");
        }
        let b = get_bytes(&raw(&format!("{id}/{}", f.split('/').map(urlencode).collect::<Vec<_>>().join("/"))))?;
        let to = tmp.join(f);
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&to, b)?;
    }
    let dest = dir.join(id);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// Install a pack from the engine's games folder (same as a game added on this PC).
fn install(id: &str) -> Result<()> {
    let eng = super::customgame::engine().context("the game engine is missing")?;
    let root = super::ctx().dist.root.clone();
    let mut c = Command::new(&eng);
    c.current_dir(eng.parent().unwrap()).args(["powder", "install", id, "--powder"]).arg(&root);
    crate::gpu::HiddenExt::creation_flags_hidden(&mut c);
    let out = c.output()?;
    if !out.status.success() {
        bail!("install failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// Sharing

/// Zip a game pack made on this PC and open a pre-filled "new game" issue on GitHub.
pub fn share(id: &str) -> Value {
    if let Some(w) = worker_url() {
        match upload(id, &w) {
            Ok(url) => {
                super::core::open_path(&url);
                return json!({"ok": true, "uploaded": true, "url": url});
            }
            Err(e) => crate::log(&format!("community upload failed, falling back to a zip: {e:#}")),
        }
    }
    match share_inner(id) {
        Ok(zip) => json!({"ok": true, "zip": zip.display().to_string()}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    }
}

/// Send the game to the relay, which opens a pull request for the reviewers. Returns its link.
fn upload(id: &str, worker: &str) -> Result<String> {
    use base64::Engine;
    if !is_safe_id(id) {
        bail!("unknown game");
    }
    let src = games_dir().join(id);
    let meta: Value = std::fs::read(src.join("game.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut files = serde_json::Map::new();
    for f in list_files(&src)? {
        if f == "my-edits.json" {
            continue;
        }
        files.insert(f.clone(), json!(base64::engine::general_purpose::STANDARD.encode(std::fs::read(src.join(&f))?)));
    }
    let body = json!({"id": id, "name": meta["name"].as_str().unwrap_or(id), "author": meta["authors"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("a Rewind user"), "files": files});
    let r = super::download::agent().post(&format!("{worker}/submit")).set("User-Agent", "Rewind").send_json(body);
    let v: Value = match r {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(_, r)) => r.into_json().unwrap_or(json!({})),
        Err(e) => bail!("{e}"),
    };
    if v["ok"] != true {
        bail!("{}", v["error"].as_str().unwrap_or("the upload was refused"));
    }
    Ok(v["url"].as_str().unwrap_or("").to_string())
}

fn share_inner(id: &str) -> Result<PathBuf> {
    if !is_safe_id(id) {
        bail!("unknown game");
    }
    let src = games_dir().join(id);
    if !src.join("game_postprocess.lua").exists() {
        bail!("the game's files weren't found in {}", src.display());
    }
    let name = std::fs::read(src.join("game.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["name"].as_str().map(String::from)).unwrap_or_else(|| id.to_string());
    let out_dir = dirs::download_dir().or_else(dirs::desktop_dir).unwrap_or_else(std::env::temp_dir);
    let zip_path = out_dir.join(format!("Rewind-game-{id}.zip"));
    let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path)?);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for f in list_files(&src)? {
        z.start_file(format!("{id}/{f}"), opt)?;
        z.write_all(&std::fs::read(src.join(&f))?)?;
    }
    z.finish()?;
    let body = format!(
        "**Game:** {name}\n**Game id:** {id}\n\nThe zip **Rewind-game-{id}.zip** is in your Downloads folder. Drag it into this box to attach it, then click *Submit new issue*.\n\nAnything worth knowing (which events work, your resolution, the game mode you recorded):\n\n"
    );
    let url = format!("https://github.com/{REPO}/issues/new?title={}&body={}", urlencode(&format!("New game: {name}")), urlencode(&body));
    super::core::open_path(&url);
    super::core::reveal(&zip_path.display().to_string());
    Ok(zip_path)
}

/// Every file of a pack, as forward-slash paths relative to it (work files left out).
fn list_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, rel: &str, out: &mut Vec<String>) -> Result<()> {
        let mut ents: Vec<_> = std::fs::read_dir(root.join(rel))?.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with('.') || n.starts_with('_') {
                continue;
            }
            let r = if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") };
            if e.path().is_dir() {
                walk(root, &r, out)?;
            } else {
                out.push(r);
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(dir, "", &mut v)?;
    Ok(v)
}

/// `Rewind.exe --community-index <games folder>`: (re)write games/index.json for the repo.
pub fn write_index(dir: &Path) -> Result<usize> {
    use sha2::{Digest, Sha256};
    let mut games = Vec::new();
    let mut ents: Vec<_> = std::fs::read_dir(dir)?.flatten().filter(|e| e.path().is_dir()).collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let id = e.file_name().to_string_lossy().to_string();
        if !is_safe_id(&id) || !e.path().join("game_postprocess.lua").exists() || !e.path().join("events.json").exists() {
            continue;
        }
        let files = list_files(&e.path())?;
        let mut h = Sha256::new();
        for f in &files {
            h.update(f.as_bytes());
            h.update(std::fs::read(e.path().join(f))?);
        }
        let meta: Value = std::fs::read(e.path().join("game.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        games.push(json!({
            "id": id,
            "name": meta["name"].as_str().unwrap_or(&id),
            "authors": meta["authors"].clone(),
            "version": hex::encode(&h.finalize()[..8]),
            "files": files,
        }));
    }
    let n = games.len();
    std::fs::write(dir.join("index.json"), serde_json::to_vec_pretty(&json!({"format": 1, "games": games}))?)?;
    Ok(n)
}
