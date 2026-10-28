//! Encrypted Note — a native, Obsidian-like markdown note taking app whose
//! contents are encrypted at rest with AES-256-GCM, protected by a master
//! password (Argon2id-derived key).
//!
//! This app is **Wayland-only**: it refuses to start without a Wayland session
//! and is built without any X11/XCB backend (so XWayland is not used either).

mod app;
mod canvas;
mod config;
mod crypto;
mod dates;
mod desktop;
mod feeds;
mod frontmatter;
mod graph;
mod icons;
mod index;
mod markdown;
mod math;
mod media;
mod mermaid;
mod palette;
mod passwords;
mod search;
mod system_clipboard;
mod theme;
mod vault;
mod vim;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // Enforce Wayland: we deliberately do not support X11 or XWayland.
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!(
            "Encrypted Note requires a Wayland session, but WAYLAND_DISPLAY is not set.\n\
             This app intentionally does not support X11 (including XWayland)."
        );
        std::process::exit(1);
    }

    if std::env::args().any(|a| a == "--clip-selftest") {
        system_clipboard::init();
        std::thread::sleep(std::time::Duration::from_millis(900));
        system_clipboard::set("encrypted-note-clip-selftest");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        eprintln!("clip-selftest get = {:?}", system_clipboard::get());
        std::process::exit(0);
    }

    // Enforce a single running instance.
    let _instance = match single_instance_guard() {
        Some(guard) => guard,
        None => {
            eprintln!("Encrypted Note is already running — focus the existing window.");
            std::process::exit(0);
        }
    };

    // Install the ext-data-control clipboard bridge so copy/paste works with
    // other apps on KWin (which lacks the wlr-data-control protocol that
    // eframe/egui-winit otherwise use).
    system_clipboard::init();
    egui_winit::external_clipboard::install(system_clipboard::get, system_clipboard::set);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Encrypted Note")
        .with_app_id("encrypted-note")
        .with_inner_size([1280.0, 820.0])
        .with_min_inner_size([720.0, 480.0]);
    // Start windowed so the login screen is a normal window; the app switches to
    // the saved window mode (fullscreen by default) after unlocking.
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }

    // Install a desktop entry + icon so panels show our logo (Wayland looks the
    // icon up from the `.desktop` file matching the window app id).
    desktop::install(include_bytes!("../assets/logo.svg"));

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Encrypted Note",
        native_options,
        Box::new(|cc| Box::new(app::EncryptedNoteApp::new(cc)) as Box<dyn eframe::App>),
    )
}

/// Bind a per-user Unix socket to ensure only one instance of the app runs.
/// Returns the listener (kept alive for the process lifetime), or `None` when
/// another instance already holds it.
fn single_instance_guard() -> Option<std::os::unix::net::UnixListener> {
    use std::os::unix::net::{UnixListener, UnixStream};
    // Prefer the per-user runtime dir; fall back to the per-user config dir (not
    // /tmp, which other users could squat on with a pre-created socket).
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
                })
                .unwrap_or_else(std::env::temp_dir)
                .join("encrypted-note")
        });
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("encrypted-note.sock");
    if path.exists() {
        // A live listener answers a connect; a stale socket file does not.
        if UnixStream::connect(&path).is_ok() {
            return None;
        }
        let _ = std::fs::remove_file(&path);
    }
    UnixListener::bind(&path).ok()
}

/// Rasterise the SVG logo into RGBA pixels for the window/taskbar icon.
fn app_icon() -> Option<egui::IconData> {
    use resvg::usvg::TreeParsing;
    let bytes: &[u8] = include_bytes!("../assets/logo.svg");
    let usvg_tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = usvg_tree.size.to_int_size();
    let (width, height) = (size.width(), size.height());
    let tree = resvg::Tree::from_usvg(&usvg_tree);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)?;
    tree.render(resvg::tiny_skia::Transform::default(), &mut pixmap.as_mut());
    Some(egui::IconData {
        rgba: pixmap.take(),
        width,
        height,
    })
}
