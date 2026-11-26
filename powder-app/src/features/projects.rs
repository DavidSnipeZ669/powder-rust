//! Port of src/main/features/extraction-projects: every recording/import is a JSON project file in
//! %APPDATA%\powder-desktop\Feature Storage\Extraction Projects\<id>.json (same files as Powder).
//! `query()` mirrors the original `find()`: a filter + formatter kept in sync with a bridge variable.

use super::core::{now_iso, write_json};
use crate::host::Host;
use crate::server::Ctx;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

pub fn folder() -> PathBuf {
    super::user_data().join("Feature Storage").join("Extraction Projects")
}

fn file(id: &str) -> PathBuf {
    folder().join(format!("{id}.json"))
}

type Filter = Box<dyn Fn(&Value) -> bool + Send + Sync>;
type Format = Box<dyn Fn(Vec<Value>) -> Value + Send + Sync>;

struct Query {
    filter: Filter,
    format: Format,
    bridge: &'static str,
    var: &'static str,
    last: Value,
}

pub enum Change {
    Created(String),
    Updated(String, Value),
    Removed(String),
    BookmarkAdded(String),
}

pub struct Projects {
    cache: Mutex<BTreeMap<String, Value>>,
    queries: Mutex<Vec<Query>>,
    listeners: Mutex<Vec<Box<dyn Fn(&Change) + Send + Sync>>>,
    ctx: Arc<Ctx>,
}

static P: OnceLock<Arc<Projects>> = OnceLock::new();

pub fn get() -> &'static Arc<Projects> {
    P.get().expect("projects")
}

fn uuid() -> String {
    let t = crate::server::random_token();
    format!("{}-{}-4{}-a{}-{}", &t[0..8], &t[8..12], &t[13..16], &t[17..20], &t[20..32])
}

impl Projects {
    /// read(): storage file + derived fields (title, posterUrl, videoUrl, videoMetadata, game ...)
    pub fn read(&self, id: &str) -> Result<Option<Value>> {
        let p = file(id);
        if !p.exists() {
            return Ok(None);
        }
        let st: Value = serde_json::from_str(&std::fs::read_to_string(&p)?).with_context(|| format!("{id}.json"))?;
        if st.get("id").is_none() {
            return Ok(Some(Value::Null)); // legacy format
        }
        let video_path = st["videoPath"].as_str().unwrap_or("").to_string();
        let has_video = !video_path.is_empty();
        let exists = has_video && std::fs::metadata(&video_path).map(|m| m.len() > 0).unwrap_or(false);
        let file_ok = exists || (!has_video && !st["rawResult"].is_null());
        let meta = if exists { crate::media::metadata(std::path::Path::new(&video_path)).ok() } else { None };
        let duration_sec = match &meta {
            Some(m) if m.duration_ms > 0 => m.duration_ms as f64 / 1000.0,
            _ => st["durationAtCreation"].as_f64().map(|d| d / 1000.0).or_else(|| st["remoteDurationSec"].as_f64()).unwrap_or(0.0),
        };
        let game = st["powderGameId"].as_str().and_then(|g| {
            self.ctx.host.get("games", "supportedGames").and_then(|l| l.as_array().and_then(|a| a.iter().find(|x| x["powderGameId"] == g).cloned()))
        });
        let mut p = st.clone();
        let title = st["title"].as_str().filter(|t| !t.is_empty()).map(String::from).or_else(|| {
            has_video.then(|| std::path::Path::new(&video_path).file_name().map(|f| f.to_string_lossy().to_string())).flatten()
        });
        p["title"] = json!(title.unwrap_or_else(|| "Untitled".into()));
        p["posterUrl"] = if has_video { json!(super::library::poster_url(&self.ctx, &video_path)) } else { st["remotePosterUrl"].clone() };
        p["videoUrl"] = if file_ok && has_video { json!(super::library::video_url(&self.ctx, &video_path)) } else { Value::Null };
        p["videoMetadata"] = meta.map(|m| m.to_json()).unwrap_or(Value::Null);
        p["durationSec"] = json!(duration_sec);
        p["game"] = game.unwrap_or(Value::Null);
        p["automontageEventsCount"] = json!(automontage_events_count(&st));
        p["missingVideoFile"] = json!(has_video && !exists);
        let changed = {
            let mut c = self.cache.lock();
            let old = c.insert(id.to_string(), p.clone());
            old.as_ref() != Some(&p)
        };
        if changed {
            self.refresh_queries();
            self.ctx.host.set("extraction-projects", "extractionProjectsCount", json!(self.cache.lock().len()));
        }
        Ok(Some(p))
    }

    pub fn create(&self, required: Value, optional: Option<Value>) -> Result<Value> {
        let id = uuid();
        let now = now_iso();
        let mut st = required.as_object().cloned().unwrap_or_default();
        st.insert("id".into(), json!(id));
        st.insert("userId".into(), json!("a682ee57-66cb-4155-a48a-6ff6d8c56d8b"));
        st.insert("status".into(), json!("NOT_ANALYZED"));
        st.insert("unwatched".into(), json!(true));
        st.insert("recordingSessionId".into(), Value::Null);
        st.insert("createdAt".into(), optional.as_ref().and_then(|o| o.get("createdAt").cloned()).unwrap_or(json!(now)));
        st.insert("updatedAt".into(), json!(now));
        if let Some(Value::Object(o)) = optional {
            for (k, v) in o {
                st.insert(k, v);
            }
        }
        write_json(&file(&id), &Value::Object(st))?;
        let p = self.read(&id)?.unwrap_or(Value::Null);
        self.emit(Change::Created(id));
        Ok(p)
    }

    pub fn update(&self, id: &str, data: Value, replace: bool) -> Result<()> {
        let cur: Value = serde_json::from_str(&std::fs::read_to_string(file(id)).context("Extraction project not found")?)?;
        if data.is_null() {
            return Ok(());
        }
        let mut upd = if replace { data.clone() } else { merge(cur.clone(), data.clone()) };
        upd["updatedAt"] = json!(now_iso());
        let n_old = cur["cartMomentsById"].as_object().map(|m| m.len()).unwrap_or(0);
        let n_new = upd["cartMomentsById"].as_object().map(|m| m.len()).unwrap_or(0);
        if n_new > n_old {
            upd["lastBookmarkAt"] = json!(now_iso());
            self.emit(Change::BookmarkAdded(id.to_string()));
        }
        write_json(&file(id), &upd)?;
        self.read(id)?;
        self.emit(Change::Updated(id.to_string(), data));
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _ = std::fs::remove_file(file(id));
        self.cache.lock().remove(id);
        self.refresh_queries();
        self.ctx.host.set("extraction-projects", "extractionProjectsCount", json!(self.cache.lock().len()));
        self.emit(Change::Removed(id.to_string()));
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Option<Value> {
        if let Some(v) = self.cache.lock().get(id) {
            return Some(v.clone());
        }
        self.read(id).ok().flatten()
    }

    /// raw storage file (without derived fields)
    pub fn storage(&self, id: &str) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(file(id)).ok()?).ok()
    }

    pub fn all(&self) -> Vec<Value> {
        self.cache.lock().values().cloned().collect()
    }

    pub fn on_change<F: Fn(&Change) + Send + Sync + 'static>(&self, f: F) {
        self.listeners.lock().push(Box::new(f));
    }

    fn emit(&self, c: Change) {
        for l in self.listeners.lock().iter() {
            l(&c);
        }
    }

    /// find(filter, variable, {formatterFn})
    pub fn query<F, G>(&self, bridge: &'static str, var: &'static str, filter: F, format: G)
    where
        F: Fn(&Value) -> bool + Send + Sync + 'static,
        G: Fn(Vec<Value>) -> Value + Send + Sync + 'static,
    {
        let items: Vec<Value> = self.all().into_iter().filter(|p| filter(p)).collect();
        let v = format(items);
        self.ctx.host.set(bridge, var, v.clone());
        self.queries.lock().push(Query { filter: Box::new(filter), format: Box::new(format), bridge, var, last: v });
    }

    pub fn refresh_queries(&self) {
        let all = self.all();
        let mut qs = self.queries.lock();
        for q in qs.iter_mut() {
            let items: Vec<Value> = all.iter().filter(|p| (q.filter)(p)).cloned().collect();
            let v = (q.format)(items);
            if v != q.last {
                q.last = v.clone();
                self.ctx.host.set(q.bridge, q.var, v);
            }
        }
    }
}

pub fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(ao), Value::Object(bo)) = (a.as_object_mut(), b) {
        for (k, v) in bo {
            ao.insert(k, v);
        }
    }
    a
}

/// montage-maker/lib/canCreateAutomontage: number of usable events (ai events + bookmarks)
pub fn automontage_events_count(st: &Value) -> usize {
    let (Some(events), Some(game)) = (st["rawResult"]["videoEvents"].as_array(), st["powderGameId"].as_str()) else { return 0 };
    let cfg = super::cues::model(game).and_then(|m| m["events"].as_array().cloned()).unwrap_or_default();
    events.iter().filter(|e| cfg.iter().any(|c| c["name"] == e["eventName"] && c["automontage"]["primary"] == true)).count()
}

/// Read every project file in the background (the window opens straight away; lists fill in as they load).
pub fn load_all() {
    std::thread::spawn(|| {
        let Ok(rd) = std::fs::read_dir(folder()) else { return };
        let mut ids: Vec<(std::time::SystemTime, String)> = rd
            .flatten()
            .filter_map(|e| {
                let id = e.file_name().to_string_lossy().strip_suffix(".json").map(String::from)?;
                Some((e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), id))
            })
            .collect();
        ids.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
        let ids: Vec<String> = ids.into_iter().map(|x| x.1).collect();
        // a few at a time: probing videos is I/O bound
        let chunks: Vec<Vec<String>> = ids.chunks(ids.len().div_ceil(4).max(1)).map(|c| c.to_vec()).collect();
        let hs: Vec<_> = chunks
            .into_iter()
            .map(|c| {
                std::thread::spawn(move || {
                    for id in c {
                        if let Err(e) = get().read(&id) {
                            crate::log(&format!("project {id}: {e:#}"));
                        }
                    }
                })
            })
            .collect();
        for h in hs {
            let _ = h.join();
        }
        crate::log(&format!("{} projects loaded", get().all().len()));
    });
}

pub fn init(h: &Host, ctx: &Arc<Ctx>) {
    let p = Arc::new(Projects { cache: Mutex::new(BTreeMap::new()), queries: Mutex::new(vec![]), listeners: Mutex::new(vec![]), ctx: ctx.clone() });
    let _ = P.set(p.clone());
    h.set("extraction-projects", "extractionProjectsCount", json!(0));
    let _ = std::fs::create_dir_all(folder());
    h.handle("extraction-projects", "create", |_, a| {
        let o = a.into_iter().next().unwrap_or(Value::Null);
        get().create(o["requiredData"].clone(), o.get("optionalData").cloned().filter(|v| !v.is_null()))
    });
    h.handle("extraction-projects", "read", |_, a| Ok(get().read(a.first().and_then(|v| v.as_str()).unwrap_or(""))?.unwrap_or(Value::Null)));
    h.handle("extraction-projects", "update", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let replace = a.get(2).map(|o| o["replace"] == true).unwrap_or(false);
        get().update(&id, a.get(1).cloned().unwrap_or(Value::Null), replace)?;
        Ok(Value::Null)
    });
    h.handle("extraction-projects", "delete", |_, a| {
        get().delete(a.first().and_then(|v| v.as_str()).unwrap_or(""))?;
        Ok(Value::Null)
    });
    let _: Option<Map<String, Value>> = None;
}
