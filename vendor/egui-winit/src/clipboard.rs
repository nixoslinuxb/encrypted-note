use raw_window_handle::RawDisplayHandle;

/// Handles interfacing with the OS clipboard.
///
/// If the "clipboard" feature is off, or we cannot connect to the OS clipboard,
/// then a fallback clipboard that just works within the same app is used instead.
pub struct Clipboard {
    #[cfg(all(feature = "arboard", not(target_os = "android")))]
    arboard: Option<arboard::Clipboard>,

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ),
        feature = "smithay-clipboard"
    ))]
    smithay: Option<smithay_clipboard::Clipboard>,

    /// Fallback manual clipboard.
    clipboard: String,
}

impl Clipboard {
    /// Construct a new instance
    pub fn new(_raw_display_handle: Option<RawDisplayHandle>) -> Self {
        Self {
            #[cfg(all(feature = "arboard", not(target_os = "android")))]
            arboard: init_arboard(),

            #[cfg(all(
                any(
                    target_os = "linux",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd"
                ),
                feature = "smithay-clipboard"
            ))]
            smithay: init_smithay_clipboard(_raw_display_handle),

            clipboard: Default::default(),
        }
    }

    pub fn get(&mut self) -> Option<String> {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ),
            feature = "smithay-clipboard"
        ))]
        if let Some(clipboard) = &mut self.smithay {
            match clipboard.load() {
                Ok(text) => {
                    self.clipboard = text.clone();
                    return Some(text);
                }
                Err(err) => {
                    // e.g. KWin implements ext-data-control but not the older
                    // wlr-data-control protocol smithay uses, so fall back to the
                    // in-app clipboard rather than losing paste entirely.
                    log::error!("smithay paste error: {err}; using in-app clipboard");
                }
            }
        }

        #[cfg(all(feature = "arboard", not(target_os = "android")))]
        if let Some(clipboard) = &mut self.arboard {
            match clipboard.get_text() {
                Ok(text) => {
                    self.clipboard = text.clone();
                    return Some(text);
                }
                Err(err) => {
                    log::error!("arboard paste error: {err}; using in-app clipboard");
                }
            }
        }

        // A host-installed backend (e.g. ext-data-control on KWin).
        if let Some(text) = crate::external_clipboard::get() {
            self.clipboard = text.clone();
            return Some(text);
        }

        Some(self.clipboard.clone())
    }

    pub fn set(&mut self, text: String) {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ),
            feature = "smithay-clipboard"
        ))]
        if let Some(clipboard) = &mut self.smithay {
            clipboard.store(text.clone());
        }

        #[cfg(all(feature = "arboard", not(target_os = "android")))]
        if let Some(clipboard) = &mut self.arboard {
            if let Err(err) = clipboard.set_text(text.clone()) {
                log::error!("arboard copy/cut error: {err}");
            }
        }

        // Mirror to the host-installed backend (e.g. ext-data-control on KWin).
        crate::external_clipboard::set(&text);

        // Always keep an in-app copy so paste still works when the platform
        // clipboard backend is unavailable (e.g. KWin's ext-data-control).
        self.clipboard = text;
    }
}

#[cfg(all(feature = "arboard", not(target_os = "android")))]
fn init_arboard() -> Option<arboard::Clipboard> {
    crate::profile_function!();

    log::trace!("Initializing arboard clipboard…");
    match arboard::Clipboard::new() {
        Ok(clipboard) => Some(clipboard),
        Err(err) => {
            log::warn!("Failed to initialize arboard clipboard: {err}");
            None
        }
    }
}

#[cfg(all(
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ),
    feature = "smithay-clipboard"
))]
fn init_smithay_clipboard(
    raw_display_handle: Option<RawDisplayHandle>,
) -> Option<smithay_clipboard::Clipboard> {
    crate::profile_function!();

    if let Some(RawDisplayHandle::Wayland(display)) = raw_display_handle {
        log::trace!("Initializing smithay clipboard…");
        #[allow(unsafe_code)]
        Some(unsafe { smithay_clipboard::Clipboard::new(display.display.as_ptr()) })
    } else {
        #[cfg(feature = "wayland")]
        log::debug!("Cannot init smithay clipboard without a Wayland display handle");
        #[cfg(not(feature = "wayland"))]
        log::debug!(
            "Cannot init smithay clipboard: the 'wayland' feature of 'egui-winit' is not enabled"
        );
        None
    }
}
