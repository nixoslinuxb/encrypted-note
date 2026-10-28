//! A hook by which the host application can supply its own clipboard backend.
//!
//! eframe enables egui-winit's `smithay-clipboard` backend, which speaks the
//! `wlr-data-control` protocol. Compositors like KWin only implement the newer
//! `ext-data-control` protocol, so that backend fails. The host app can install
//! an external provider (e.g. one built on ext-data-control) here; `Clipboard`
//! consults it after the built-in backends fail, and mirrors copies to it.

use std::sync::Mutex;

type GetFn = fn() -> Option<String>;
type SetFn = fn(&str);

static GET: Mutex<Option<GetFn>> = Mutex::new(None);
static SET: Mutex<Option<SetFn>> = Mutex::new(None);

/// Install the external clipboard provider.
pub fn install(get: GetFn, set: SetFn) {
    if let Ok(mut g) = GET.lock() {
        *g = Some(get);
    }
    if let Ok(mut s) = SET.lock() {
        *s = Some(set);
    }
}

/// Read from the external provider, if installed.
pub fn get() -> Option<String> {
    let f = GET.lock().ok().and_then(|g| *g);
    f.and_then(|f| f())
}

/// Write to the external provider, if installed.
pub fn set(text: &str) {
    let f = SET.lock().ok().and_then(|s| *s);
    if let Some(f) = f {
        f(text);
    }
}
