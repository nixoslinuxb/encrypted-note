//! System clipboard bridge using Wayland's **ext-data-control** protocol.
//!
//! eframe/egui-winit use `smithay-clipboard`, which speaks `wlr-data-control`.
//! KDE's KWin only implements the newer `ext-data-control`, so the built-in
//! backend fails there and the system clipboard is unusable. This module talks
//! to `ext_data_control_manager_v1` directly on its own connections:
//!
//! * a **reader** thread watches the current selection and caches its text;
//! * each copy spawns a short-lived **writer** thread that serves a data source
//!   until the compositor cancels it (i.e. until something else is copied).
//!
//! The app installs [`get`]/[`set`] into the vendored egui-winit via
//! `egui_winit::external_clipboard::install`, so `Ctrl+C`/`Ctrl+V` work with
//! other applications as well.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::{
    self, ExtDataControlManagerV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self, ExtDataControlSourceV1,
};

/// Text mime types we offer/accept, most preferred first.
const MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Reader,
    Writer,
}

/// Event-loop state shared by the reader and writer connections.
#[derive(Default)]
struct State {
    mode: Mode,
    /// Reader: where to publish selected text.
    shared: Option<Arc<Mutex<String>>>,
    /// Writer: the text to serve.
    text: String,
    /// Offers seen (and their advertised mime types).
    offers: Vec<(ExtDataControlOfferV1, Vec<String>)>,
    cancelled: bool,
}

struct Clip {
    text: Arc<Mutex<String>>,
    tx: Sender<String>,
}

static CLIP: OnceLock<Option<Clip>> = OnceLock::new();

/// Start the clipboard bridge (idempotent).
pub fn init() {
    let text = Arc::new(Mutex::new(String::new()));
    let (tx, rx) = channel::<String>();

    let reader_text = text.clone();
    std::thread::spawn(move || reader_loop(reader_text));

    std::thread::spawn(move || {
        for t in rx {
            writer_loop(&t);
        }
    });

    let _ = CLIP.set(Some(Clip { text, tx }));
}

/// The last text seen on the system clipboard, if any.
pub fn get() -> Option<String> {
    let clip = CLIP.get()?.as_ref()?;
    let s = clip.text.lock().ok()?.clone();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Place `text` on the system clipboard.
pub fn set(text: &str) {
    if let Some(Some(clip)) = CLIP.get() {
        if let Ok(mut t) = clip.text.lock() {
            *t = text.to_string();
        }
        let _ = clip.tx.send(text.to_string());
    }
}

// ---------------------------------------------------------------- connections

fn bind_globals(conn: &Connection) -> Option<(ExtDataControlManagerV1, WlSeat, EventQueue<State>)> {
    let (globals, queue) = registry_queue_init::<State>(conn).ok()?;
    let qh = queue.handle();
    let manager = globals
        .bind::<ExtDataControlManagerV1, State, ()>(&qh, 1..=1, ())
        .ok()?;
    let seat = globals.bind::<WlSeat, State, ()>(&qh, 1..=9, ()).ok()?;
    Some((manager, seat, queue))
}

fn reader_loop(shared: Arc<Mutex<String>>) {
    let Ok(conn) = Connection::connect_to_env() else {
        return;
    };
    let Some((manager, seat, mut queue)) = bind_globals(&conn) else {
        return;
    };
    let _device = manager.get_data_device(&seat, &queue.handle(), ());
    let mut state = State {
        mode: Mode::Reader,
        shared: Some(shared),
        ..Default::default()
    };
    let _ = conn.flush();
    loop {
        if queue.blocking_dispatch(&mut state).is_err() {
            break;
        }
    }
}

fn writer_loop(text: &str) {
    let Ok(conn) = Connection::connect_to_env() else {
        return;
    };
    let Some((manager, seat, mut queue)) = bind_globals(&conn) else {
        return;
    };
    let qh = queue.handle();
    let device = manager.get_data_device(&seat, &qh, ());
    let source = manager.create_data_source(&qh, ());
    for mime in MIMES {
        source.offer((*mime).to_string());
    }
    device.set_selection(Some(&source));
    let _ = conn.flush();

    let mut state = State {
        mode: Mode::Writer,
        text: text.to_string(),
        ..Default::default()
    };
    loop {
        if state.cancelled {
            break;
        }
        if queue.blocking_dispatch(&mut state).is_err() {
            break;
        }
    }
    source.destroy();
    let _ = conn.flush();
}

// ------------------------------------------------------------------- dispatch

/// Read the offered text via a pipe and publish it.
fn read_offer(state: &mut State, conn: &Connection, offer: &ExtDataControlOfferV1) {
    let mimes = state
        .offers
        .iter()
        .find(|(o, _)| o == offer)
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    let Some(mime) = MIMES
        .iter()
        .find(|want| mimes.iter().any(|m| m == *want))
        .map(|m| (*m).to_string())
    else {
        return;
    };
    let Ok((reader, writer)) = UnixStream::pair() else {
        return;
    };
    let _ = reader.set_read_timeout(Some(Duration::from_millis(800)));
    offer.receive(mime, writer.as_fd());
    let _ = conn.flush();
    drop(writer);

    let mut buf = Vec::new();
    let mut reader = reader;
    let _ = reader.read_to_end(&mut buf);
    if buf.is_empty() {
        return;
    }
    if let (Some(shared), Ok(text)) = (&state.shared, String::from_utf8(buf)) {
        if let Ok(mut guard) = shared.lock() {
            *guard = text;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ExtDataControlManagerV1,
        _event: ext_data_control_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _proxy: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_data_control_device_v1::Event;
        match event {
            Event::DataOffer { id } => {
                state.offers.push((id, Vec::new()));
            }
            Event::Selection { id } => {
                if state.mode == Mode::Reader {
                    if let Some(offer) = id {
                        read_offer(state, conn, &offer);
                    }
                }
            }
            Event::PrimarySelection { .. } => {}
            Event::Finished => state.cancelled = true,
            _ => {}
        }
    }

    fn event_created_child(
        opcode: u16,
        qhandle: &QueueHandle<Self>,
    ) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        match opcode {
            ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => {
                qhandle.make_data::<ExtDataControlOfferV1, ()>(())
            }
            _ => unreachable!("unexpected ext_data_control_device_v1 child event"),
        }
    }
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_data_control_offer_v1::Event;
        if let Event::Offer { mime_type } = event {
            if let Some((_, mimes)) = state.offers.iter_mut().find(|(o, _)| o == proxy) {
                mimes.push(mime_type);
            }
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        _proxy: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_data_control_source_v1::Event;
        match event {
            Event::Send { fd, .. } => {
                let mut file = File::from(fd);
                let _ = file.write_all(state.text.as_bytes());
                // Dropping `file` closes the fd, signalling EOF to the reader.
            }
            Event::Cancelled => state.cancelled = true,
            _ => {}
        }
    }
}
