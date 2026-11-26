//! Powder's per-game event configuration (visualCueModels in main.js): event names, display names,
//! icons, scores and automontage rules for every supported game. Custom games add theirs from
//! %APPDATA%\powder-desktop\custom-games.json ("visualCueModel" on each game).
use serde_json::Value;
use std::sync::OnceLock;

static CUES: OnceLock<parking_lot::RwLock<serde_json::Map<String, Value>>> = OnceLock::new();

pub fn load(main_js: &str) {
    load_lists(main_js);
    let mut map = serde_json::Map::new();
    let re = regex_lite_find(main_js);
    if let Some(start) = re {
        if let Some(end) = crate::jslit::balanced(main_js, start) {
            match crate::jslit::to_json(&main_js[start..=end]) {
                Ok(Value::Object(m)) => map = m,
                Ok(_) => {}
                Err(e) => crate::log(&format!("visualCueModels: {e:#}")),
            }
        }
    }
    for g in super::games::custom_games() {
        if let (Some(id), Some(m)) = (g["powderGameId"].as_str(), g.get("visualCueModel")) {
            map.insert(id.to_string(), m.clone());
        }
    }
    crate::log(&format!("event configs for {} games", map.len()));
    match CUES.get() {
        Some(l) => *l.write() = map,
        None => {
            let _ = CUES.set(parking_lot::RwLock::new(map));
        }
    }
}

/// `let X={AMNGU:{name:"AMNGU",events:[` -> index of the first '{'
fn regex_lite_find(js: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(p) = js[from..].find(":{name:\"") {
        let at = from + p;
        // walk back over the game id and the '{' and '=' of `let X={ID:`
        let bytes = js.as_bytes();
        let mut k = at;
        while k > 0 && (bytes[k - 1].is_ascii_uppercase() || bytes[k - 1].is_ascii_digit() || bytes[k - 1] == b'_') {
            k -= 1;
        }
        if k > 0 && bytes[k - 1] == b'{' && js[at..].starts_with(&format!(":{{name:\"{}\",events:[", &js[k..at])) {
            let before = &js[k.saturating_sub(40)..k - 1];
            if before.trim_end().ends_with('=') && before.contains("let ") {
                return Some(k - 1);
            }
        }
        from = at + 1;
    }
    None
}

pub fn model(powder_game_id: &str) -> Option<Value> {
    CUES.get()?.read().get(powder_game_id).cloned()
}

#[allow(dead_code)] // kept for upcoming features
pub fn all() -> serde_json::Map<String, Value> {
    CUES.get().map(|l| l.read().clone()).unwrap_or_default()
}

static AUDIO: OnceLock<Vec<Value>> = OnceLock::new();
static OTHER: OnceLock<Vec<Value>> = OnceLock::new();

fn list_at(main_js: &str, marker: &str) -> Vec<Value> {
    let Some(i) = main_js.find(marker) else { return vec![] };
    let start = i + marker.find('[').unwrap_or(0);
    crate::jslit::balanced(main_js, start).and_then(|end| crate::jslit::to_json(&main_js[start..=end]).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// getAudioEvents / getOtherEvents from the ai-configs virtual module
pub fn load_lists(main_js: &str) {
    let _ = AUDIO.set(list_at(main_js, "=[{name:\"laughing\""));
    let _ = OTHER.set(list_at(main_js, "=[{name:\"bookmark\",displayName:\"Manual highlight\""));
}
pub fn audio_events() -> Vec<Value> {
    AUDIO.get().cloned().unwrap_or_default()
}
pub fn other_events() -> Vec<Value> {
    OTHER.get().cloned().unwrap_or_default()
}

/// ai-events bridge
pub fn init(h: &crate::host::Host) {
    h.handle("ai-events", "getAIEventsByCategoriesFromPowderGameId", |_, a| {
        let g = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let visual = model(g).map(|m| m["events"].clone()).filter(|v| v.is_array()).unwrap_or(serde_json::json!([]));
        let prem = |l: Vec<Value>| l.into_iter().map(|mut e| { e["premium"] = serde_json::json!(true); e }).collect::<Vec<_>>();
        Ok(serde_json::json!({"VISUAL": visual, "AUDIO": prem(audio_events()), "OTHERS": prem(other_events())}))
    });
}
