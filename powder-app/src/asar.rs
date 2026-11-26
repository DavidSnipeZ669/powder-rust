//! Minimal Electron ASAR reader/writer.
//! Layout: [u32 4][u32 header_pickle_len][u32 payload_len][u32 json_len][json][pad to 4] [file data...]
//! File offsets in the JSON are relative to the end of the header (8 + header_pickle_len).
//! Entries marked "unpacked" live in `<archive>.unpacked/` and are left untouched.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Asar {
    pub header: Value,
    data: Vec<u8>,
    base: usize,
    /// replaced file contents by path ("dist/main/main.js")
    pub overrides: BTreeMap<String, Vec<u8>>,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Asar {
    pub fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if data.len() < 16 || u32le(&data, 0) != 4 {
            bail!("{} is not an asar archive", path.display());
        }
        let header_size = u32le(&data, 4) as usize;
        let json_len = u32le(&data, 12) as usize;
        let json = std::str::from_utf8(&data[16..16 + json_len]).context("asar header is not utf8")?;
        let header: Value = serde_json::from_str(json).context("asar header json")?;
        Ok(Self { header, base: 8 + header_size, data, overrides: BTreeMap::new() })
    }

    fn entry<'a>(&'a self, path: &str) -> Option<&'a Value> {
        let mut cur = &self.header;
        for part in path.split('/') {
            cur = cur.get("files")?.get(part)?;
        }
        Some(cur)
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        if let Some(o) = self.overrides.get(path) {
            return Ok(o.clone());
        }
        let e = self.entry(path).with_context(|| format!("{path} not in asar"))?;
        if e.get("unpacked").and_then(|v| v.as_bool()) == Some(true) {
            bail!("{path} is an unpacked file (read it from app.asar.unpacked)");
        }
        let off: usize = e["offset"].as_str().context("offset")?.parse()?;
        let size = e["size"].as_u64().context("size")? as usize;
        Ok(self.data[self.base + off..self.base + off + size].to_vec())
    }

    #[allow(dead_code)] // kept for upcoming features
    pub fn write(&mut self, path: &str, content: Vec<u8>) {
        self.overrides.insert(path.to_string(), content);
    }

    /// Serialise with all overrides applied (offsets and integrity recomputed).
    #[allow(dead_code)] // kept for upcoming features
    pub fn save(&self, out: &Path) -> Result<()> {
        let mut header = self.header.clone();
        let mut blob: Vec<u8> = Vec::with_capacity(self.data.len());
        fn walk(node: &mut Map<String, Value>, prefix: &str, me: &Asar, blob: &mut Vec<u8>) -> Result<()> {
            for (name, v) in node.iter_mut() {
                let p = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                let obj = v.as_object_mut().context("bad asar node")?;
                if let Some(files) = obj.get_mut("files").and_then(|f| f.as_object_mut()) {
                    walk(files, &p, me, blob)?;
                    continue;
                }
                if obj.contains_key("link") || obj.get("unpacked").and_then(|u| u.as_bool()) == Some(true) {
                    continue;
                }
                let content = me.read(&p)?;
                obj.insert("offset".into(), Value::String(blob.len().to_string()));
                obj.insert("size".into(), Value::from(content.len() as u64));
                if obj.contains_key("integrity") && me.overrides.contains_key(&p) {
                    obj.insert("integrity".into(), integrity(&content));
                }
                blob.extend_from_slice(&content);
            }
            Ok(())
        }
        if let Some(files) = header.get_mut("files").and_then(|f| f.as_object_mut()) {
            walk(files, "", self, &mut blob)?;
        }
        let json = serde_json::to_string(&header)?;
        let jl = json.len();
        let padded = (jl + 3) / 4 * 4;
        let payload = 4 + padded; // string length field + string
        let header_pickle = 4 + payload; // payload size field + payload
        let mut out_bytes = Vec::with_capacity(16 + padded + blob.len());
        out_bytes.extend_from_slice(&4u32.to_le_bytes());
        out_bytes.extend_from_slice(&(header_pickle as u32).to_le_bytes());
        out_bytes.extend_from_slice(&(payload as u32).to_le_bytes());
        out_bytes.extend_from_slice(&(jl as u32).to_le_bytes());
        out_bytes.extend_from_slice(json.as_bytes());
        out_bytes.resize(16 + padded, 0);
        out_bytes.extend_from_slice(&blob);
        let tmp = out.with_extension("asar.tmp");
        std::fs::write(&tmp, &out_bytes)?;
        std::fs::rename(&tmp, out)?;
        Ok(())
    }
}

#[allow(dead_code)] // kept for upcoming features
fn integrity(content: &[u8]) -> Value {
    const BLOCK: usize = 4 * 1024 * 1024;
    let hash = hex::encode(Sha256::digest(content));
    let blocks: Vec<Value> = content.chunks(BLOCK).map(|c| Value::String(hex::encode(Sha256::digest(c)))).collect();
    let blocks = if blocks.is_empty() { vec![Value::String(hex::encode(Sha256::digest(b"")))] } else { blocks };
    serde_json::json!({ "algorithm": "SHA256", "hash": hash, "blockSize": BLOCK, "blocks": blocks })
}
