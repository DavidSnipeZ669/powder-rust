//! Installs powder-rs game packs into the ORIGINAL Powder desktop app (v8.x).
//!
//! Why earlier manual attempts failed: Powder registers a game in FOUR places and all must agree.
//!   1. ai-configs/whitelist.json                       - list of enabled game ids
//!   2. ai-configs/visual_cues/<ID>/                    - events.json + game_postprocess.lua (read by PowderRecorderCli)
//!   3. app.asar -> dist/main/main.js, supported-games  - `JSON.parse('{"games":[...]}')` (name, ids). A game
//!      missing here is silently dropped from the list. Bad fields (e.g. websiteUrl without a
//!      trailing slash) make the whole app fail validation at start-up.
//!   4. app.asar -> dist/main/main.js, visualCueModels  - event definitions bundled from every events.json at
//!      build time. Without it, events show up with fallback names / are filtered out.
//!   + assets/supported-games/<ID>/{banner/480x270x2.webp, cover/264x352x1.webp, icon-light-bg-color/96x96x1.svg}
//!     (exact file names - `bg-main.webp`/`poster.webp` are never loaded).

pub mod asar;

use crate::game::{find_game, GamePack};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use image::{imageops, Rgb, RgbImage};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum PowderCmd {
    /// Install (or update) a game pack into Powder
    Install {
        game: String,
        /// Powder folder (contains Powder.exe and resources/). Default: %LOCALAPPDATA%\Programs\powder-desktop
        #[arg(long)]
        powder: Option<PathBuf>,
    },
    /// Remove a game installed by powder-rs
    Uninstall {
        id: String,
        #[arg(long)]
        powder: Option<PathBuf>,
    },
    /// Show which custom games are installed
    Status {
        #[arg(long)]
        powder: Option<PathBuf>,
    },
    /// Restore Powder's original app.asar and whitelist.json from the backup powder-rs made
    Restore {
        #[arg(long)]
        powder: Option<PathBuf>,
    },
}

const MAIN_JS: &str = "dist/main/main.js";
const BACKUP_EXT: &str = "powder-rs-backup";

pub fn powder_dir(flag: Option<PathBuf>) -> Result<PathBuf> {
    let mut cands = Vec::new();
    if let Some(f) = flag.filter(|f| !f.as_os_str().is_empty()) {
        cands.push(f);
    }
    if let Ok(p) = std::env::var("POWDER_DIR") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        cands.push(Path::new(&l).join("Programs").join("powder-desktop"));
    }
    for c in &cands {
        if (c.join("resources").join("app.asar").is_file() || c.join("resources").join("app-unpacked").is_dir()) && c.join("ai-configs").is_dir() {
            return Ok(c.clone());
        }
    }
    bail!("Powder folder not found (tried {:?}). Pass --powder \"<folder with Powder.exe>\"", cands)
}

fn backup_once(p: &Path) -> Result<()> {
    let b = PathBuf::from(format!("{}.{BACKUP_EXT}", p.display()));
    if !b.exists() {
        std::fs::copy(p, &b).with_context(|| format!("backing up {}", p.display()))?;
        println!("  backup: {}", b.display());
    }
    Ok(())
}

// ---------- JS string helpers ----------
fn js_unescape_single(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let h: String = (0..4).filter_map(|_| it.next()).collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some('x') => {
                let h: String = (0..2).filter_map(|_| it.next()).collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

fn js_escape_single(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\'' => o.push_str("\\'"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\u{2028}' => o.push_str("\\u2028"),
            '\u{2029}' => o.push_str("\\u2029"),
            c => o.push(c),
        }
    }
    o
}

/// Byte range of the single-quoted literal content holding the supported-games JSON.
fn supported_games_range(js: &str) -> Result<(usize, usize)> {
    let marker = "JSON.parse('{\"games\":[";
    let s = js.find(marker).context("supported-games JSON not found in main.js (unsupported Powder version?)")? + "JSON.parse('".len();
    let b = js.as_bytes();
    let mut i = s;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\'' => return Ok((s, i)),
            _ => i += 1,
        }
    }
    bail!("unterminated supported-games literal")
}

/// Byte range (inclusive braces) of the bundled visualCueModels object literal.
fn cue_models_range(js: &str) -> Result<(usize, usize)> {
    let re = Regex::new(r#"let [A-Za-z_$][A-Za-z0-9_$]*=(\{)[A-Z0-9_]+:\{name:"[A-Z0-9_]+",events:\["#).unwrap();
    let m = re.captures(js).context("visualCueModels object not found in main.js (unsupported Powder version?)")?;
    let start = m.get(1).unwrap().start();
    let b = js.as_bytes();
    let (mut depth, mut i) = (0i64, start);
    let mut in_str: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                in_str = None;
            }
        } else {
            match c {
                b'"' | b'\'' | b'`' => in_str = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok((start, i));
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    bail!("unterminated visualCueModels object")
}

fn marker_re(id: &str) -> Regex {
    Regex::new(&format!(r"/\*powder-rs:{0}\*/.*?/\*/powder-rs:{0}\*/", regex::escape(id))).unwrap()
}

fn supported_game_entry(g: &GamePack) -> Value {
    let m = &g.meta;
    let mut e = json!({
        "name": m.name,
        "powderGameId": g.id,
        "urlSlug": m.url_slug.clone().unwrap_or_else(|| g.id.to_lowercase()),
        "description": m.description.clone().unwrap_or_else(|| format!("{} (custom game added with powder-rs)", m.name)),
        "productionDatabasePowderGameId": "",
        "developmentDatabasePowderGameId": "",
        "free2play": m.free2play,
    });
    let o = e.as_object_mut().unwrap();
    let mut libs = Vec::new();
    if let Some(s) = &m.steam_id {
        o.insert("steamId".into(), json!(s));
        libs.push("STEAM");
    }
    if let Some(s) = &m.epic_game_id {
        o.insert("epicGameId".into(), json!(s));
        libs.push("EPIC");
    }
    if !libs.is_empty() {
        o.insert("libraries".into(), json!(libs));
    }
    if let Some(t) = &m.twitch_id {
        o.insert("twitchId".into(), json!(t));
    }
    // Powder's own entries use the unvalidated "website" key; "websiteUrl" is strictly checked.
    if let Some(w) = &m.website_url {
        o.insert("website".into(), json!(w));
    }
    e
}

fn cue_model_entry(g: &GamePack) -> Value {
    let mut events = g.events["events"].clone();
    if let Some(arr) = events.as_array_mut() {
        for e in arr.iter_mut() {
            let o = e.as_object_mut().unwrap();
            let name = o.get("name").and_then(|v| v.as_str()).unwrap_or("event").to_string();
            o.entry("displayName").or_insert(json!(name));
            o.entry("icon").or_insert(json!("fps-kill"));
            o.entry("default").or_insert(json!(true));
            o.entry("instruction").or_insert(json!(format!("* {name} *")));
            o.entry("automontage").or_insert(json!({"primary": false, "secondary": false, "offsetBefore": 2, "offsetAfter": 2, "effectTypes": []}));
        }
    }
    json!({ "name": g.id, "events": events, "powderGameId": g.id, "isEventsConfigurationSupported": true })
}

/// Apply (or remove, when `game` is None) one game's registrations to main.js source.
fn patch_main_js(js: &str, id: &str, game: Option<&GamePack>) -> Result<String> {
    // supported games
    let (s, e) = supported_games_range(js)?;
    let mut sg: Value = serde_json::from_str(&js_unescape_single(&js[s..e])).context("parsing supported-games JSON")?;
    let games = sg["games"].as_array_mut().context("games array")?;
    games.retain(|g| g["powderGameId"].as_str() != Some(id));
    if let Some(g) = game {
        // keep Just Chatting etc. order; append before the trailing entry
        games.push(supported_game_entry(g));
    }
    let lit = js_escape_single(&serde_json::to_string(&sg)?);
    let mut out = String::with_capacity(js.len() + 8192);
    out.push_str(&js[..s]);
    out.push_str(&lit);
    out.push_str(&js[e..]);

    // visual cue models (event definitions)
    let out = marker_re(id).replace_all(&out, "").to_string();
    let (_, end) = cue_models_range(&out)?;
    let mut res = String::with_capacity(out.len() + 8192);
    res.push_str(&out[..end]);
    if let Some(g) = game {
        res.push_str(&format!("/*powder-rs:{id}*/,{}:{}/*/powder-rs:{id}*/", serde_json::to_string(id)?, serde_json::to_string(&cue_model_entry(g))?));
    }
    res.push_str(&out[end..]);
    Ok(res)
}

fn installed_ids(js: &str) -> Vec<String> {
    let re = Regex::new(r"/\*powder-rs:([A-Z0-9_]+)\*/").unwrap();
    let mut v: Vec<String> = re.captures_iter(js).map(|c| c[1].to_string()).collect();
    v.dedup();
    v
}

fn update_whitelist(powder: &Path, id: &str, add: bool) -> Result<()> {
    let p = powder.join("ai-configs").join("whitelist.json");
    backup_once(&p)?;
    let mut list: Vec<String> = serde_json::from_str(&std::fs::read_to_string(&p)?).context("whitelist.json")?;
    list.retain(|x| x != id);
    if add {
        list.push(id.to_string());
    }
    std::fs::write(&p, serde_json::to_string_pretty(&list)?)?;
    Ok(())
}

fn find_asset(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["png", "webp", "jpg", "jpeg", "svg"].iter().map(|e| dir.join(format!("{stem}.{e}"))).find(|p| p.exists())
}

fn placeholder(w: u32, h: u32, seed: &str) -> RgbImage {
    let hsh: u32 = seed.bytes().fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
    let (r, g, b) = ((hsh & 0xff) as f32, ((hsh >> 8) & 0xff) as f32, ((hsh >> 16) & 0xff) as f32);
    RgbImage::from_fn(w, h, |x, y| {
        let t = (x as f32 / w as f32 + y as f32 / h as f32) / 2.0;
        Rgb([(r * (1.0 - t) + 20.0 * t) as u8, (g * (1.0 - t) + 20.0 * t) as u8, (b * (1.0 - t) + 40.0 * t) as u8])
    })
}

fn write_webp(src: Option<PathBuf>, w: u32, h: u32, out: &Path, seed: &str) -> Result<()> {
    let img = match src.filter(|p| p.extension().map(|e| e != "svg").unwrap_or(true)) {
        Some(p) => {
            let i = image::open(&p).with_context(|| format!("opening {}", p.display()))?.to_rgb8();
            // cover-fit then centre crop
            let s = (w as f32 / i.width() as f32).max(h as f32 / i.height() as f32);
            let r = imageops::resize(&i, (i.width() as f32 * s).ceil() as u32, (i.height() as f32 * s).ceil() as u32, imageops::FilterType::Lanczos3);
            imageops::crop_imm(&r, (r.width() - w) / 2, (r.height() - h) / 2, w, h).to_image()
        }
        None => placeholder(w, h, seed),
    };
    std::fs::create_dir_all(out.parent().unwrap())?;
    let f = std::fs::File::create(out)?;
    image::codecs::webp::WebPEncoder::new_lossless(f).encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)?;
    Ok(())
}

fn write_icon(src: Option<PathBuf>, out: &Path, g: &GamePack) -> Result<()> {
    std::fs::create_dir_all(out.parent().unwrap())?;
    match src {
        Some(p) if p.extension().map(|e| e == "svg").unwrap_or(false) => {
            std::fs::copy(&p, out)?;
        }
        Some(p) => {
            let i = image::open(&p)?.to_rgba8();
            let i = imageops::resize(&i, 96, 96, imageops::FilterType::Lanczos3);
            let mut png = Vec::new();
            i.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(png);
            std::fs::write(out, format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96"><image width="96" height="96" href="data:image/png;base64,{b64}"/></svg>"#))?;
        }
        None => {
            let initials: String = g.meta.name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
            std::fs::write(out, format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96"><rect width="96" height="96" rx="18" fill="#2b2f3a"/><text x="48" y="60" font-family="Arial, sans-serif" font-size="38" font-weight="700" text-anchor="middle" fill="#fff">{initials}</text></svg>"##))?;
        }
    }
    Ok(())
}

fn copy_game_files(g: &GamePack, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for f in ["events.json", "game_postprocess.lua"] {
        std::fs::copy(g.dir.join(f), dst.join(f)).with_context(|| format!("copying {f}"))?;
    }
    // any extra lua modules the game ships
    for e in std::fs::read_dir(&g.dir)?.flatten() {
        let p = e.path();
        if p.extension().map(|x| x == "lua").unwrap_or(false) {
            std::fs::copy(&p, dst.join(e.file_name()))?;
        }
    }
    Ok(())
}

pub fn install(g: &GamePack, powder: &Path, lua_libs: &[PathBuf]) -> Result<()> {
    let libs: Vec<&Path> = lua_libs.iter().map(|p| p.as_path()).collect();
    let rep = crate::tools::validate(g, &libs);
    for w in &rep.warnings {
        println!("  warning: {w}");
    }
    if !rep.errors.is_empty() {
        for e in &rep.errors {
            println!("  ERROR: {e}");
        }
        bail!("fix the errors above before installing (powder-rs validate {})", g.id);
    }
    if g.dir.join("model.json").exists() {
        println!("  note: Powder can't load open model.onnx files (it only reads its encrypted .pwdmdl), so only OCR events will fire inside Powder. Use powder-rs analyze for model-based events.");
    }
    // Powder (Rust) reads custom games from %APPDATA%\powder-desktop\custom-games.json
    register_rust_app(g, true)?;
    println!("  registered in custom-games.json (Powder Rust app)");
    // the original Electron Powder needs its app.asar patched (skipped when there is no real app.asar,
    // e.g. an extracted copy that only has resources/app-unpacked)
    let asar_path = powder.join("resources").join("app.asar");
    if asar_path.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
        backup_once(&asar_path)?;
        let mut a = asar::Asar::open(&asar_path)?;
        let js = String::from_utf8(a.read(MAIN_JS)?).context("main.js utf8")?;
        let patched = patch_main_js(&js, &g.id, Some(g))?;
        a.write(MAIN_JS, patched.into_bytes());
        a.save(&asar_path)?;
        println!("  patched app.asar (supported games + event definitions)");
    }

    copy_game_files(g, &powder.join("ai-configs").join("visual_cues").join(&g.id))?;
    println!("  copied events.json + game_postprocess.lua to ai-configs/visual_cues/{}", g.id);
    update_whitelist(powder, &g.id, true)?;
    println!("  whitelisted {}", g.id);

    let assets = powder.join("assets").join("supported-games").join(&g.id);
    let src = g.dir.join("assets");
    write_webp(find_asset(&src, "banner"), 960, 540, &assets.join("banner").join("480x270x2.webp"), &g.id)?;
    write_webp(find_asset(&src, "cover"), 264, 352, &assets.join("cover").join("264x352x1.webp"), &g.id)?;
    write_icon(find_asset(&src, "icon"), &assets.join("icon-light-bg-color").join("96x96x1.svg"), g)?;
    println!("  wrote artwork to assets/supported-games/{}", g.id);
    Ok(())
}

/// Add/remove a game in the Rust app's registry (%APPDATA%\powder-desktop\custom-games.json).
pub fn register_rust_app(g: &GamePack, add: bool) -> Result<()> {
    let path = dirs::config_dir().context("no config dir")?.join("powder-desktop").join("custom-games.json");
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({"games": []}));
    let list = v["games"].as_array_mut().context("games")?;
    list.retain(|x| x["powderGameId"].as_str() != Some(&g.id));
    if add {
        let mut e = supported_game_entry(g);
        e["visualCueModel"] = cue_model_entry(g);
        list.push(e);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

fn unregister_rust_app(id: &str) {
    if let Some(path) = dirs::config_dir().map(|d| d.join("powder-desktop").join("custom-games.json")) {
        if let Some(mut v) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            if let Some(l) = v["games"].as_array_mut() {
                l.retain(|x| x["powderGameId"].as_str() != Some(id));
            }
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default());
        }
    }
}

pub fn uninstall(id: &str, powder: &Path) -> Result<()> {
    unregister_rust_app(id);
    let asar_path = powder.join("resources").join("app.asar");
    if !asar_path.metadata().map(|m| m.len() > 1000).unwrap_or(false) {
        update_whitelist(powder, id, false)?;
        let _ = std::fs::remove_dir_all(powder.join("ai-configs").join("visual_cues").join(id));
        let _ = std::fs::remove_dir_all(powder.join("assets").join("supported-games").join(id));
        println!("  removed {id}");
        return Ok(());
    }
    let mut a = asar::Asar::open(&asar_path)?;
    let js = String::from_utf8(a.read(MAIN_JS)?)?;
    if !installed_ids(&js).contains(&id.to_string()) {
        println!("  {id} was not installed by powder-rs; leaving Powder's built-in data alone");
        return Ok(());
    }
    a.write(MAIN_JS, patch_main_js(&js, id, None)?.into_bytes());
    a.save(&asar_path)?;
    update_whitelist(powder, id, false)?;
    let vc = powder.join("ai-configs").join("visual_cues").join(id);
    let _ = std::fs::remove_dir_all(&vc);
    let _ = std::fs::remove_dir_all(powder.join("assets").join("supported-games").join(id));
    println!("  removed {id}");
    Ok(())
}

pub fn run(cmd: PowderCmd, game_dirs: &[PathBuf], lua_libs: &[PathBuf]) -> Result<()> {
    match cmd {
        PowderCmd::Install { game, powder } => {
            let p = powder_dir(powder)?;
            let g = find_game(&game, game_dirs)?;
            println!("installing {} ({}) into {}", g.id, g.meta.name, p.display());
            install(&g, &p, lua_libs)?;
            println!("done. Close Powder fully (also from the tray) and start it again.");
        }
        PowderCmd::Uninstall { id, powder } => uninstall(&id, &powder_dir(powder)?)?,
        PowderCmd::Status { powder } => {
            let p = powder_dir(powder)?;
            let a = asar::Asar::open(&p.join("resources").join("app.asar"))?;
            let js = String::from_utf8(a.read(MAIN_JS)?)?;
            let ids = installed_ids(&js);
            let wl: Vec<String> = serde_json::from_str(&std::fs::read_to_string(p.join("ai-configs").join("whitelist.json"))?)?;
            println!("Powder at {}", p.display());
            println!("  {} whitelisted games; custom (powder-rs): {}", wl.len(), if ids.is_empty() { "none".into() } else { ids.join(", ") });
        }
        PowderCmd::Restore { powder } => {
            let p = powder_dir(powder)?;
            for f in [p.join("resources").join("app.asar"), p.join("ai-configs").join("whitelist.json")] {
                let b = PathBuf::from(format!("{}.{BACKUP_EXT}", f.display()));
                if b.exists() {
                    std::fs::copy(&b, &f)?;
                    println!("  restored {}", f.display());
                } else {
                    println!("  no backup for {}", f.display());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escape_roundtrip() {
        let s = r#"{"a":"it's \\ fine","b":"x\ny"}"#;
        assert_eq!(js_unescape_single(&js_escape_single(s)), s);
    }
}
