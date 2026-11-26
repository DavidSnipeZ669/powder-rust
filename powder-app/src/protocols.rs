//! Powder's custom file protocols (powder-local-video://, powder-music://, powder-game-assets:// ...).
//! The original registered them with Electron; here they're served as
//! /__pw/proto/<scheme>/?t=<token>&p=<what came after scheme://>, with HTTP Range support for video.

use crate::server::{percent_decode, Ctx};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use crate::http::Request;

type Resolver = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

fn table() -> &'static RwLock<HashMap<String, Resolver>> {
    static T: OnceLock<RwLock<HashMap<String, Resolver>>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(HashMap::new()))
}

/// registerFileProtocol(scheme, handler): handler gets what follows `scheme://` and returns a file.
pub fn register<F: Fn(&str) -> Option<PathBuf> + Send + Sync + 'static>(scheme: &str, f: F) {
    table().write().insert(scheme.to_string(), Arc::new(f));
}

/// protocolize(path) - the URL the interface should use for `scheme://rest`.
pub fn url(token: &str, scheme: &str, rest: &str) -> String {
    format!("/__pw/proto/{scheme}/?t={token}&p={}", encode(rest))
}

pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

pub fn serve(ctx: &Ctx, req: Request, rest: &str) {
    let _ = ctx;
    let (scheme, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let q = req.url.split_once('?').map(|x| x.1).unwrap_or("");
    let p = q.split('&').find_map(|kv| kv.strip_prefix("p=")).map(percent_decode).unwrap_or_else(|| tail.to_string());
    let resolver = table().read().get(scheme).cloned();
    let file = match resolver {
        Some(r) => r(&p),
        None => Some(PathBuf::from(&p)),
    };
    match file.filter(|f| f.is_file()) {
        Some(f) => req.file(&f),
        None => req.status(404),
    }
}

