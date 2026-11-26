//! Window commands from the bridge threads to the UI thread (minimize, maximize, drag, ...).
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone)]
pub struct WindowCtl {
    tx: Arc<parking_lot::Mutex<Option<Box<dyn Fn(String) + Send>>>>,
    fallback: Sender<String>,
}

impl WindowCtl {
    pub fn new() -> (WindowCtl, Receiver<String>) {
        let (tx, rx) = channel();
        (WindowCtl { tx: Arc::new(parking_lot::Mutex::new(None)), fallback: tx }, rx)
    }
    /// Install the real sender once the event loop exists (an EventLoopProxy on Windows).
    pub fn connect(&self, f: Box<dyn Fn(String) + Send>) {
        *self.tx.lock() = Some(f);
    }
    pub fn send(&self, cmd: &str) {
        match &*self.tx.lock() {
            Some(f) => f(cmd.to_string()),
            None => {
                let _ = self.fallback.send(cmd.to_string());
            }
        }
    }
}
