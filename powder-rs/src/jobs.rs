//! Background jobs (analysis, clip export, montage, add-game, downloads) with progress,
//! polled by the UI.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Default)]
pub struct JobState {
    pub id: u64,
    pub kind: String,
    pub title: String,
    /// "running" | "done" | "error"
    pub state: String,
    /// 0..1, or -1 when unknown
    pub progress: f64,
    pub message: String,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<(u64, BTreeMap<u64, JobState>)>>,
}

#[derive(Clone)]
pub struct JobHandle {
    jobs: Jobs,
    pub id: u64,
}

impl JobHandle {
    pub fn progress(&self, p: f64, msg: impl Into<String>) {
        let mut g = self.jobs.inner.lock().unwrap();
        if let Some(j) = g.1.get_mut(&self.id) {
            j.progress = p;
            j.message = msg.into();
        }
    }
}

impl Jobs {
    pub fn spawn<F>(&self, kind: &str, title: &str, f: F) -> u64
    where
        F: FnOnce(JobHandle) -> anyhow::Result<Value> + Send + 'static,
    {
        let id = {
            let mut g = self.inner.lock().unwrap();
            g.0 += 1;
            let id = g.0;
            g.1.insert(id, JobState { id, kind: kind.into(), title: title.into(), state: "running".into(), progress: -1.0, ..Default::default() });
            id
        };
        let h = JobHandle { jobs: self.clone(), id };
        let jobs = self.clone();
        std::thread::spawn(move || {
            let r = f(h);
            let mut g = jobs.inner.lock().unwrap();
            if let Some(j) = g.1.get_mut(&id) {
                match r {
                    Ok(v) => {
                        j.state = "done".into();
                        j.progress = 1.0;
                        j.result = Some(v);
                    }
                    Err(e) => {
                        j.state = "error".into();
                        j.error = Some(format!("{e:#}"));
                    }
                }
            }
        });
        id
    }

    pub fn list(&self) -> Vec<JobState> {
        let g = self.inner.lock().unwrap();
        g.1.values().rev().take(30).cloned().collect()
    }
}
