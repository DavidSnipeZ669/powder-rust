//! Port of src/main/features/vertical-format: 9:16 crop templates (Powder's defaults in assets/vertical
//! plus user templates in Feature Storage/verticalCrops.json).
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

static ROOT: OnceLock<PathBuf> = OnceLock::new();

fn vertical_dir() -> PathBuf {
    ROOT.get().cloned().unwrap_or_default().join("assets").join("vertical")
}
fn storage() -> PathBuf {
    super::user_data().join("Feature Storage").join("verticalCrops.json")
}
fn read_json(p: &std::path::Path) -> Option<Value> {
    let t = std::fs::read_to_string(p).ok()?;
    serde_json::from_str(t.trim_start_matches('\u{feff}')).ok()
}
fn custom() -> Map<String, Value> {
    read_json(&storage()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
fn game(id: &str) -> Option<Value> {
    super::games::find(id)
}

fn defaults() -> Map<String, Value> {
    let mut out = Map::new();
    let Ok(rd) = std::fs::read_dir(vertical_dir()) else { return out };
    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for n in names {
        let Some(mut data) = read_json(&vertical_dir().join(&n).join("default.json")) else { continue };
        let g = game(&n);
        data["name"] = g.as_ref().map(|g| g["name"].clone()).unwrap_or(json!("default"));
        data["icon"] = g.as_ref().map(|g| g["iconUrl"].clone()).unwrap_or(Value::Null);
        data["id"] = json!(n);
        out.insert(n, data);
    }
    out
}

pub fn list() -> Value {
    let defs = defaults();
    let mut list = defs.clone();
    for (k, mut v) in custom() {
        let g = game(&k);
        let name = v["name"].as_str().map(String::from).or_else(|| g.as_ref().and_then(|g| g["name"].as_str().map(String::from))).unwrap_or_else(|| if k == "default" { "default".into() } else { "Unknown".into() });
        v["name"] = json!(name);
        v["createdByUser"] = json!(!defs.contains_key(&k));
        v["icon"] = g.map(|g| g["iconUrl"].clone()).unwrap_or(Value::Null);
        v["id"] = json!(k);
        list.insert(k, v);
    }
    // "default" first
    let mut ordered = Map::new();
    if let Some((k, v)) = list.iter().find(|(_, v)| v["name"] == "default") {
        ordered.insert(k.clone(), v.clone());
    }
    for (k, v) in list {
        ordered.entry(k).or_insert(v);
    }
    Value::Object(ordered)
}

/// readVerticalFormatTemplateByPowderGameId
pub fn template_for(game: Option<&str>) -> Option<Value> {
    let name = game.unwrap_or("default");
    if let Some(t) = custom().get(name) {
        return Some(t.clone());
    }
    let p = game.map(|g| vertical_dir().join(g).join("default.json")).filter(|p| p.exists()).unwrap_or_else(|| vertical_dir().join("default").join("default.json"));
    read_json(&p)
}

fn write(id: &str, t: Value) -> Result<Value> {
    let mut c = custom();
    c.insert(id.to_string(), t.clone());
    super::core::write_json(&storage(), &Value::Object(c))?;
    Ok(t)
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    let _ = ROOT.set(ctx.dist.root.clone());
    h.set("vertical-format", "templatesById", list());
    h.set("vertical-format", "templatesIdsByExtractionProjectId", json!({}));
    h.handle("vertical-format", "saveVerticalFormatTemplateByTemplateId", |h, a| {
        let t = write(a.first().and_then(|v| v.as_str()).unwrap_or("default"), a.get(1).cloned().unwrap_or(json!({})))?;
        h.set("vertical-format", "templatesById", list());
        Ok(t)
    });
    h.handle("vertical-format", "deleteVerticalFormatTemplateByTemplateId", |h, a| {
        let mut c = custom();
        c.remove(a.first().and_then(|v| v.as_str()).unwrap_or(""));
        super::core::write_json(&storage(), &Value::Object(c))?;
        h.set("vertical-format", "templatesById", list());
        Ok(Value::Null)
    });
    h.handle("vertical-format", "resetDefaultVerticalTemplateById", |h, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let t = defaults().get(id).cloned().with_context(|| format!("Default template {id} not found"))?;
        write(id, t.clone())?;
        h.set("vertical-format", "templatesById", list());
        Ok(t)
    });
}
