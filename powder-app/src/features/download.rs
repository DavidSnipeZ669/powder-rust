//! Port of src/main/features/video-download-manager + modules/video-downloader: import a Twitch VOD/clip,
//! YouTube video or Kick VOD by link (metadata from the platforms, download by Powder's native
//! video_downloader).
use super::projects;
use crate::host::Host;
use crate::native::{Accel, Job};
use crate::server::Ctx;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

const B: &str = "video-download-manager";
const TWITCH_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";

fn host() -> &'static Host {
    &super::ctx().host
}

/// lib/parseUrl.ts -> (source, kind, id)
pub fn parse_url(url: &str) -> Option<(&'static str, &'static str, String)> {
    let u = url.trim();
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://"))?;
    let (host, path_q) = rest.split_once('/').unwrap_or((rest, ""));
    let (path, query) = path_q.split_once('?').unwrap_or((path_q, ""));
    let parts: Vec<&str> = std::iter::once("").chain(path.split('/')).collect();
    match host {
        "www.twitch.tv" | "twitch.tv" | "m.twitch.tv" => {
            if parts.len() == 4 && parts[2] == "clip" {
                Some(("twitch", "clip", parts[3].into()))
            } else if parts.len() == 3 && parts[1] == "videos" {
                Some(("twitch", "video", parts[2].into()))
            } else if parts.len() == 4 && parts[2] == "video" {
                Some(("twitch", "video", parts[3].into()))
            } else {
                None
            }
        }
        "www.kick.com" | "kick.com" => (parts.len() == 4 && parts[2] == "videos").then(|| ("kick", "video", parts[3].to_string())),
        "www.youtube.com" | "youtube.com" | "youtu.be" | "www.youtu.be" => {
            if let Some(v) = query.split('&').find_map(|kv| kv.strip_prefix("v=")) {
                return Some(("youtube", "video", v.to_string()));
            }
            (parts.len() == 2 && !parts[1].is_empty()).then(|| ("youtube", "video", parts[1].to_string()))
        }
        _ => None,
    }
}

/// HTTPS agent: Windows' own TLS (SChannel) on Windows, rustls elsewhere.
pub fn agent() -> ureq::Agent {
    #[cfg(windows)]
    {
        if let Ok(c) = native_tls::TlsConnector::new() {
            return ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(c)).build();
        }
    }
    ureq::AgentBuilder::new().build()
}

fn twitch_gql(body: Value) -> Result<Value> {
    let r = agent().post("https://gql.twitch.tv/gql").set("Client-ID", TWITCH_CLIENT_ID).set("Content-Type", "text/plain;charset=UTF-8").send_string(&body.to_string()).map_err(|e| anyhow::anyhow!("No internet connection ({e})"))?;
    Ok(r.into_json()?)
}

fn kick_data(id: &str) -> Result<Value> {
    let r = agent().get(&format!("https://kick.com/api/v1/video/{id}")).set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36").set("Accept", "application/json").call().map_err(|e| anyhow::anyhow!("Failed to fetch video data ({e})"))?;
    Ok(r.into_json()?)
}

fn ytdlp() -> Option<PathBuf> {
    let c = super::ctx();
    [c.dist.root.join("yt-dlp.exe"), c.dist.native.join("yt-dlp.exe")].into_iter().find(|p| p.exists())
}

fn fetch_metadata(url: &str) -> Result<Value> {
    let (source, kind, id) = parse_url(url).context("Invalid url.")?;
    Ok(match (source, kind) {
        ("twitch", "video") => {
            let r = twitch_gql(json!([{"operationName": "VideoMetadata", "variables": {"channelLogin": "", "videoID": id},
                "extensions": {"persistedQuery": {"version": 1, "sha256Hash": "49b5b8f268cdeb259d75b58dcb0c1a748e3b575003448a2333dc5cdafd49adad"}}}]))?;
            let v = &r[0]["data"]["video"];
            json!({"source": "twitch", "kind": "video", "id": v["id"].as_str().unwrap_or(&id), "title": v["title"], "gameId": v["game"]["id"], "gameName": v["game"]["name"],
                   "publishedAt": v["createdAt"], "durationSec": v["lengthSeconds"], "ownerId": v["owner"]["id"], "ownerDisplayName": v["owner"]["displayName"],
                   "posterUrl": v["previewThumbnailURL"].as_str().map(|s| s.replace("90x60", "640x360"))})
        }
        ("twitch", _) => {
            let r = twitch_gql(json!({"query": "query c($slug: ID!) { clip(slug: $slug) { id game { id } title durationSeconds } }", "variables": {"slug": id}}))?;
            let c = &r["data"]["clip"];
            if !c["durationSeconds"].is_number() {
                bail!("Invalid durationSeconds.");
            }
            json!({"source": "twitch", "kind": "clip", "id": c["id"].as_str().unwrap_or(&id), "durationSeconds": c["durationSeconds"], "gameId": c["game"]["id"], "title": c["title"]})
        }
        ("kick", _) => {
            let d = kick_data(&id)?;
            let l = &d["livestream"];
            json!({"source": "kick", "kind": "video", "id": d["uuid"], "title": l["session_title"], "gameId": l["categories"][0]["id"].to_string(), "gameName": l["categories"][0]["name"],
                   "publishedAt": d["created_at"], "durationSec": (l["duration"].as_f64().unwrap_or(0.0) / 1000.0).to_string(), "ownerId": l["channel"]["user_id"].to_string(),
                   "ownerDisplayName": l["channel"]["user"]["username"]})
        }
        _ => {
            // YouTube: yt-dlp (shipped with Powder) gives the same fields ytdl-core did
            let yt = ytdlp().context("yt-dlp.exe not found in the Powder folder")?;
            let mut c = std::process::Command::new(yt);
            c.args(["-J", "--no-warnings", "--skip-download", url]);
            let out = crate::gpu::HiddenExt::creation_flags_hidden(&mut c).output()?;
            let v: Value = serde_json::from_slice(&out.stdout).context("Url not valid")?;
            let date = v["upload_date"].as_str().map(|d| format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]));
            json!({"source": "youtube", "kind": "video", "id": v["id"], "title": v["title"], "gameId": Value::Null, "gameName": v["categories"].get(0).cloned().unwrap_or(Value::Null),
                   "publishedAt": date, "durationSec": v["duration"].as_f64().map(|d| d.to_string()), "ownerId": v["channel_id"], "ownerDisplayName": v["channel"], "posterUrl": v["thumbnail"]})
        }
    })
}

fn find_game_by(field: &str, value: &str) -> Option<String> {
    let norm = |s: &str| s.to_lowercase().replace(' ', "");
    super::games::db()
        .games
        .iter()
        .find(|g| if field == "name" { g["name"].as_str().map(norm) == Some(norm(value)) } else { g[field] == value })
        .and_then(|g| g["powderGameId"].as_str().map(String::from))
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static J: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_task(id: &str, t: Option<Value>) {
    let mut all = host().get(B, "downloadTasksByExtractionProjectId").unwrap_or(json!({}));
    match t {
        Some(t) => all[id] = t,
        None => {
            if let Some(o) = all.as_object_mut() {
                o.remove(id);
            }
        }
    }
    host().set(B, "downloadTasksByExtractionProjectId", all);
}
fn patch(id: &str, p: Value) {
    let all = host().get(B, "downloadTasksByExtractionProjectId").unwrap_or(json!({}));
    if let Some(cur) = all.get(id).cloned() {
        set_task(id, Some(projects::merge(cur, p)));
    }
}

fn download(id: &str) -> Result<Value> {
    let p = projects::get().get_by_id(id).context("Recording url not found")?;
    let url = p["remoteUrl"].as_str().context("Recording url not found")?.to_string();
    let source = p["source"].as_str().unwrap_or("");
    if source.is_empty() || source == "recorder" || source == "local-file-import" {
        bail!("Invalid recording source");
    }
    let folder = PathBuf::from(super::settings::read()["downloadFolder"].as_str().context("Invalid download folder")?);
    if !folder.exists() {
        std::fs::create_dir_all(&folder).map_err(|_| anyhow::anyhow!("Download folder does not exist, update the path on settings"))?;
    }
    let tmp = folder.join("downloading");
    std::fs::create_dir_all(&tmp)?;
    let uid = super::montage::uuid();
    let out = tmp.join(format!("{uid}.downloading"));
    let (plat, _, vid) = parse_url(&url).context("Invalid url.")?;
    let mut input = json!({"videoPlatform": plat, "videoId": vid, "outputPath": out});
    if plat == "kick" {
        input["kickData"] = kick_data(&vid)?;
    }
    set_task(id, Some(json!({"progress": 0, "etaSeconds": 0, "status": "DOWNLOADING", "paused": false})));
    crate::gpu::block_sleep(id, true);
    let pid = id.to_string();
    let last = Arc::new(Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(2)));
    let r = (|| {
        let job = Job::start("video_downloader", vec![], input, "progress", "finished", Accel::from_settings(), move |pr| {
            let mut l = last.lock();
            if l.elapsed().as_millis() >= 1000 {
                *l = std::time::Instant::now();
                patch(&pid, json!({"progress": pr["progress"].as_f64().unwrap_or(0.0) / 100.0, "etaSeconds": pr["etaSeconds"]}));
            }
        })?;
        jobs().lock().insert(id.to_string(), job.clone());
        let r = job.wait();
        jobs().lock().remove(id);
        r
    })();
    crate::gpu::block_sleep(id, false);
    match r {
        Ok(fin) => {
            let rid = p["remoteRecordingId"].as_str().unwrap_or(id).to_string();
            let final_path = folder.join(format!("{source}-{rid}{}", fin["fileExtension"].as_str().unwrap_or(".mp4")));
            std::fs::rename(fin["outputPath"].as_str().unwrap_or(""), &final_path)?;
            patch(id, json!({"progress": 1, "etaSeconds": 0.5, "status": "DONE"}));
            let vm = crate::media::metadata(&final_path).context("Video metadata not found")?;
            let pid2 = id.to_string();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(30));
                set_task(&pid2, None);
            });
            Ok(json!({"videoPath": final_path, "videoMetadata": vm.to_json()}))
        }
        Err(e) => {
            if let Ok(rd) = std::fs::read_dir(&tmp) {
                for f in rd.flatten().filter(|f| f.file_name().to_string_lossy().contains(&uid)) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
            if crate::native::is_abort(&e) {
                bail!("AbortError");
            }
            patch(id, json!({"progress": 0, "etaSeconds": -1, "status": "FAILED", "error": {"name": "Error", "message": format!("{e:#}")}}));
            Err(e)
        }
    }
}

pub fn init(h: &Host, _ctx: &Arc<Ctx>) {
    let dl = PathBuf::from(super::settings::read()["downloadFolder"].as_str().unwrap_or(""));
    let _ = std::fs::remove_dir_all(dl.join("downloading"));
    h.set(B, "downloadTasksByExtractionProjectId", json!({}));
    h.handle(B, "fetchRemoteVideoMetadata", |_, a| {
        let url = a.first().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let m = fetch_metadata(&url)?;
        let mut f = json!({"url": url, "source": m["source"], "kind": m["kind"], "id": m["id"], "title": m["title"], "gameId": m["gameId"], "publishedAt": m["publishedAt"],
                           "durationSec": m["durationSec"], "ownerId": m["ownerId"], "ownerDisplayName": m["ownerDisplayName"], "posterUrl": m["posterUrl"]});
        let mut g = m["gameId"].as_str().and_then(|t| find_game_by("twitchId", t));
        if g.is_none() {
            g = m["gameName"].as_str().and_then(|n| find_game_by("name", n));
        }
        if let Some(g) = g {
            f["powderGameId"] = json!(g);
        }
        Ok(f)
    });
    h.handle(B, "createDownloadTaskByExtractionProjectId", |_, a| download(a.first().and_then(|v| v.as_str()).unwrap_or("")));
    h.handle(B, "cancelDownloadTaskByExtractionProjectId", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        if let Some(j) = jobs().lock().get(id) {
            j.stop();
        }
        set_task(id, None);
        Ok(Value::Null)
    });
    h.handle(B, "tryTogglePauseResumeDownloadByExtractionProjectId", |_, a| {
        let id = a.first().and_then(|v| v.as_str()).unwrap_or("");
        let j = jobs().lock().get(id).cloned();
        if let Some(j) = j {
            let p = j.toggle_pause();
            patch(id, json!({"paused": p}));
        }
        Ok(Value::Null)
    });
    let _ = Path::new("");
}
