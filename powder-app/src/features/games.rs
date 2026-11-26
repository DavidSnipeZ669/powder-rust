//! Ports of src/main/features/local-game-db + games.
//! Official games come from the list bundled in Powder's main.js and the engine's ai-configs/whitelist.json
//! (exactly what the original reads). Custom games are added from %APPDATA%\powder-desktop\custom-games.json.

use crate::host::Host;
use crate::server::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Unescape the single-quoted JS literal Powder's bundler emits.
fn js_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let h: String = it.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
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

pub fn bundled_games(main_js: &str) -> Result<Vec<Value>> {
    let marker = "JSON.parse('{\"games\":[";
    let s = main_js.find(marker).context("supported games list not found in main.js")? + "JSON.parse('".len();
    let b = main_js.as_bytes();
    let mut i = s;
    while i < b.len() && b[i] != b'\'' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    let v: Value = serde_json::from_str(&js_unescape(&main_js[s..i]))?;
    Ok(v["games"].as_array().cloned().unwrap_or_default())
}

pub fn custom_games_path() -> PathBuf {
    super::user_data().join("custom-games.json")
}

pub fn custom_games() -> Vec<Value> {
    std::fs::read_to_string(custom_games_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["games"].as_array().cloned())
        .unwrap_or_default()
}

/// Folder that holds ai-configs/ (the original walks up from the working dir).
pub fn ai_configs(ctx: &Ctx) -> Option<PathBuf> {
    let starts = [ctx.dist.root.clone(), ctx.dist.native.clone()];
    for s in starts {
        let mut cur: Option<&Path> = Some(&s);
        while let Some(c) = cur {
            if c.join("ai-configs").is_dir() {
                return Some(c.join("ai-configs"));
            }
            cur = c.parent();
        }
    }
    None
}

pub struct Db {
    pub games: Vec<Value>, // normalized
}

static DB: OnceLock<parking_lot::RwLock<Db>> = OnceLock::new();

pub fn db() -> parking_lot::RwLockReadGuard<'static, Db> {
    DB.get().expect("game db").read()
}

fn normalize(ctx: &Ctx, g: &Value) -> Value {
    let id = g["powderGameId"].as_str().unwrap_or("");
    let asset = |p: &str| crate::protocols::url(&ctx.token, "powder-game-assets", &format!("supported-games/{id}/{p}"));
    json!({
        "name": g["name"], "input": g,
        "urn": format!("pwd:game:whichgame:{}", crate::protocols::encode(id)),
        "powderGameId": id, "steamId": g["steamId"], "twitchId": g["twitchId"], "epicGameId": g["epicGameId"],
        "battlenetId": g["battlenetId"], "libraries": g["libraries"], "cannotInstallOnSteam": g["cannotInstallOnSteam"],
        "description": g["description"], "websiteUrl": g["websiteUrl"], "discordUrl": g["discordUrl"], "twitterUrl": g["twitterUrl"],
        "bannerUrl": asset("banner/480x270x2.webp"),
        "iconUrl": asset("icon-light-bg-color/96x96x1.svg"),
        "coverUrl": asset("cover/264x352x1.webp"),
        "databasePowderGameId": g["productionDatabasePowderGameId"],
        "detectedGameEventLabels": [], "shareUrl": null, "npu": g["npu"],
    })
}

pub fn find(id: &str) -> Option<Value> {
    db().games.iter().find(|g| g["powderGameId"] == id).cloned()
}

pub fn load(ctx: &Ctx) -> Result<()> {
    let main_js = String::from_utf8(ctx.dist.read("dist/main/main.js").context("dist/main/main.js missing")?)?;
    super::cues::load(&main_js);
    let mut raw = bundled_games(&main_js)?;
    for c in custom_games() {
        let id = c["powderGameId"].as_str().unwrap_or("").to_string();
        raw.retain(|g| g["powderGameId"] != id.as_str());
        raw.push(c);
    }
    let games = raw.iter().map(|g| normalize(ctx, g)).collect();
    match DB.get() {
        Some(l) => l.write().games = games,
        None => {
            let _ = DB.set(parking_lot::RwLock::new(Db { games }));
        }
    }
    Ok(())
}

/// supportedGames = whitelist.json order, games without db entry dropped, Just Chatting first.
pub fn supported(ctx: &Ctx) -> Vec<Value> {
    let mut ids: Vec<String> = ai_configs(ctx)
        .and_then(|a| std::fs::read_to_string(a.join("whitelist.json")).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for c in custom_games() {
        if let Some(id) = c["powderGameId"].as_str() {
            if !ids.iter().any(|x| x == id) {
                ids.push(id.to_string());
            }
        }
    }
    let mut out: Vec<Value> = ids
        .iter()
        .filter_map(|id| find(id))
        .map(|g| json!({"name": g["name"], "powderGameId": g["powderGameId"], "bannerUrl": g["bannerUrl"], "iconUrl": g["iconUrl"], "coverUrl": g["coverUrl"]}))
        .collect();
    if let Some(jc) = find("JSTCHT") {
        out.insert(0, jc);
    }
    out
}

pub fn refresh(h: &Host, ctx: &Ctx) {
    if let Err(e) = load(ctx) {
        crate::log(&format!("game db: {e:#}"));
        let _ = DB.set(parking_lot::RwLock::new(Db { games: vec![] }));
    }
    h.set("games", "supportedGames", Value::Array(supported(ctx)));
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    refresh(h, ctx);
    let root = ctx.dist.root.clone();
    crate::protocols::register("powder-game-assets", move |rest| {
        let rest = rest.trim_start_matches('/');
        // custom game artwork first, then Powder's own assets folder
        let custom = super::user_data().join("custom-games-assets").join(rest.trim_start_matches("supported-games/"));
        if custom.is_file() {
            return Some(custom);
        }
        let p = root.join("assets").join(rest);
        p.is_file().then_some(p)
    });
    h.handle("games", "getGameByPowderGameId", |h, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        Ok(h.get("games", "supportedGames").and_then(|l| l.as_array().and_then(|a| a.iter().find(|g| g["powderGameId"] == id).cloned())).unwrap_or(Value::Null))
    });
    h.handle("games", "getNpuGames", |_, _| Ok(Value::Array(db().games.iter().filter(|g| g["npu"] == true).cloned().collect())));
}
