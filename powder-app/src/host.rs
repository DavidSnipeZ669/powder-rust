//! Rust replacement for Electron's ipcMain + Powder's `bridge/main.ts`.
//! Functions answer `{data}` / `{error}`, variables keep a last value and broadcast changes,
//! streams broadcast events, exactly like the original.

use anyhow::Result;
use parking_lot::{Condvar, Mutex};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub type Handler = Arc<dyn Fn(&Host, Vec<Value>) -> Result<Value> + Send + Sync>;

#[derive(Clone)]
struct BridgeInfo {
    name: String,
    id: String,
}

#[derive(Default)]
struct Events {
    seq: u64,
    queue: Vec<(u64, String, Value)>,
}

pub struct Host {
    bridges: HashMap<String, BridgeInfo>, // by name
    fn_channels: HashMap<String, (String, String)>, // channel -> (bridge, function)
    var_fetch: HashMap<String, (String, String)>,
    handlers: Mutex<HashMap<String, Handler>>, // "bridge.function"
    vars: Mutex<HashMap<String, Value>>,       // "bridge.variable" (missing = undefined)
    events: Mutex<Events>,
    cv: Condvar,
    pub bridges_key: String,
    pub window: crate::window::WindowCtl,
}

pub enum Reply {
    Json(Value),
    Undefined,
}

impl Host {
    pub fn new(manifest: &Value, window: crate::window::WindowCtl) -> Arc<Host> {
        let mut bridges = HashMap::new();
        let mut fn_channels = HashMap::new();
        let mut var_fetch = HashMap::new();
        for b in manifest["bridges"].as_array().unwrap() {
            let name = b["name"].as_str().unwrap().to_string();
            bridges.insert(name.clone(), BridgeInfo { name: name.clone(), id: b["id"].as_str().unwrap().into() });
            for f in b["functions"].as_array().unwrap() {
                fn_channels.insert(f["channel"].as_str().unwrap().into(), (name.clone(), f["name"].as_str().unwrap().into()));
            }
            for v in b["variables"].as_array().unwrap() {
                var_fetch.insert(v["fetch"].as_str().unwrap().into(), (name.clone(), v["name"].as_str().unwrap().into()));
            }
        }
        Arc::new(Host {
            bridges,
            fn_channels,
            var_fetch,
            handlers: Mutex::new(HashMap::new()),
            vars: Mutex::new(HashMap::new()),
            events: Mutex::new(Events::default()),
            cv: Condvar::new(),
            bridges_key: manifest["bridgesVariableKey"].as_str().unwrap().into(),
            window,
        })
    }

    /// Register a function implementation: `host.handle("settings", "setTheme", |h, args| ...)`.
    pub fn handle<F>(&self, bridge: &str, func: &str, f: F)
    where
        F: Fn(&Host, Vec<Value>) -> Result<Value> + Send + Sync + 'static,
    {
        self.handlers.lock().insert(format!("{bridge}.{func}"), Arc::new(f));
    }

    pub fn get(&self, bridge: &str, var: &str) -> Option<Value> {
        self.vars.lock().get(&format!("{bridge}.{var}")).cloned()
    }

    /// Variable.next(value): store and broadcast.
    pub fn set(&self, bridge: &str, var: &str, value: Value) {
        self.vars.lock().insert(format!("{bridge}.{var}"), value.clone());
        if bridge == "settings" && var == "theme" {
            // theme feature: the original switched classes on <html> from the main process
            self.emit("__pw.theme".into(), json!({ "value": value }));
        }
        if let Some(b) = self.bridges.get(bridge) {
            self.emit(format!("{}.{}--variable--{}", b.name, var, b.id), json!({ "value": value }));
        }
    }

    /// stream.next(value)
    pub fn stream(&self, bridge: &str, stream: &str, value: Value) {
        if let Some(b) = self.bridges.get(bridge) {
            self.emit(format!("{}.{}--stream--{}", b.name, stream, b.id), json!({ "value": value }));
        }
    }

    pub fn emit(&self, channel: String, payload: Value) {
        let mut e = self.events.lock();
        e.seq += 1;
        let s = e.seq;
        e.queue.push((s, channel, payload));
        if e.queue.len() > 5000 {
            let n = e.queue.len() - 5000;
            e.queue.drain(..n);
        }
        self.cv.notify_all();
    }

    /// Long-poll: events after `since`, waiting up to 25 s for new ones.
    pub fn events_since(&self, since: u64) -> Vec<Value> {
        let mut e = self.events.lock();
        if e.seq <= since {
            self.cv.wait_for(&mut e, Duration::from_secs(25));
        }
        e.queue.iter().filter(|(s, _, _)| *s > since).map(|(s, c, p)| json!({"seq": s, "channel": c, "payload": p})).collect()
    }

    pub fn current_seq(&self) -> u64 {
        self.events.lock().seq
    }

    fn bridges_value(&self) -> Value {
        let list: Vec<Value> = self.bridges.values().map(|b| json!({"key": format!("{}--{}", b.name, b.id), "isExposed": true})).collect();
        Value::Array(list)
    }

    /// ipcRenderer.invoke(channel, ...args)
    pub fn invoke(&self, channel: &str, args: Vec<Value>) -> Reply {
        if channel == self.bridges_key {
            return Reply::Json(self.bridges_value());
        }
        if channel == "__pw.seq" {
            return Reply::Json(serde_json::json!(self.current_seq()));
        }
        if channel == "__pw.theme" {
            return Reply::Json(self.get("settings", "theme").unwrap_or(serde_json::json!("dark")));
        }
        if let Some(cmd) = channel.strip_prefix("__pw.window.") {
            self.window.send(cmd);
            return Reply::Undefined;
        }
        if let Some((b, v)) = self.var_fetch.get(channel) {
            return match self.get(b, v) {
                Some(v) => Reply::Json(v),
                None => Reply::Undefined,
            };
        }
        if let Some((b, f)) = self.fn_channels.get(channel) {
            let h = self.handlers.lock().get(&format!("{b}.{f}")).cloned();
            let t0 = std::time::Instant::now();
            let _slow = SlowLog(format!("{b}.{f}"), t0);
            return match h {
                Some(h) => match h(self, args) {
                    Ok(v) => Reply::Json(json!({ "data": v })),
                    Err(e) => {
                        crate::log(&format!("{b}.{f} failed: {e:#}"));
                        Reply::Json(json!({ "error": { "name": "Error", "message": format!("{e:#}") } }))
                    }
                },
                None => {
                    crate::log(&format!("not implemented yet: {b}.{f}"));
                    Reply::Json(json!({ "data": null }))
                }
            };
        }
        crate::log(&format!("unknown channel {channel}"));
        Reply::Undefined
    }
}

/// Logs bridge calls that take longer than 3 s (helps find anything that blocks the interface).
struct SlowLog(String, std::time::Instant);
impl Drop for SlowLog {
    fn drop(&mut self) {
        let d = self.1.elapsed();
        if d.as_secs_f64() > 3.0 {
            crate::log(&format!("slow call {} took {:.1}s", self.0, d.as_secs_f64()));
        }
    }
}
