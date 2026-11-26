//! System info used by settings (screens, audio inputs, AMD GPU/Adrenalin) and run-on-startup.
use parking_lot::RwLock;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Screen {
    pub index: i64,
    pub name: String,
    #[allow(dead_code)] // kept for upcoming features
    pub width: u32,
    pub height: u32,
}

fn screens_cell() -> &'static RwLock<Vec<Screen>> {
    static S: OnceLock<RwLock<Vec<Screen>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Vec::new()))
}

/// Filled from the window system at start-up (tao monitors on Windows).
pub fn set_screens(v: Vec<Screen>) {
    *screens_cell().write() = v;
}

pub fn screens() -> Vec<Screen> {
    let s = screens_cell().read().clone();
    if s.is_empty() {
        vec![Screen { index: 0, name: "Screen 1".into(), width: 1920, height: 1080 }]
    } else {
        s
    }
}

/// Audio capture devices as (label, id). Filled by the native device enumerator when it runs.
pub fn audio_inputs() -> Vec<(String, String)> {
    audio_cell().read().clone()
}
fn audio_cell() -> &'static RwLock<Vec<(String, String)>> {
    static S: OnceLock<RwLock<Vec<(String, String)>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Vec::new()))
}
pub fn set_audio_inputs(v: Vec<(String, String)>) {
    *audio_cell().write() = v;
}

/// (has Radeon GPU, has AFMF-capable GPU: RX 6000 series or newer)
pub fn radeon() -> (bool, bool) {
    if let Some(v) = enumerate() {
        let gpus = v["gpus"].as_array().cloned().unwrap_or_default();
        let names: Vec<String> = gpus.iter().filter(|g| g["vendor"] == "Amd").map(|g| g["name"].as_str().unwrap_or("").to_lowercase()).collect();
        let afmf = names.iter().any(|n| n.contains("rx 6") || n.contains("rx 7") || n.contains("rx 9"));
        return (!names.is_empty(), afmf);
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_VideoController).Name"])
            .creation_flags_hidden()
            .output();
        if let Ok(o) = out {
            let names = String::from_utf8_lossy(&o.stdout).to_lowercase();
            let radeon = names.contains("radeon");
            let afmf = names.lines().any(|l| l.contains("radeon") && (l.contains("rx 6") || l.contains("rx 7") || l.contains("rx 9")));
            return (radeon, afmf);
        }
    }
    (false, false)
}

pub fn adrenalin_path() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(r"C:\Program Files\AMD\CNext\CNext\RadeonSoftware.exe");
    p.exists().then_some(p)
}

#[allow(unused_variables)]
pub fn set_run_on_startup(on: bool) {
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
        let mut c = std::process::Command::new("reg");
        if on {
            c.args(["add", key, "/v", "Powder", "/t", "REG_SZ", "/d", &format!("\"{exe}\" --hidden"), "/f"]);
        } else {
            c.args(["delete", key, "/v", "Powder", "/f"]);
        }
        let _ = c.creation_flags_hidden().output();
    }
}

/// Hide console windows of helper processes on Windows.
pub trait HiddenExt {
    fn creation_flags_hidden(&mut self) -> &mut Self;
}
impl HiddenExt for std::process::Command {
    fn creation_flags_hidden(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        self
    }
}

/// shell.trashItem: move to the Recycle Bin on Windows.
pub fn trash(p: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let ps = format!(
            "Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile('{}','OnlyErrorDialogs','SendToRecycleBin')",
            p.display().to_string().replace('\'', "''")
        );
        let st = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps]).creation_flags_hidden().status()?;
        anyhow::ensure!(st.success(), "could not move {} to the Recycle Bin", p.display());
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(p)?;
        Ok(())
    }
}

/// block-sleep feature: keep the PC awake while any key holds a lock (analysis, export, download ...).
pub fn block_sleep(key: &str, on: bool) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut k = keys.lock().unwrap();
    if on {
        k.insert(key.to_string());
    } else {
        k.remove(key);
    }
    let any = !k.is_empty();
    drop(k);
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::*;
        // a dedicated thread owns the request (SetThreadExecutionState is per thread)
        static TX: OnceLock<Mutex<std::sync::mpsc::Sender<bool>>> = OnceLock::new();
        let tx = TX.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            std::thread::spawn(move || {
                for on in rx {
                    unsafe {
                        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
                    }
                }
            });
            Mutex::new(tx)
        });
        let _ = tx.lock().unwrap().send(any);
    }
    #[cfg(not(windows))]
    let _ = any;
}

/// powder-native DeviceEnumeratorProcess: screens, audio devices, GPUs, NPU (cached for 1 s like the original).
pub fn enumerate() -> Option<serde_json::Value> {
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<Option<(std::time::Instant, serde_json::Value)>>> = OnceLock::new();
    let c = CACHE.get_or_init(|| Mutex::new(None));
    if let Some((t, v)) = c.lock().unwrap().as_ref() {
        if t.elapsed().as_secs_f64() < 1.0 {
            return Some(v.clone());
        }
    }
    let acc = crate::native::Accel { nvidia: false, intel: false, amd: false };
    let p = crate::native::NativeProcess::start("device_enumerator", vec![], acc, Box::new(|_, _| {})).ok()?;
    let r = p.push("start", None).recv_timeout(std::time::Duration::from_secs(15)).ok().and_then(|r| r.ok());
    p.kill();
    let v = r?;
    set_screens(v["outputs"].as_array().cloned().unwrap_or_default().iter().map(|o| Screen {
        index: o["index"].as_i64().unwrap_or(0), name: o["name"].as_str().unwrap_or("").to_string(),
        width: o["width"].as_u64().unwrap_or(0) as u32, height: o["height"].as_u64().unwrap_or(0) as u32,
    }).collect());
    set_audio_inputs(v["audioCaptureDevices"].as_array().cloned().unwrap_or_default().iter().map(|d| (d["name"].as_str().unwrap_or("").to_string(), d["id"].as_str().unwrap_or("").to_string())).collect());
    *c.lock().unwrap() = Some((std::time::Instant::now(), v.clone()));
    Some(v)
}
