//! Best-effort install of a desktop entry + app icon so the logo shows up in
//! taskbars, window lists, and system monitors.
//!
//! Wayland has no per-window icon protocol: a compositor/panel resolves an
//! application's icon from the `.desktop` file whose name matches the window's
//! **app id**. So for a Wayland app to show a custom logo, it needs (a) a
//! `.desktop` file named after its app id with `Icon=<name>`, and (b) an icon
//! file named `<name>.(svg|png)` in the hicolor icon theme.
//!
//! This module writes both under the user's data dir (`$XDG_DATA_HOME`, default
//! `~/.local/share`), idempotently. If the paths aren't writable it does
//! nothing.

use std::fs;
use std::path::PathBuf;

/// Must match the window app id set in `main.rs` (`with_app_id`).
const APP_ID: &str = "encrypted-note";

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Encrypted Note\n\
         GenericName=Markdown Notes\n\
         Comment=A native, Obsidian-like markdown note taking app with AES-256 encryption\n\
         Exec=\"{exec}\" %F\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Office;TextEditor;Security;\n\
         Keywords=notes;markdown;encrypted;aes;obsidian;wiki;\n\
         StartupNotify=true\n\
         StartupWMClass={APP_ID}\n"
    )
}

/// Install the scalable icon and the desktop entry (idempotent).
pub fn install(logo_svg: &[u8]) {
    let Some(data) = data_home() else {
        return;
    };

    // Scalable SVG icon in the hicolor theme.
    let icon_dir = data.join("icons/hicolor/scalable/apps");
    if fs::create_dir_all(&icon_dir).is_ok() {
        let _ = fs::write(icon_dir.join(format!("{APP_ID}.svg")), logo_svg);
    }

    // Desktop entry (name == app id => panels match it to the window).
    let apps = data.join("applications");
    if fs::create_dir_all(&apps).is_ok() {
        let exec = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| APP_ID.to_string());
        let content = desktop_entry(&exec);
        let path = apps.join(format!("{APP_ID}.desktop"));
        let changed = fs::read_to_string(&path)
            .map(|existing| existing != content)
            .unwrap_or(true);
        if changed {
            let _ = fs::write(&path, content);
        }
    }
}
