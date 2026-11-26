//! ONNX Runtime environment (loaded dynamically so one binary works with any onnxruntime build:
//! the DirectML one we ship for AMD/NVIDIA/Intel GPUs on Windows, or a plain CPU build).

use anyhow::{anyhow, Result};
use ort::environment::Environment;
use ort::session::{builder::GraphOptimizationLevel, Session};
use std::path::Path;
use std::sync::OnceLock;

static ENV: OnceLock<Environment> = OnceLock::new();
static USE_GPU: OnceLock<bool> = OnceLock::new();
static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Intra-op threads per session (0 = auto).
pub fn set_threads(n: usize) {
    THREADS.store(n, std::sync::atomic::Ordering::Relaxed);
}

pub fn gpu() -> bool {
    *USE_GPU.get().unwrap_or(&false) && cfg!(windows)
}

pub fn init(dylib: &Path, gpu: bool) -> Result<()> {
    if ENV.get().is_some() {
        return Ok(());
    }
    let env = ort::init_from(dylib)
        .map_err(|e| anyhow!("failed to load onnxruntime from {}: {e}", dylib.display()))?
        .with_name("powder-rs")
        .build()
        .map_err(|e| anyhow!("onnxruntime init: {e}"))?;
    let _ = ENV.set(env);
    let _ = USE_GPU.set(gpu);
    Ok(())
}

pub fn session(model: &Path) -> Result<Session> {
    let env = ENV.get().ok_or_else(|| anyhow!("onnxruntime not initialised"))?;
    let mut b = Session::builder(env)
        .map_err(|e| anyhow!("{e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| anyhow!("{e}"))?;
    #[cfg(windows)]
    if *USE_GPU.get().unwrap_or(&false) {
        b = b
            .with_execution_providers([ort::ep::DirectML::default().build()])
            .map_err(|e| anyhow!("{e}"))?;
    }
    let mut threads = THREADS.load(std::sync::atomic::Ordering::Relaxed);
    if threads == 0 {
        threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    }
    b = b.with_intra_threads(threads).map_err(|e| anyhow!("{e}"))?;
    b.commit_from_file(model).map_err(|e| anyhow!("loading model {}: {e}", model.display()))
}
