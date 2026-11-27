#![cfg_attr(not(windows), allow(dead_code))] // non-Windows builds are only for testing
//! Powder (Rust). Opens Powder's original interface in a native window (WebView2 on Windows) and runs
//! Powder's main process - every bridge the interface uses - in Rust. Powder's native engine
//! (PowderRecorderCli.exe) is driven from Rust for recording, analysis and video processing.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod asar;
mod dialogs;
mod dist;
mod features;
mod gpu;
mod host;
mod http;
mod hotkeys;
mod jslit;
mod media;
mod mp4meta;
mod native;
mod protocols;
mod server;
mod setup;
mod window;

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

const MANIFEST: &str = include_str!("../assets/bridges.json");

pub fn log_path() -> PathBuf {
    features::app_data().join("powder-rs.log")
}

pub fn log(msg: &str) {
    static F: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
    let f = F.get_or_init(|| Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(log_path()).ok()));
    let line = format!("{} {msg}\n", features::core::now_iso());
    if let Some(f) = f.lock().unwrap().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
    if std::env::var("POWDER_RS_DEBUG").is_ok() {
        eprint!("{line}");
    }
}

fn fail(msg: &str) -> ! {
    log(msg);
    #[cfg(windows)]
    {
        rfd::MessageDialog::new().set_title("Rewind").set_description(msg).set_level(rfd::MessageLevel::Error).show();
    }
    eprintln!("{msg}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    if let Some(i) = args.iter().position(|a| a == "--make-full") {
        let (Some(src), Some(out)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: Rewind.exe --make-full <Powder folder> <out.exe>");
            std::process::exit(2)
        };
        match setup::make_full(std::path::Path::new(src), std::path::Path::new(out)) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                log(&format!("make-full: {e:#}"));
                eprintln!("{e:#}");
                std::process::exit(1)
            }
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--community-index") {
        let dir = args.get(i + 1).map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("games"));
        match features::community::write_index(&dir) {
            Ok(n) => {
                println!("wrote {} with {n} games", dir.join("index.json").display());
                std::process::exit(0)
            }
            Err(e) => {
                log(&format!("community-index: {e:#}"));
                eprintln!("{e:#}");
                std::process::exit(1)
            }
        }
    }
    if args.iter().any(|a| a == "--uninstall") {
        setup::uninstall();
        return;
    }
    // A shared Rewind.exe sets itself up on first run (Powder's files, engine, shortcuts).
    // --powder-dir / POWDER_DIR (development) skip that and use the given folder directly.
    let dev = arg("--powder-dir").is_some() || std::env::var("POWDER_DIR").is_ok() || args.iter().any(|a| a == "--no-setup");
    if !dev && (setup::needed() || args.iter().any(|a| a == "--setup")) {
        run_setup(arg("--port").and_then(|p| p.parse().ok()).unwrap_or(0));
        return;
    }
    if !dev {
        setup::refresh_engine_pack();
    }
    let powder_dir = arg("--powder-dir").map(std::path::PathBuf::from).or_else(|| (!dev).then(setup::base_dir));
    let dist = match dist::Dist::locate(powder_dir.as_deref()) {
        Ok(d) => d,
        Err(e) => fail(&format!("{e:#}")),
    };
    log(&format!("Rewind starting with {}", dist.label));
    // the original looks for PowderRecorderCli.exe next to the app first, then in the unpacked dist folder
    for c in [dist.root.join("PowderRecorderCli.exe"), dist.native.join("PowderRecorderCli.exe")] {
        if c.exists() {
            native::set_cli(c);
            break;
        }
    }
    media::set_ffmpeg([dist.root.join("ffmpeg.exe"), dist.native.join("ffmpeg.exe")].into_iter().find(|p| p.exists()));
    let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    let (wctl, wrx) = window::WindowCtl::new();
    let host = host::Host::new(&manifest, wctl.clone());
    let ctx = Arc::new(server::Ctx { host, dist: Arc::new(dist), token: server::random_token() });
    features::init(&ctx);
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(0);
    let port = match server::start(ctx.clone(), port) {
        Ok(p) => p,
        Err(e) => fail(&format!("could not start: {e:#}")),
    };
    let url = format!("http://127.0.0.1:{port}/");
    log(&format!("interface at {url}"));

    #[cfg(windows)]
    {
        let _ = wrx;
        run_webview_window(url, wctl, args.iter().any(|a| a == "--hidden"), (1440.0, 900.0), (1024.0, 660.0), Some(ctx));
    }
    #[cfg(not(windows))]
    {
        // Development on Linux/macOS: no native window, open the URL in any Chromium to test.
        let _ = ctx;
        println!("{url}");
        for cmd in wrx {
            log(&format!("window command: {cmd}"));
            if cmd == "quit" || cmd == "close" {
                std::process::exit(0);
            }
        }
    }
}

/// The first-run setup window (setup.rs does the work; this just hosts its page).
fn run_setup(port: u16) {
    let (wctl, wrx) = window::WindowCtl::new();
    let w2 = wctl.clone();
    let quit = move |launch: Option<std::path::PathBuf>| {
        if let Some(exe) = launch {
            let _ = std::process::Command::new(&exe).arg("--no-setup-check").spawn();
        }
        w2.send("quit");
    };
    let port = match http::serve(port, move |req| setup::handle(req, &quit)) {
        Ok(p) => p,
        Err(e) => fail(&format!("could not start setup: {e:#}")),
    };
    let url = format!("http://127.0.0.1:{port}/");
    log(&format!("setup at {url}"));
    setup::set_window(wctl.clone());
    #[cfg(windows)]
    {
        let _ = wrx;
        run_webview_window(url, wctl, false, (880.0, 680.0), (720.0, 560.0), None);
    }
    #[cfg(not(windows))]
    {
        let _ = wctl;
        println!("{url}");
        for cmd in wrx {
            if cmd == "quit" || cmd == "close" {
                return;
            }
        }
    }
}

#[cfg(windows)]
fn make_tray(proxy: tao::event_loop::EventLoopProxy<String>) -> Option<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let img = image::load_from_memory(include_bytes!("../assets/branding/icon-256.png")).ok()?.resize(32, 32, image::imageops::FilterType::Lanczos3).to_rgba8();
    let (w, h) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), w, h).ok()?;
    let menu = Menu::new();
    let open = MenuItem::with_id("tray-open", "Open Rewind", true, None);
    let rec = MenuItem::with_id("tray-record", "Start / stop session", true, None);
    let quit = MenuItem::with_id("tray-quit", "Quit Rewind", true, None);
    let _ = menu.append_items(&[&open, &PredefinedMenuItem::separator(), &rec, &PredefinedMenuItem::separator(), &quit]);
    let p1 = parking_lot::Mutex::new(proxy.clone());
    MenuEvent::set_event_handler(Some(std::sync::Arc::new(move |e: MenuEvent| {
        let _ = p1.lock().send_event(e.id.0.clone());
    })));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = proxy.send_event("tray-open".to_string());
        }
    }));
    match TrayIconBuilder::new().with_icon(icon).with_tooltip("Rewind").with_menu(Box::new(menu)).with_menu_on_left_click(false).build() {
        Ok(t) => Some(t),
        Err(e) => {
            log(&format!("tray icon: {e}"));
            None
        }
    }
}

#[cfg(windows)]
fn load_icon() -> Option<tao::window::Icon> {
    let img = image::load_from_memory(include_bytes!("../assets/branding/icon-256.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    tao::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

#[cfg(windows)]
fn run_webview_window(url: String, wctl: window::WindowCtl, hidden: bool, size: (f64, f64), min: (f64, f64), ctx: Option<Arc<server::Ctx>>) {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::window::WindowBuilder;
    use wry::{WebViewBuilder, WebViewBuilderExtWindows};

    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    wctl.connect(Box::new(move |cmd| {
        let _ = proxy.send_event(cmd);
    }));

    // same window as the original: 1280x768 min 1024x660 (frameless, Powder draws its own title bar)
    let is_app = ctx.is_some();
    let window = WindowBuilder::new()
        .with_title(if is_app { "Rewind" } else { "Rewind setup" })
        // the app draws Powder's own title bar; the setup window keeps a normal Windows one
        .with_decorations(!is_app)
        .with_inner_size(LogicalSize::new(size.0, size.1))
        .with_min_inner_size(LogicalSize::new(min.0, min.1))
        .with_visible(!hidden)
        .with_window_icon(load_icon())
        .build(&event_loop)
        .unwrap_or_else(|e| fail(&format!("window: {e}")));

    gpu::set_screens(
        window
            .available_monitors()
            .enumerate()
            .map(|(i, m)| gpu::Screen { index: i as i64, name: m.name().unwrap_or_else(|| format!("Screen {}", i + 1)), width: m.size().width, height: m.size().height })
            .collect(),
    );

    let ipc_proxy_win = wctl.clone();
    let webview = WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(std::env::var("POWDER_RS_DEBUG").is_ok())
        // like Electron without a menu: no F5/Ctrl+R reload, no Alt+Left back, no browser right-click menu
        .with_browser_accelerator_keys(false)
        .with_default_context_menus(false)
        .with_background_color((19, 19, 24, 255))
        .with_ipc_handler(move |req| ipc_proxy_win.send(req.body()))
        .with_navigation_handler({
            let base = url.clone();
            move |u| {
                if u.starts_with(&base) || u.starts_with("about:") {
                    true
                } else {
                    features::core::open_path(&u); // external links open in the default browser, like Electron's shell.openExternal
                    false
                }
            }
        })
        .with_new_window_req_handler(|u, _| {
            features::core::open_path(&u);
            wry::NewWindowResponse::Deny
        })
        .build(&window)
        .unwrap_or_else(|e| fail(&format!("WebView2 could not start ({e}). Install the Microsoft Edge WebView2 Runtime from https://developer.microsoft.com/microsoft-edge/webview2/")));

    // Like Powder: the app lives in the notification area. Closing the window (Powder's X hides it)
    // keeps it running there; the tray icon brings it back, its menu quits.
    let tray_proxy = event_loop.create_proxy();
    let mut tray: Option<tray_icon::TrayIcon> = None;
    event_loop.run(move |event, _, control_flow| {
        // the setup window refreshes its page twice a second; the app just waits for events
        *control_flow = if is_app { ControlFlow::Wait } else { ControlFlow::WaitUntil(std::time::Instant::now() + std::time::Duration::from_millis(450)) };
        let _ = (&webview, &ctx);
        if !is_app {
            if let Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. }) | Event::NewEvents(tao::event::StartCause::Init) = event {
                let _ = webview.evaluate_script(&format!("window.__setState && window.__setState({})", setup::state_json()));
            }
        }
        if let Event::UserEvent(cmd) = &event {
            if let Some(json) = cmd.strip_prefix("setup:") {
                let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
                let c = v["cmd"].as_str().unwrap_or("").to_string();
                let a = v["arg"].as_str().unwrap_or("").to_string();
                match c.as_str() {
                    "browse" => {
                        let r = rfd::FileDialog::new().set_title("Select your Powder folder (the one with Powder.exe)").set_parent(&window).pick_folder();
                        setup::picked_folder(r);
                    }
                    "location" => {
                        let r = rfd::FileDialog::new().set_title("Choose where to install Rewind").set_parent(&window).pick_folder();
                        setup::picked_location(r);
                    }
                    "close" => *control_flow = ControlFlow::Exit,
                    "launch" => {
                        if let Some(exe) = setup::launch_target() {
                            let _ = std::process::Command::new(&exe).spawn();
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {
                        std::thread::spawn(move || setup::command(&c, &a));
                    }
                }
                let _ = webview.evaluate_script(&format!("window.__setState && window.__setState({})", setup::state_json()));
                return;
            }
        }
        match event {
            Event::NewEvents(tao::event::StartCause::Init) if is_app => {
                tray = make_tray(tray_proxy.clone());
            }
            Event::UserEvent(cmd) => match cmd.as_str() {
                "tray-open" => {
                    window.set_visible(true);
                    window.set_minimized(false);
                    window.set_focus();
                }
                "tray-record" => {
                    std::thread::spawn(|| {
                        let r = if features::recorder::is_recording() { features::recorder::stop("tray") } else { features::recorder::start() };
                        if let Err(e) = r {
                            log(&format!("tray recording: {e:#}"));
                        }
                    });
                }
                "tray-quit" => {
                    tray.take();
                    *control_flow = ControlFlow::Exit;
                }
                "pick-folder" => {
                    let r = rfd::FileDialog::new().set_title("Select your Powder folder (the one with Powder.exe)").set_parent(&window).pick_folder();
                    setup::picked_folder(r);
                }
                "drag" => {
                    let _ = window.drag_window();
                }
                "minimize" => window.set_minimized(true),
                "toggle-maximize" => window.set_maximized(!window.is_maximized()),
                "show" => {
                    window.set_visible(true);
                    window.set_minimized(false);
                    window.set_focus();
                }
                "hide" => {
                    if tray.is_some() {
                        window.set_visible(false);
                    } else {
                        window.set_minimized(true);
                    }
                }
                "close" | "quit" => {
                    tray.take();
                    *control_flow = ControlFlow::Exit;
                }
                _ => {}
            },
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                tray.take();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
