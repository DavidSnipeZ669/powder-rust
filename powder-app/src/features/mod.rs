//! Ports of Powder's main-process features (src/main/features/* in the original).
pub mod analyzer;
pub mod autoedits;
pub mod automontage;
pub mod bookmark;
pub mod community;
pub mod gameedit;
pub mod core;
pub mod cues;
pub mod customgame;
pub mod download;
pub mod editor;
pub mod library;
pub mod montage;
pub mod music;
pub mod projects;
pub mod recorder;
pub mod games;
pub mod settings;
pub mod sounds;
pub mod stream;
pub mod vertical;

use crate::host::Host;
use crate::server::Ctx;
use std::path::PathBuf;
use std::sync::Arc;

/// %APPDATA%\powder-desktop (Electron's userData for Powder)
pub fn user_data() -> PathBuf {
    let d = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("powder-desktop");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// %LOCALAPPDATA%\Powder (APP_DATA_PATH in the original)
pub fn app_data() -> PathBuf {
    let d = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("Powder");
    let _ = std::fs::create_dir_all(&d);
    d
}

static CTX: std::sync::OnceLock<Arc<Ctx>> = std::sync::OnceLock::new();

/// The app context (lives for the whole program).
pub fn ctx() -> &'static Arc<Ctx> {
    CTX.get().expect("ctx")
}

pub fn init(ctx: &Arc<Ctx>) {
    let _ = CTX.set(ctx.clone());
    let h: &Host = &ctx.host;
    core::init(h, ctx);
    settings::init(h, ctx);
    games::init(h, ctx);
    projects::init(h, ctx);
    library::init(h, ctx);
    analyzer::init(h, ctx);
    music::init(h, ctx);
    vertical::init(h, ctx);
    montage::init(h, ctx);
    bookmark::init(h, ctx);
    stream::init(h, ctx);
    automontage::init(h, ctx);
    autoedits::init(h, ctx);
    editor::init(h, ctx);
    download::init(h, ctx);
    recorder::init(h, ctx);
    cues::init(h);
    projects::load_all();
    community::start_sync();
}

/// ISO date from epoch milliseconds
pub fn automontage_iso(ms: u128) -> String {
    automontage::iso_pub(ms)
}
