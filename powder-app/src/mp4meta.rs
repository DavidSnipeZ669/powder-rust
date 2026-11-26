//! powder-metadata-block: Powder tags its exports (auto-montages, montages, clips) with a JSON block
//! inside the MP4, read/written by the native mp4_metadata_handler. A sidecar copy is kept in
//! Feature Storage so lists still work when the native engine isn't available.
use crate::native::{Accel, NativeProcess};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NO_ACCEL: Accel = Accel { nvidia: false, intel: false, amd: false };

fn sidecar(path: &Path) -> PathBuf {
    use md5::Digest;
    let h = hex::encode(md5::Md5::digest(path.to_string_lossy().as_bytes()));
    crate::features::user_data().join("Feature Storage").join("powder-rs-metadata").join(format!("{h}.json"))
}

fn call(cmd: &str, args: Value) -> Result<Value> {
    let p = NativeProcess::start("mp4_metadata_handler", vec![], NO_ACCEL, Box::new(|_, _| {}))?;
    let r = p.call(cmd, Some(&args));
    let _ = p.push("quit", Some(&Value::Null));
    std::thread::sleep(std::time::Duration::from_millis(50));
    p.kill();
    r
}

pub fn read(path: &Path) -> Option<Value> {
    if let Ok(v) = call("read", json!({"path": path})) {
        if !v.is_null() {
            return Some(v["data"].clone());
        }
    }
    serde_json::from_str(&std::fs::read_to_string(sidecar(path)).ok()?).ok()
}

pub fn write(path: &Path, data: &Value) -> Result<()> {
    let _ = crate::features::core::write_json(&sidecar(path), data);
    call("write", json!({"path": path, "data": data, "version": 1})).map(|_| ())
}
