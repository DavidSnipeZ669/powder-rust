//! Native dialogs and clipboard (Electron's dialog/clipboard). Same return shapes as Powder's main-window feature.
use serde_json::{json, Value};

#[cfg(windows)]
fn dlg() -> rfd::FileDialog {
    rfd::FileDialog::new()
}

fn p(x: Option<std::path::PathBuf>) -> Value {
    x.map(|p| json!(p.to_string_lossy())).unwrap_or(Value::Null)
}

fn many(x: Option<Vec<std::path::PathBuf>>) -> Value {
    json!(x.unwrap_or_default().iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>())
}

#[allow(unused_variables)]
pub fn pick(kind: &str) -> Value {
    #[cfg(windows)]
    {
        return match kind {
            "folder" => p(dlg().pick_folder()),
            "video" => p(dlg().add_filter("video", &["mp4", "mkv", "webm"]).pick_file()),
            "videos" => many(dlg().add_filter("mp4 video", &["mp4"]).pick_files()),
            "music" => p(dlg().add_filter("Audio", &["mp3"]).pick_file()),
            "images" => many(dlg().add_filter("Images", &["jpg", "jpeg", "png"]).pick_files()),
            _ => Value::Null,
        };
    }
    #[cfg(not(windows))]
    {
        let _ = (p(None), many(None));
        if kind == "videos" || kind == "images" { json!([]) } else { Value::Null }
    }
}

#[allow(unused_variables)]
pub fn open_dialog(o: Value) -> Value {
    #[cfg(windows)]
    {
        let mut d = dlg();
        if let Some(dp) = o["defaultPath"].as_str() {
            d = d.set_directory(dp);
        }
        for f in o["filters"].as_array().cloned().unwrap_or_default() {
            let exts: Vec<String> = f["extensions"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
            d = d.add_filter(f["name"].as_str().unwrap_or("files"), &exts);
        }
        let props: Vec<String> = o["properties"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
        let r = if props.iter().any(|x| x == "openDirectory") { d.pick_folder() } else { d.pick_file() };
        return p(r);
    }
    #[cfg(not(windows))]
    Value::Null
}

#[allow(unused_variables)]
pub fn save_dialog(o: Value) -> Value {
    #[cfg(windows)]
    {
        let mut d = dlg();
        if let Some(dp) = o["defaultPath"].as_str() {
            let pb = std::path::PathBuf::from(dp);
            if let Some(n) = pb.file_name() {
                d = d.set_file_name(n.to_string_lossy());
            }
            if let Some(par) = pb.parent() {
                d = d.set_directory(par);
            }
        }
        for f in o["filters"].as_array().cloned().unwrap_or_default() {
            let exts: Vec<String> = f["extensions"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(String::from)).collect()).unwrap_or_default();
            d = d.add_filter(f["name"].as_str().unwrap_or("files"), &exts);
        }
        let Some(mut path) = d.save_file() else { return Value::Null };
        if let Some(ext) = o["extension"].as_str() {
            if !path.to_string_lossy().ends_with(&format!(".{ext}")) {
                path = std::path::PathBuf::from(format!("{}.{ext}", path.display()));
            }
        }
        return json!(path.to_string_lossy());
    }
    #[cfg(not(windows))]
    Value::Null
}

#[allow(unused_variables)]
pub fn clipboard_set(t: &str) {
    #[cfg(windows)]
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(t.to_string());
    }
}

pub fn clipboard_get() -> String {
    #[cfg(windows)]
    if let Ok(mut c) = arboard::Clipboard::new() {
        return c.get_text().unwrap_or_default();
    }
    String::new()
}
