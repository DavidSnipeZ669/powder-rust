//! Rust port of powder-native/PowderNativeProcessBase.ts: runs Powder's native engine
//! (PowderRecorderCli.exe <command> ...) and talks to it over a named pipe, exactly like the original:
//! commands are UTF-16LE lines "NAME {json}\n", answers are "RESPONSE_OK {json}" or event lines.

use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, OnceLock};

pub type EventFn = Box<dyn Fn(&str, &Value) + Send + Sync>;

#[derive(Clone, Copy)]
pub struct Accel {
    pub nvidia: bool,
    pub intel: bool,
    pub amd: bool,
}

impl Accel {
    pub fn from_settings() -> Accel {
        let s = crate::features::settings::read();
        Accel {
            nvidia: s["allowNvidiaAcceleration"].as_bool().unwrap_or(true),
            intel: s["allowIntelAcceleration"].as_bool().unwrap_or(true),
            amd: s["allowAmdAcceleration"].as_bool().unwrap_or(true),
        }
    }
}

static CLI: OnceLock<PathBuf> = OnceLock::new();

/// Where PowderRecorderCli.exe lives (set at start-up from the Powder folder).
pub fn set_cli(p: PathBuf) {
    let _ = CLI.set(p);
}
pub fn cli() -> Option<&'static PathBuf> {
    CLI.get()
}

struct Shared {
    queue: VecDeque<(String, Sender<Result<Value>>)>,
    writer: Option<Arc<dyn PipeWrite>>,
    closed: bool,
}

trait PipeWrite: Send + Sync {
    fn write_line(&self, s: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct NativeProcess {
    pub command: String,
    shared: Arc<Mutex<Shared>>,
    pid: Arc<Mutex<Option<u32>>>,
}

impl NativeProcess {
    /// Start `PowderRecorderCli.exe ... <command> <extra args>`; `on_event(name, data)` gets every line.
    pub fn start(command: &str, extra: Vec<String>, accel: Accel, on_event: EventFn) -> Result<NativeProcess> {
        let p = NativeProcess {
            command: command.to_string(),
            shared: Arc::new(Mutex::new(Shared { queue: VecDeque::new(), writer: None, closed: false })),
            pid: Arc::new(Mutex::new(None)),
        };
        imp::spawn(&p, extra, accel, Arc::new(on_event))?;
        Ok(p)
    }

    /// pushCommand: queued, one at a time; resolves on RESPONSE_OK.
    pub fn push(&self, name: &str, args: Option<&Value>) -> Receiver<Result<Value>> {
        let (tx, rx) = channel();
        let mut line = name.to_string();
        if let Some(a) = args {
            if !a.is_null() {
                line.push(' ');
                line.push_str(&a.to_string());
            }
        }
        line.push('\n');
        let mut s = self.shared.lock();
        if s.closed {
            let _ = tx.send(Err(anyhow::anyhow!("{} process is closed", self.command)));
            return rx;
        }
        s.queue.push_back((line.clone(), tx));
        if s.queue.len() == 1 {
            if let Some(w) = &s.writer {
                let _ = w.write_line(&line);
            }
        }
        rx
    }

    /// push + wait
    pub fn call(&self, name: &str, args: Option<&Value>) -> Result<Value> {
        self.push(name, args).recv().map_err(|_| anyhow::anyhow!("{} process ended", self.command))?
    }

    pub fn kill(&self) {
        if let Some(pid) = self.pid.lock().take() {
            imp::kill(pid);
        }
    }

    pub fn suspend(&self) -> bool {
        self.pid.lock().map(|p| imp::suspend(p, true)).unwrap_or(false)
    }
    pub fn resume(&self) -> bool {
        self.pid.lock().map(|p| imp::suspend(p, false)).unwrap_or(false)
    }

    /// Called by the reader with each complete line.
    fn on_line(shared: &Arc<Mutex<Shared>>, on_event: &EventFn, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        let (name, data) = match line.find(' ') {
            Some(i) => (&line[..i], serde_json::from_str(&line[i + 1..]).unwrap_or(Value::Null)),
            None => (line, Value::Null),
        };
        if name == "RESPONSE_OK" {
            let mut s = shared.lock();
            if let Some((_, tx)) = s.queue.pop_front() {
                let _ = tx.send(Ok(data.clone()));
            }
            if let (Some((next, _)), Some(w)) = (s.queue.front(), &s.writer) {
                let _ = w.write_line(next);
            }
        } else if name == "LOG" {
            crate::log(&format!("native: {data}"));
        }
        on_event(name, &data);
    }

    fn on_closed(shared: &Arc<Mutex<Shared>>, on_event: &EventFn) {
        let mut s = shared.lock();
        s.closed = true;
        for (_, tx) in s.queue.drain(..) {
            let _ = tx.send(Err(anyhow::anyhow!("native process connection closed")));
        }
        drop(s);
        on_event("connection-closed", &Value::Null);
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::gpu::HiddenExt;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Pipes::*;
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::System::IO::*;

    struct Pipe(HANDLE);
    unsafe impl Send for Pipe {}
    unsafe impl Sync for Pipe {}

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One overlapped operation, waited synchronously (lets read and write run on different threads).
    unsafe fn overlapped<F: FnOnce(*mut OVERLAPPED) -> BOOL>(h: HANDLE, f: F) -> Result<u32> {
        let ev = CreateEventW(null_mut(), 1, 0, null_mut());
        let mut ov: OVERLAPPED = std::mem::zeroed();
        ov.hEvent = ev;
        let ok = f(&mut ov);
        let mut n: u32 = 0;
        let res = if ok == 0 && GetLastError() != ERROR_IO_PENDING {
            Err(anyhow::anyhow!("pipe io error {}", GetLastError()))
        } else if GetOverlappedResult(h, &ov, &mut n, 1) == 0 {
            Err(anyhow::anyhow!("pipe io error {}", GetLastError()))
        } else {
            Ok(n)
        };
        CloseHandle(ev);
        res
    }

    impl PipeWrite for Pipe {
        fn write_line(&self, s: &str) -> Result<()> {
            let bytes: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let mut off = 0;
            while off < bytes.len() {
                let n = unsafe {
                    overlapped(self.0, |ov| WriteFile(self.0, bytes[off..].as_ptr(), (bytes.len() - off) as u32, null_mut(), ov))?
                };
                off += n as usize;
            }
            Ok(())
        }
    }

    pub fn spawn(p: &NativeProcess, extra: Vec<String>, accel: Accel, on_event: Arc<EventFn>) -> Result<()> {
        let Some(exe) = cli() else { bail!("PowderRecorderCli.exe not found in the Powder folder") };
        let name = format!("POWDER_PIPE_{}", &crate::server::random_token()[..32]);
        let full = wide(&format!(r"\\.\pipe\{name}"));
        let h = unsafe {
            CreateNamedPipeW(
                full.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                1 << 16,
                1 << 16,
                0,
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            bail!("CreateNamedPipe failed");
        }
        let pipe = Arc::new(Pipe(h));
        let child = std::process::Command::new(exe)
            .current_dir(exe.parent().unwrap())
            .arg("--commandPipeName")
            .arg(&name)
            .args(["--allowNvidiaAcceleration", &accel.nvidia.to_string()])
            .args(["--allowIntelAcceleration", &accel.intel.to_string()])
            .args(["--allowAmdAcceleration", &accel.amd.to_string()])
            .arg(&p.command)
            .args(&extra)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags_hidden()
            .spawn()?;
        *p.pid.lock() = Some(child.id());
        let shared = p.shared.clone();
        let cmd = p.command.clone();
        std::thread::spawn(move || {
            let mut child = child;
            let mut buf = vec![0u8; 1 << 16];
            // The CLI may reconnect (the recorder does after a hybrid-graphics restart), so keep serving
            // the pipe until the process exits, like Node's net server did.
            loop {
                let _ = unsafe { overlapped(pipe.0, |ov| ConnectNamedPipe(pipe.0, ov)) };
                {
                    let mut s = shared.lock();
                    s.writer = Some(pipe.clone() as Arc<dyn PipeWrite>);
                    if let Some((first, _)) = s.queue.front() {
                        let _ = pipe.write_line(first);
                    }
                }
                let mut pending: Vec<u8> = Vec::new();
                let mut text = String::new();
                loop {
                    let n = match unsafe { overlapped(pipe.0, |ov| ReadFile(pipe.0, buf.as_mut_ptr(), buf.len() as u32, null_mut(), ov)) } {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n as usize,
                    };
                    pending.extend_from_slice(&buf[..n]);
                    let even = pending.len() & !1;
                    let units: Vec<u16> = pending[..even].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    pending.drain(..even);
                    text.push_str(&String::from_utf16_lossy(&units));
                    while let Some(i) = text.find('\n') {
                        let line: String = text.drain(..=i).collect();
                        NativeProcess::on_line(&shared, &on_event, &line);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                on_event("connection-closed", &Value::Null);
                unsafe { DisconnectNamedPipe(pipe.0) };
            }
            crate::log(&format!("{cmd}: pipe closed"));
            NativeProcess::on_closed(&shared, &on_event);
            unsafe { CloseHandle(pipe.0) };
            let _ = child.wait();
        });
        Ok(())
    }

    pub fn kill(pid: u32) {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }

    /// suspendProcess / resumeProcess from powder-native.node: suspend every thread of the process.
    pub fn suspend(pid: u32, suspend: bool) -> bool {
        use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut te: THREADENTRY32 = std::mem::zeroed();
            te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut ok = Thread32First(snap, &mut te) != 0;
            while ok {
                if te.th32OwnerProcessID == pid {
                    let t = OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID);
                    if !t.is_null() {
                        if suspend {
                            SuspendThread(t);
                        } else {
                            ResumeThread(t);
                        }
                        CloseHandle(t);
                    }
                }
                ok = Thread32Next(snap, &mut te) != 0;
            }
            CloseHandle(snap);
            true
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn spawn(_p: &NativeProcess, _extra: Vec<String>, _accel: Accel, _on_event: Arc<EventFn>) -> Result<()> {
        let _ = cli();
        bail!("Powder's native engine (PowderRecorderCli.exe) only runs on Windows")
    }
    pub fn kill(_pid: u32) {}
    pub fn suspend(_pid: u32, _s: bool) -> bool {
        false
    }
    #[allow(dead_code)]
    fn _unused(_: &dyn PipeWrite) {}
}

/// The start/progress/finished pattern every native task uses (AnalyzerProcess, EventsAggregatorProcess,
/// KeywordSearchProcess, MontageMakerProcess ...). Stop = AbortError, like the original.
pub struct Job {
    proc: NativeProcess,
    killed: std::sync::atomic::AtomicBool,
    suspended: std::sync::atomic::AtomicBool,
    rx: Mutex<std::sync::mpsc::Receiver<(String, Value)>>,
    finish: &'static str,
}

#[derive(Debug)]
pub struct Aborted;
impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AbortError")
    }
}
impl std::error::Error for Aborted {}

impl Job {
    /// Start `cmd` and send `start <input>`. `progress` events are passed to `on_progress`.
    pub fn start<P: Fn(&Value) + Send + Sync + 'static>(cmd: &str, extra: Vec<String>, input: Value, progress: &'static str, finish: &'static str, accel: Accel, on_progress: P) -> Result<Arc<Job>> {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let proc = NativeProcess::start(
            cmd,
            extra,
            accel,
            Box::new(move |name, data| {
                if name == progress {
                    on_progress(data);
                } else if name == finish || name.ends_with("ERROR_JSON") || name == "connection-closed" {
                    let _ = tx.lock().send((name.to_string(), data.clone()));
                }
            }),
        )?;
        let _ = proc.push("start", Some(&input));
        Ok(Arc::new(Job { proc, killed: Default::default(), suspended: Default::default(), rx: Mutex::new(rx), finish }))
    }

    /// Wait for the result.
    pub fn wait(&self) -> Result<Value> {
        let r = self.rx.lock().recv();
        let out = match r {
            Ok((n, d)) if n == self.finish => Ok(d),
            Ok((n, d)) => {
                if self.killed.load(std::sync::atomic::Ordering::SeqCst) {
                    Err(anyhow::Error::new(Aborted))
                } else {
                    Err(anyhow::anyhow!("{}: {n} {d}", self.proc.command))
                }
            }
            Err(_) => Err(if self.killed.load(std::sync::atomic::Ordering::SeqCst) { anyhow::Error::new(Aborted) } else { anyhow::anyhow!("connection closed") }),
        };
        self.proc.kill();
        out
    }

    pub fn push(&self, name: &str, args: Option<&Value>) -> Receiver<Result<Value>> {
        self.proc.push(name, args)
    }

    pub fn stop(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.proc.kill();
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn toggle_pause(&self) -> bool {
        let s = self.is_suspended();
        let ok = if s { self.proc.resume() } else { self.proc.suspend() };
        if ok {
            self.suspended.store(!s, std::sync::atomic::Ordering::SeqCst);
        }
        self.is_suspended()
    }
}

pub fn is_abort(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Aborted>().is_some()
}
