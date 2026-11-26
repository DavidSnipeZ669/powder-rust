//! Global shortcuts (Electron globalShortcut): Powder's TOGGLE_RECORD_SESSION (Control+F8) and
//! MANUAL_BOOKMARK (Control+F9), registered with RegisterHotKey on a dedicated thread.
use parking_lot::Mutex;
use std::sync::OnceLock;

type Handler = Box<dyn Fn() + Send + Sync>;

struct State {
    handlers: Vec<(String, Handler)>, // shortcut type -> handler
    active: bool,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State { handlers: vec![], active: true }))
}

pub fn on<F: Fn() + Send + Sync + 'static>(ty: &str, f: F) {
    state().lock().handlers.push((ty.to_string(), Box::new(f)));
}

pub fn set_active(a: bool) {
    state().lock().active = a;
}

fn fire(ty: &str) {
    let s = state().lock();
    if !s.active {
        return;
    }
    for (t, h) in &s.handlers {
        if t == ty {
            h();
        }
    }
}

/// "Control+Shift+F8" -> (modifiers, virtual key)
pub fn parse(acc: &str) -> Option<(u32, u32)> {
    let mut mods = 0u32;
    let mut vk = None;
    for part in acc.split('+') {
        let p = part.trim().to_lowercase();
        match p.as_str() {
            "control" | "ctrl" | "commandorcontrol" | "cmdorctrl" => mods |= 0x2,
            "alt" | "option" => mods |= 0x1,
            "shift" => mods |= 0x4,
            "super" | "meta" | "cmd" | "command" => mods |= 0x8,
            k => {
                vk = Some(match k {
                    _ if k.len() == 1 && k.as_bytes()[0].is_ascii_alphanumeric() => k.to_ascii_uppercase().as_bytes()[0] as u32,
                    _ if k.starts_with('f') && k[1..].parse::<u32>().map(|n| (1..=24).contains(&n)).unwrap_or(false) => 0x6F + k[1..].parse::<u32>().unwrap(),
                    "space" => 0x20,
                    "insert" => 0x2D,
                    "delete" => 0x2E,
                    "home" => 0x24,
                    "end" => 0x23,
                    "pageup" => 0x21,
                    "pagedown" => 0x22,
                    "up" => 0x26,
                    "down" => 0x28,
                    "left" => 0x25,
                    "right" => 0x27,
                    "printscreen" => 0x2C,
                    "scrolllock" => 0x91,
                    "pause" => 0x13,
                    "tab" => 0x09,
                    "enter" | "return" => 0x0D,
                    "escape" | "esc" => 0x1B,
                    "plus" => 0xBB,
                    "-" | "minus" => 0xBD,
                    _ if k.starts_with("num") && k[3..].parse::<u32>().is_ok() => 0x60 + k[3..].parse::<u32>().unwrap(),
                    _ => return None,
                })
            }
        }
    }
    Some((mods | 0x4000, vk?)) // MOD_NOREPEAT
}

/// (Re)register the shortcuts from settings. Returns which types registered.
pub fn reload() -> Vec<(String, bool)> {
    let s = crate::features::settings::read();
    let list: Vec<(String, String)> = s["shortcuts"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v["shortcut"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    imp::register(list)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc::{channel, Sender};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, PostThreadMessageW, MSG, WM_HOTKEY, WM_USER};

    struct Worker {
        tid: u32,
        tx: Sender<(Vec<(String, String)>, Sender<Vec<(String, bool)>>)>,
    }

    fn worker() -> &'static Worker {
        static W: OnceLock<Worker> = OnceLock::new();
        W.get_or_init(|| {
            let (tx, rx) = channel::<(Vec<(String, String)>, Sender<Vec<(String, bool)>>)>();
            let (tid_tx, tid_rx) = channel();
            std::thread::spawn(move || unsafe {
                let _ = tid_tx.send(GetCurrentThreadId());
                let mut ids: Vec<(i32, String)> = vec![];
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_HOTKEY {
                        if let Some((_, ty)) = ids.iter().find(|(i, _)| *i == msg.wParam as i32) {
                            let ty = ty.clone();
                            std::thread::spawn(move || fire(&ty));
                        }
                    } else if msg.message == WM_USER + 1 {
                        while let Ok((list, reply)) = rx.try_recv() {
                            for (i, _) in ids.drain(..) {
                                UnregisterHotKey(std::ptr::null_mut(), i);
                            }
                            let mut res = vec![];
                            for (n, (ty, acc)) in list.into_iter().enumerate() {
                                let id = 100 + n as i32;
                                let ok = parse(&acc).map(|(m, vk)| RegisterHotKey(std::ptr::null_mut(), id, m, vk) != 0).unwrap_or(false);
                                if ok {
                                    ids.push((id, ty.clone()));
                                }
                                res.push((ty, ok));
                            }
                            let _ = reply.send(res);
                        }
                    }
                }
            });
            Worker { tid: tid_rx.recv().unwrap_or(0), tx }
        })
    }

    pub fn register(list: Vec<(String, String)>) -> Vec<(String, bool)> {
        let w = worker();
        let (rtx, rrx) = channel();
        let _ = w.tx.send((list, rtx));
        unsafe {
            PostThreadMessageW(w.tid, WM_USER + 1, 0, 0);
        }
        rrx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or_default()
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn register(list: Vec<(String, String)>) -> Vec<(String, bool)> {
        let _ = super::fire;
        list.into_iter().map(|(t, _)| (t, false)).collect()
    }
}
