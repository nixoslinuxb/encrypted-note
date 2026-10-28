//! The main `eframe::App` implementation and all of the UI.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui::{self, Color32, RichText};

use crate::canvas;
use crate::config::{self, Config};
use crate::crypto::{self, Keyfile, VaultCrypto};
use crate::feeds;
use crate::frontmatter;
use crate::graph;
use crate::icons::{self, Icon};
use crate::index::{self, NoteIndex};
use crate::markdown;
use crate::media;
use crate::palette;
use crate::passwords;
use crate::search;
use crate::theme;
use crate::vault::{TreeNode, Vault};
use crate::vim;

/// Refresh results from a background feed fetch.
type FeedRx = mpsc::Receiver<(u32, Result<(String, Vec<feeds::Item>), String>)>;
/// Results from a background favicon fetch (url, bytes).
type FaviconRx = mpsc::Receiver<(String, Result<Vec<u8>, String>)>;

/// What kind of file the attachment picker is looking for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachKind {
    Image,
    Audio,
    Video,
}

/// A playback action chosen from the inline media controls.
#[derive(Clone, Copy)]
enum Transport {
    Start,
    Toggle,
    Stop,
    Seek(f32),
}

/// An entry being added or edited in the password manager.
struct PwEditor {
    draft: passwords::Entry,
    is_new: bool,
    show: bool,
    /// Whether the inline "new folder" field is showing.
    new_folder: bool,
    new_folder_name: String,
}

/// Actions requested by the password detail view, applied after the UI closure.
#[derive(Default)]
struct PwActions {
    fav: bool,
    edit: bool,
    del: bool,
    toggle_reveal: bool,
    copy: Option<(&'static str, String)>,
    open_url: Option<String>,
}

/// The action chosen on a credential row in the password tree.
#[derive(Clone, Copy)]
enum PwRowAction {
    Select,
    Edit,
    Delete,
}

impl AttachKind {
    fn label(self) -> &'static str {
        match self {
            AttachKind::Image => "image",
            AttachKind::Audio => "voice recording / audio",
            AttachKind::Video => "video",
        }
    }
    fn matches(self, ext: &str) -> bool {
        match self {
            AttachKind::Image => is_image_ext(ext),
            AttachKind::Audio => is_audio_ext(ext),
            AttachKind::Video => is_video_ext(ext),
        }
    }
    fn start_dir(self) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let candidates = match self {
            AttachKind::Image => vec!["Pictures", "Images"],
            AttachKind::Audio => vec!["Music", "Recordings", "Voice"],
            AttachKind::Video => vec!["Videos", "Movies"],
        };
        for c in candidates {
            let p = home.join(c);
            if p.is_dir() {
                return p;
            }
        }
        home
    }
}

/// A minimal file chooser (dirs + matching files) for attachments.
struct FilePicker {
    current: PathBuf,
    dirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    error: Option<String>,
    kind: AttachKind,
}

impl FilePicker {
    fn new(start: PathBuf, kind: AttachKind) -> Self {
        let mut p = FilePicker {
            current: start,
            dirs: Vec::new(),
            files: Vec::new(),
            error: None,
            kind,
        };
        p.refresh();
        p
    }
    fn refresh(&mut self) {
        self.dirs.clear();
        self.files.clear();
        match std::fs::read_dir(&self.current) {
            Ok(entries) => {
                for e in entries.flatten() {
                    let name = e.file_name();
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        self.dirs.push(e.path());
                    } else {
                        let ext = e
                            .path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if self.kind.matches(&ext) {
                            self.files.push(e.path());
                        }
                    }
                }
                self.dirs
                    .sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                self.files
                    .sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot read directory: {e}")),
        }
    }
    fn enter(&mut self, p: PathBuf) {
        self.current = p;
        self.refresh();
    }
    fn go_up(&mut self) {
        if let Some(parent) = self.current.parent() {
            let p = parent.to_path_buf();
            self.enter(p);
        }
    }
}

/// An open `.canvas` board.
struct CanvasView {
    path: PathBuf,
    data: canvas::Canvas,
    /// Screen position of world origin.
    offset: egui::Vec2,
    zoom: f32,
    /// Whether to re-centre on the next frame.
    fit: bool,
    /// Selected node ids.
    sel: std::collections::HashSet<String>,
    /// Node currently being text-edited, if any.
    editing: Option<String>,
    /// Source node while dragging a connector.
    edge_from: Option<String>,
    /// Rubber-band selection, in world coordinates.
    marquee: Option<(egui::Vec2, egui::Vec2)>,
    /// Undo/redo history of whole-canvas snapshots.
    undo: Vec<canvas::Canvas>,
    redo: Vec<canvas::Canvas>,
    /// Snapshot + original text captured while inline-editing a card.
    edit_undo: Option<(canvas::Canvas, String)>,
}

impl CanvasView {
    fn snapshot(&mut self) {
        self.undo.push(self.data.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.data, prev));
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.data, next));
        }
    }
}

/// Obsidian canvas colour presets (`1`..`6`).
fn canvas_color(code: &str) -> Option<Color32> {
    Some(match code {
        "1" => Color32::from_rgb(0xef, 0x6b, 0x6b),
        "2" => Color32::from_rgb(0xf0, 0x9f, 0x4a),
        "3" => Color32::from_rgb(0xe0, 0xc0, 0x4f),
        "4" => Color32::from_rgb(0x5f, 0xc2, 0x7a),
        "5" => Color32::from_rgb(0x4a, 0xc3, 0xe0),
        "6" => Color32::from_rgb(0x9b, 0x7f, 0xf0),
        _ => return None,
    })
}

const CANVAS_COLORS: [&str; 6] = ["1", "2", "3", "4", "5", "6"];

/// How the window is shown at startup / after toggling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowMode {
    Fullscreen,
    Maximized,
    Windowed,
}

impl WindowMode {
    fn from_str(s: &str) -> Self {
        match s {
            "maximized" => WindowMode::Maximized,
            "windowed" => WindowMode::Windowed,
            _ => WindowMode::Fullscreen,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            WindowMode::Fullscreen => "fullscreen",
            WindowMode::Maximized => "maximized",
            WindowMode::Windowed => "windowed",
        }
    }
    fn label(self) -> &'static str {
        match self {
            WindowMode::Fullscreen => "Fullscreen",
            WindowMode::Maximized => "Maximized",
            WindowMode::Windowed => "Windowed",
        }
    }
    fn next(self) -> Self {
        match self {
            WindowMode::Fullscreen => WindowMode::Maximized,
            WindowMode::Maximized => WindowMode::Windowed,
            WindowMode::Windowed => WindowMode::Fullscreen,
        }
    }
}

/// State for the command palette / quick switcher.
struct PaletteState {
    query: String,
    selected: usize,
    focus: bool,
}

/// Something the palette can open or run.
#[derive(Clone)]
enum PaletteItem {
    Note(PathBuf),
    Command(Command),
}

/// App commands available from the palette.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    NewNote,
    NewSubnote,
    OpenVault,
    DailyNote,
    DailyPrev,
    DailyNext,
    DailySettings,
    NewFromTemplate,
    NewCanvas,
    OpenCanvas,
    OpenFeeds,
    OpenPasswords,
    Undo,
    Redo,
    WindowMode,
    ToggleVim,
    TogglePreview,
    TogglePanel,
    ToggleGraph,
    Lock,
    CloseTab,
    Quit,
}

impl Command {
    const ALL: [Command; 22] = [
        Command::NewNote,
        Command::NewSubnote,
        Command::OpenVault,
        Command::DailyNote,
        Command::DailyPrev,
        Command::DailyNext,
        Command::DailySettings,
        Command::NewFromTemplate,
        Command::NewCanvas,
        Command::OpenCanvas,
        Command::OpenFeeds,
        Command::OpenPasswords,
        Command::Undo,
        Command::Redo,
        Command::WindowMode,
        Command::ToggleVim,
        Command::TogglePreview,
        Command::TogglePanel,
        Command::ToggleGraph,
        Command::Lock,
        Command::CloseTab,
        Command::Quit,
    ];

    fn label(self) -> &'static str {
        match self {
            Command::NewNote => "New note",
            Command::NewSubnote => "New sub-note (under current note)",
            Command::OpenVault => "Open vault",
            Command::DailyNote => "Open today's daily note",
            Command::DailyPrev => "Open previous daily note",
            Command::DailyNext => "Open next daily note",
            Command::DailySettings => "Daily notes settings",
            Command::NewFromTemplate => "New note from template",
            Command::NewCanvas => "New canvas",
            Command::OpenCanvas => "Open canvas board",
            Command::OpenFeeds => "RSS feeds",
            Command::OpenPasswords => "Password manager",
            Command::Undo => "Undo",
            Command::Redo => "Redo",
            Command::WindowMode => "Cycle window mode (F11)",
            Command::ToggleVim => "Toggle Vim mode",
            Command::TogglePreview => "Cycle editor view (edit/split/preview)",
            Command::TogglePanel => "Toggle side panel",
            Command::ToggleGraph => "Toggle graph view",
            Command::Lock => "Lock the vault",
            Command::CloseTab => "Close current tab",
            Command::Quit => "Quit",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Command::NewNote => Icon::Add,
            Command::NewSubnote => Icon::Folder,
            Command::OpenVault => Icon::FolderOpen,
            Command::DailyNote => Icon::Recent,
            Command::DailyPrev => Icon::Backlink,
            Command::DailyNext => Icon::ChevronRight,
            Command::DailySettings => Icon::Edit,
            Command::NewFromTemplate => Icon::File,
            Command::NewCanvas => Icon::Add,
            Command::OpenCanvas => Icon::Graph,
            Command::OpenFeeds => Icon::Rss,
            Command::OpenPasswords => Icon::Passwords,
            Command::Undo => Icon::Undo,
            Command::Redo => Icon::Redo,
            Command::WindowMode => Icon::PanelRight,
            Command::ToggleVim => Icon::Edit,
            Command::TogglePreview => Icon::Split,
            Command::TogglePanel => Icon::PanelRight,
            Command::ToggleGraph => Icon::Graph,
            Command::Lock => Icon::Lock,
            Command::CloseTab => Icon::Close,
            Command::Quit => Icon::Close,
        }
    }
}

/// How a note is displayed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Edit,
    Split,
    Preview,
    Live,
}

/// One open note.
struct Tab {
    path: PathBuf,
    title: String,
    text: String,
    saved: String,
    view: ViewMode,
    /// Past text states (oldest first) for undo.
    undo: Vec<String>,
    /// Future text states for redo.
    redo: Vec<String>,
}

impl Tab {
    fn dirty(&self) -> bool {
        self.text != self.saved
    }
}

/// A lightweight built-in directory chooser (avoids pulling in a file-dialog
/// dependency for a Wayland-native app).
struct DirPicker {
    current: PathBuf,
    subdirs: Vec<PathBuf>,
    manual: String,
    error: Option<String>,
}

impl DirPicker {
    fn new(start: PathBuf) -> Self {
        let mut picker = DirPicker {
            current: start,
            subdirs: Vec::new(),
            manual: String::new(),
            error: None,
        };
        picker.refresh();
        picker
    }

    fn refresh(&mut self) {
        self.subdirs.clear();
        match std::fs::read_dir(&self.current) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let name = entry.file_name();
                        if name.to_string_lossy().starts_with('.') {
                            continue;
                        }
                        self.subdirs.push(entry.path());
                    }
                }
                self.subdirs
                    .sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Cannot read directory: {e}")),
        }
    }

    fn enter(&mut self, path: PathBuf) {
        self.current = path;
        self.refresh();
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.current.parent() {
            let parent = parent.to_path_buf();
            self.enter(parent);
        }
    }
}

/// The login gate that stands in front of the vault. Notes are only readable
/// once the master password has been accepted.
enum Gate {
    /// No master password configured yet: the user must create one.
    CreatePassword {
        password: String,
        confirm: String,
        error: Option<String>,
    },
    /// A master password exists: the user must enter it.
    Unlock {
        password: String,
        error: Option<String>,
    },
    /// Unlocked: normal operation.
    Open,
}

/// The action requested by the password screen for this frame.
enum GateAction {
    None,
    Create,
    Unlock,
}

pub struct EncryptedNoteApp {
    config: Config,
    /// Present only while unlocked; dropped (and its key zeroed) on lock/exit.
    crypto: Option<VaultCrypto>,
    gate: Gate,
    vault: Option<Vault>,
    tree: TreeNode,
    tabs: Vec<Tab>,
    active: usize,

    // Sidebar / search
    search: String,
    focus_search: bool,
    show_search_results: bool,
    /// Directory rel-paths currently expanded in the sidebar tree.
    expanded_dirs: HashSet<PathBuf>,
    /// Optional tag filter: when set, the sidebar lists notes with this tag.
    tag_filter: Option<String>,

    // Indexes over the vault (titles, aliases, tags, links).
    index: NoteIndex,
    metas: BTreeMap<PathBuf, frontmatter::Meta>,
    titles: BTreeMap<PathBuf, String>,

    // Views
    show_graph: bool,
    palette: Option<PaletteState>,
    /// The Vim editing engine (when `config.vim` is on).
    vim: crate::vim::Vim,
    /// The line being edited in Live Preview mode.
    live_line: usize,
    /// An open canvas board, if any.
    canvas: Option<CanvasView>,
    /// The RSS reader.
    feeds_open: bool,
    feeds: feeds::Store,
    feed_sel: Option<u32>,
    feed_article: Option<usize>,
    feed_search: String,
    feed_add_open: bool,
    feed_url: String,
    feed_title: String,
    feed_category: String,
    feed_edit_id: Option<u32>,
    feed_delete_id: Option<u32>,
    /// Refresh results from background threads.
    feed_rx: Option<FeedRx>,
    feed_refreshing: bool,
    /// Attachment file chooser.
    attach_picker: Option<FilePicker>,
    /// Full-article fetch results (keyed by link).
    feed_full_rx: Option<mpsc::Receiver<(String, Result<String, String>)>>,
    feed_full_loading: Option<String>,
    /// Canvas chooser, when open (`Open canvas board` with >1 board).
    canvas_pick: Option<Vec<PathBuf>>,
    /// Template chooser, when open.
    template_pick: Option<Vec<PathBuf>>,
    /// Daily-notes settings dialog, when open.
    daily_settings_open: bool,
    /// New-canvas dialog.
    new_canvas_open: bool,
    new_canvas_name: String,
    /// New-sub-note dialog.
    subnote_open: bool,
    subnote_name: String,
    subnote_parent: Option<PathBuf>,
    /// Inline validation errors shown under the name fields.
    new_note_error: Option<String>,
    subnote_error: Option<String>,
    /// Where to place the next note action dialog (near the note row).
    dialog_pos: Option<egui::Pos2>,
    /// Calendar sidebar: currently shown month.
    cal_year: i32,
    cal_month: u32,
    /// A daily note the calendar asked to open, applied after the UI.
    pending_daily: Option<(i32, u32, u32)>,
    // Graph layout cache.
    graph_nodes: Vec<PathBuf>,
    graph_edges: Vec<(usize, usize)>,
    graph_pos: Vec<(f32, f32)>,
    /// Pan / zoom of the graph view (Obsidian-like navigation).
    graph_zoom: f32,
    graph_pan: egui::Vec2,
    // Graph filters.
    graph_local: bool,
    graph_depth: usize,
    graph_tag: String,
    graph_show_orphans: bool,

    // In-app media playback (audio / video) for note attachments.
    player: Option<media::Player>,
    /// Absolute path of the media currently loaded in `player`.
    player_path: PathBuf,
    /// Texture holding the newest decoded video frame.
    player_tex: Option<egui::TextureHandle>,
    /// Set each frame when the playing media was drawn, so playback stops when
    /// you navigate away from the note that contains it.
    player_rendered: bool,
    /// Per-attachment image layout (size + position).
    media_layout: media::Layouts,
    /// Decoded note images, keyed by path (path → GPU texture).
    media_textures: HashMap<PathBuf, egui::TextureHandle>,

    // Password manager.
    passwords_open: bool,
    passwords: passwords::Store,
    pw_sel: Option<u64>,
    pw_search: String,
    /// `None` = all categories; `Some("")` = uncategorized.
    pw_category: Option<String>,
    pw_fav_only: bool,
    /// Folders (and `""` for Uncategorized) that are expanded in the tree.
    pw_expanded: HashSet<String>,
    /// Entry ids whose password is currently revealed.
    pw_reveal: HashSet<u64>,
    /// Entry currently being added/edited.
    pw_editor: Option<PwEditor>,
    /// Entry awaiting delete confirmation.
    pw_confirm_delete: Option<u64>,
    /// Password-generator popup state.
    pw_gen_open: bool,
    pw_gen_len: usize,
    pw_gen_upper: bool,
    pw_gen_lower: bool,
    pw_gen_digits: bool,
    pw_gen_special: bool,
    /// The most recently generated password.
    pw_gen_value: String,
    /// Password import/export dialog.
    pw_io_open: bool,
    /// File path used by the import/export dialog.
    pw_io_path: String,
    /// Password used to encrypt (export) or decrypt (import) the file.
    pw_io_password: String,
    /// Validation message for the import/export dialog.
    pw_io_error: Option<String>,
    /// "New folder" dialog.
    pw_new_folder_open: bool,
    pw_new_folder_name: String,
    /// Folder being renamed (and its draft name).
    pw_folder_rename: Option<String>,
    pw_folder_rename_name: String,
    /// Folder awaiting delete confirmation.
    pw_folder_delete: Option<String>,
    /// Favicon (icon-from-URL) support for password entries.
    favicon_rx: Option<FaviconRx>,
    favicon_tex: HashMap<String, egui::TextureHandle>,
    favicon_url: String,
    favicon_last: String,
    favicon_change_at: f64,
    favicon_error: Option<String>,
    /// Whether the unlock screen's password field is revealed.
    gate_show: bool,

    // Right panel
    show_right: bool,
    backlinks: Vec<(PathBuf, usize, String)>,
    unlinked: Vec<(PathBuf, usize, String)>,
    last_active: Option<PathBuf>,

    // Dialogs
    picker: Option<DirPicker>,
    new_note_open: bool,
    new_note_name: String,
    rename_target: Option<PathBuf>,
    rename_name: String,
    confirm_delete: Option<PathBuf>,
    /// Whether the "forgot password" erase-confirmation is showing.
    confirm_forgot: bool,
    /// Whether the post-setup "record your password" popup is showing.
    show_password_warning: bool,

    // Deferred actions (applied after the UI closures finish, to keep
    // borrowing simple).
    pending_open: Option<PathBuf>,
    pending_open_vault: Option<PathBuf>,

    status: String,
    last_autosave: f64,
    /// Last window title we sent (to avoid per-frame viewport commands).
    window_title: String,
    /// Last screen rect we saw (to repaint on resize).
    last_screen: Option<egui::Rect>,
    /// Current window mode (cycled with F11).
    window_mode: WindowMode,
    /// Whether we've requested keyboard focus yet.
    requested_focus: bool,
    /// Whether we've logged the GPU adapter yet.
    logged_renderer: bool,
}

impl EncryptedNoteApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let initial_show_right = config.show_right;
        let initial_window_mode = config.window_mode.clone();
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        // Enable SVG (and raster) image loading for the icon set.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        // Decide the initial gate: create a password on first run, otherwise
        // require the existing one. There is no passwordless path.
        let gate = if Keyfile::exists() {
            Gate::Unlock {
                password: String::new(),
                error: None,
            }
        } else {
            Gate::CreatePassword {
                password: String::new(),
                confirm: String::new(),
                error: None,
            }
        };

        let app = EncryptedNoteApp {
            config,
            crypto: None,
            gate,
            vault: None,
            tree: TreeNode::default(),
            tabs: Vec::new(),
            active: 0,
            search: String::new(),
            focus_search: false,
            show_search_results: false,
            expanded_dirs: HashSet::new(),
            tag_filter: None,
            index: NoteIndex::default(),
            metas: BTreeMap::new(),
            titles: BTreeMap::new(),
            show_graph: false,
            palette: None,
            vim: crate::vim::Vim::new(),
            live_line: 0,
            canvas: None,
            feeds_open: false,
            feeds: feeds::Store::load(),
            feed_sel: None,
            feed_article: None,
            feed_search: String::new(),
            feed_add_open: false,
            feed_url: String::new(),
            feed_title: String::new(),
            feed_category: String::new(),
            feed_edit_id: None,
            feed_delete_id: None,
            feed_rx: None,
            feed_refreshing: false,
            attach_picker: None,
            feed_full_rx: None,
            feed_full_loading: None,
            canvas_pick: None,
            template_pick: None,
            daily_settings_open: false,
            new_canvas_open: false,
            new_canvas_name: String::new(),
            subnote_open: false,
            subnote_name: String::new(),
            subnote_parent: None,
            new_note_error: None,
            subnote_error: None,
            dialog_pos: None,
            cal_year: crate::dates::today().0,
            cal_month: crate::dates::today().1,
            pending_daily: None,
            graph_nodes: Vec::new(),
            graph_edges: Vec::new(),
            graph_pos: Vec::new(),
            graph_zoom: 1.0,
            graph_pan: egui::Vec2::ZERO,
            graph_local: false,
            graph_depth: 2,
            graph_tag: String::new(),
            graph_show_orphans: true,
            player: None,
            player_path: PathBuf::new(),
            player_tex: None,
            player_rendered: false,
            media_layout: media::Layouts::load(),
            media_textures: HashMap::new(),
            passwords_open: false,
            passwords: passwords::Store::default(),
            pw_sel: None,
            pw_search: String::new(),
            pw_category: None,
            pw_fav_only: false,
            pw_expanded: HashSet::new(),
            pw_reveal: HashSet::new(),
            pw_editor: None,
            pw_confirm_delete: None,
            pw_gen_open: false,
            pw_gen_len: 20,
            pw_gen_upper: true,
            pw_gen_lower: true,
            pw_gen_digits: true,
            pw_gen_special: true,
            pw_gen_value: String::new(),
            pw_io_open: false,
            pw_io_path: default_export_path(),
            pw_io_password: String::new(),
            pw_io_error: None,
            pw_new_folder_open: false,
            pw_new_folder_name: String::new(),
            pw_folder_rename: None,
            pw_folder_rename_name: String::new(),
            pw_folder_delete: None,
            favicon_rx: None,
            favicon_tex: HashMap::new(),
            favicon_url: String::new(),
            favicon_last: String::new(),
            favicon_change_at: 0.0,
            favicon_error: None,
            gate_show: false,
            show_right: initial_show_right,
            backlinks: Vec::new(),
            unlinked: Vec::new(),
            last_active: None,
            picker: None,
            new_note_open: false,
            new_note_name: String::new(),
            rename_target: None,
            rename_name: String::new(),
            confirm_delete: None,
            confirm_forgot: false,
            show_password_warning: false,
            pending_open: None,
            pending_open_vault: None,
            status: String::new(),
            last_autosave: 0.0,
            window_title: String::new(),
            last_screen: None,
            window_mode: WindowMode::from_str(&initial_window_mode),
            requested_focus: false,
            logged_renderer: false,
        };
        theme::apply(&cc.egui_ctx, app.config.font_size);
        app
    }

    // ---------------------------------------------------------------- vault

    /// Read and decrypt a note. Files that are still plaintext markdown (e.g.
    /// from before encryption was enabled) are accepted as-is.
    fn read_note(&self, path: &Path) -> Option<String> {
        let data = Vault::read_bytes(path).ok()?;
        if crypto::is_encrypted(&data) {
            self.crypto.as_ref()?.decrypt(&data)
        } else {
            String::from_utf8(data).ok()
        }
    }

    /// Encrypt and write a note. Requires the vault to be unlocked.
    fn write_note(&self, path: &Path, text: &str) -> std::io::Result<()> {
        let crypto = self.crypto.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "vault is locked")
        })?;
        let blob = crypto.encrypt(text);
        Vault::write_bytes(path, &blob)
    }

    /// Encrypt every plaintext note in the current vault. Runs once after
    /// unlock so that any legacy/unencrypted files become encrypted at rest.
    fn migrate_plaintext(&mut self) {
        let migrated = {
            let Some(crypto) = self.crypto.as_ref() else {
                return;
            };
            let paths: Vec<PathBuf> = self
                .vault
                .as_ref()
                .map(|v| v.notes.iter().map(|n| n.path.clone()).collect())
                .unwrap_or_default();
            let mut count = 0usize;
            for path in paths {
                if let Ok(data) = Vault::read_bytes(&path) {
                    if !crypto::is_encrypted(&data) {
                        if let Ok(text) = String::from_utf8(data) {
                            let blob = crypto.encrypt(&text);
                            if Vault::write_bytes(&path, &blob).is_ok() {
                                count += 1;
                            }
                        }
                    }
                }
            }
            count
        };
        if migrated > 0 {
            self.status = format!("Encrypted {migrated} pre-existing note(s)");
        }
    }

    fn open_vault(&mut self, path: PathBuf) {
        if self.crypto.is_none() {
            self.status = "Unlock first".into();
            return;
        }
        match Vault::open(path.clone()) {
            Ok(vault) => {
                self.vault = Some(vault);
                self.rebuild_tree();
                self.tabs.clear();
                self.active = 0;
                self.last_active = None;
                self.backlinks.clear();
                self.config.remember_vault(&path);
                self.config.save();
                self.status = "Vault opened".to_string();
                // Encrypt any pre-existing plaintext notes in this vault.
                self.migrate_plaintext();
                // Restore previously-open tabs that belong to this vault.
                let root = self
                    .vault
                    .as_ref()
                    .map(|v| v.root.clone())
                    .unwrap_or_default();
                let saved = self.config.open_tabs.clone();
                let active_saved = self.config.active_tab;
                for p in saved {
                    if p.starts_with(&root) && p.is_file() {
                        self.open_note(p);
                    }
                }
                if !self.tabs.is_empty() {
                    self.active = active_saved.min(self.tabs.len() - 1);
                }
            }
            Err(e) => self.status = format!("Failed to open vault: {e}"),
        }
    }

    fn rebuild_tree(&mut self) {
        self.tree = self.vault.as_ref().map(|v| v.tree()).unwrap_or_default();
        // Expand all folders by default so the tree starts fully visible.
        let mut rels = HashSet::new();
        collect_dir_rels(&self.tree, &mut rels);
        self.expanded_dirs = rels;
        self.rebuild_index();
    }

    /// Rebuild the title/alias/tag/link indexes by reading (and decrypting)
    /// every note once.
    fn rebuild_index(&mut self) {
        let paths: Vec<(PathBuf, String)> = self
            .vault
            .as_ref()
            .map(|v| {
                v.notes
                    .iter()
                    .map(|n| (n.path.clone(), n.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut metas = BTreeMap::new();
        let mut titles = BTreeMap::new();
        let mut entries = Vec::new();
        for (path, stem) in paths {
            let text = self.read_note(&path).unwrap_or_default();
            let meta = frontmatter::parse(&text);
            let title = meta.title.clone().unwrap_or_else(|| stem.clone());
            let links = markdown::wikilinks(&meta.body);
            entries.push(index::Entry {
                path: path.clone(),
                title: title.clone(),
                aliases: meta.aliases.clone(),
                tags: meta.tags.clone(),
                links,
            });
            titles.insert(path.clone(), title);
            metas.insert(path, meta);
        }
        self.index = NoteIndex::build(entries);
        self.titles = titles;
        self.metas = metas;
        self.rebuild_graph();
    }

    /// Recompute the graph node/edge layout from the current index.
    fn rebuild_graph(&mut self) {
        let nodes: Vec<PathBuf> = self
            .vault
            .as_ref()
            .map(|v| v.notes.iter().map(|n| n.path.clone()).collect())
            .unwrap_or_default();
        let pos_of: BTreeMap<&Path, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, p)| (p.as_path(), i))
            .collect();
        let mut edges = Vec::new();
        for (a, b) in self.index.edges() {
            if let (Some(&ia), Some(&ib)) = (pos_of.get(a.as_path()), pos_of.get(b.as_path())) {
                if ia != ib {
                    edges.push((ia.min(ib), ia.max(ib)));
                }
            }
        }
        self.graph_pos = graph::layout(nodes.len(), &edges, 120);
        self.graph_nodes = nodes;
        self.graph_edges = edges;
    }

    /// The display title for a note (frontmatter title if present, else stem).
    fn title_of(&self, path: &Path) -> String {
        self.titles.get(path).cloned().unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        })
    }

    fn refresh_vault(&mut self) {
        self.media_textures.clear();
        if let Some(vault) = self.vault.as_mut() {
            if let Err(e) = vault.rescan() {
                self.status = format!("Refresh failed: {e}");
            }
        }
        self.rebuild_tree();
    }

    fn open_note(&mut self, path: PathBuf) {
        if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
            self.active = idx;
            return;
        }
        match self.read_note(&path) {
            Some(text) => {
                let title = self.title_of(&path);
                self.tabs.push(Tab {
                    path,
                    title,
                    saved: text.clone(),
                    text,
                    view: if self.config.show_preview {
                        ViewMode::Split
                    } else {
                        ViewMode::Edit
                    },
                    undo: Vec::new(),
                    redo: Vec::new(),
                });
                self.active = self.tabs.len() - 1;
            }
            None => {
                self.status = "Could not open note — wrong password or corrupted file".into();
            }
        }
    }

    fn save_active(&mut self) {
        let idx = self.active;
        if idx >= self.tabs.len() || !self.tabs[idx].dirty() {
            return;
        }
        let path = self.tabs[idx].path.clone();
        let text = self.tabs[idx].text.clone();
        let title = self.tabs[idx].title.clone();
        match self.write_note(&path, &text) {
            Ok(()) => {
                self.tabs[idx].saved = text;
                self.status = format!("Saved · {title}");
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn save_all(&mut self) {
        for i in 0..self.tabs.len() {
            if self.tabs[i].dirty() {
                let path = self.tabs[i].path.clone();
                let text = self.tabs[i].text.clone();
                if self.write_note(&path, &text).is_ok() {
                    self.tabs[i].saved = text;
                }
            }
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        // Save on close.
        let tab = &self.tabs[idx];
        if tab.dirty() {
            let path = tab.path.clone();
            let text = tab.text.clone();
            let _ = self.write_note(&path, &text);
        }
        self.tabs.remove(idx);
        if self.active >= self.tabs.len() && !self.tabs.is_empty() {
            self.active = self.tabs.len() - 1;
        }
    }

    fn create_note(&mut self, name: &str) -> bool {
        if name.trim().is_empty() {
            self.status = "Enter a note name".into();
            return false;
        }
        let Some(crypto) = self.crypto.as_ref() else {
            self.status = "Unlock first".into();
            return false;
        };
        let stem = Vault::sanitize_name(name);
        let contents = crypto.encrypt(&format!("# {stem}\n\n"));
        let created = self
            .vault
            .as_ref()
            .and_then(|v| v.create_note(name, &contents).ok());
        match created {
            Some(path) => {
                self.refresh_vault();
                self.open_note(path);
                self.status = format!("Created · {name}");
                true
            }
            None => {
                self.status = if self.vault.is_none() {
                    "Open a vault first".into()
                } else {
                    "Create failed".into()
                };
                false
            }
        }
    }

    /// Create a sub-note nested under `parent`, as `Parent/Child.md` (Obsidian's
    /// note-plus-folder convention).
    fn create_subnote(&mut self, parent: &Path, name: &str) -> bool {
        if name.trim().is_empty() {
            self.status = "Enter a sub-note name".into();
            return false;
        }
        let Some(crypto) = self.crypto.as_ref() else {
            self.status = "Unlock first".into();
            return false;
        };
        let stem = Vault::sanitize_name(name);
        // The child folder sits next to the parent note, named after it.
        let folder = parent.with_extension("");
        let mut path = folder.join(format!("{stem}.md"));
        let mut counter = 1;
        while path.exists() {
            path = folder.join(format!("{stem} {counter}.md"));
            counter += 1;
        }
        let blob = crypto.encrypt(&format!("# {stem}\n\n"));
        if let Err(e) = Vault::write_bytes(&path, &blob) {
            self.status = format!("Create sub-note failed: {e}");
            return false;
        }
        self.refresh_vault();
        self.open_note(path);
        self.status = format!("Created sub-note · {stem}");
        true
    }

    fn rename_note(&mut self, old: PathBuf, new_name: &str) {
        let stem = Vault::sanitize_name(new_name);
        let new_path = old.with_file_name(format!("{stem}.md"));
        if new_path == old {
            return;
        }
        if new_path.exists() {
            self.status = format!("A note named '{stem}' already exists");
            return;
        }
        match fs::rename(&old, &new_path) {
            Ok(()) => {
                for tab in &mut self.tabs {
                    if tab.path == old {
                        tab.path = new_path.clone();
                        tab.title = stem.clone();
                    }
                }
                self.refresh_vault();
                self.last_active = None;
                self.status = format!("Renamed to {stem}");
            }
            Err(e) => self.status = format!("Rename failed: {e}"),
        }
    }

    fn delete_note(&mut self, path: PathBuf) {
        match fs::remove_file(&path) {
            Ok(()) => {
                self.tabs.retain(|t| t.path != path);
                if self.active >= self.tabs.len() {
                    self.active = self.tabs.len().saturating_sub(1);
                }
                self.refresh_vault();
                self.last_active = None;
                self.status = "Note deleted".into();
            }
            Err(e) => self.status = format!("Delete failed: {e}"),
        }
    }

    fn recompute_backlinks(&mut self) {
        self.backlinks.clear();
        self.unlinked.clear();
        let Some(active_path) = self.tabs.get(self.active).map(|t| t.path.clone()) else {
            return;
        };
        // Match against the note's title and all of its aliases.
        let title = self.title_of(&active_path);
        let aliases = self
            .metas
            .get(&active_path)
            .map(|m| m.aliases.clone())
            .unwrap_or_default();
        let mut terms: Vec<String> = vec![title];
        terms.extend(aliases);
        terms.retain(|t| !t.trim().is_empty());
        if terms.is_empty() {
            return;
        }
        let needles: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();
        let paths: Vec<PathBuf> = self
            .vault
            .as_ref()
            .map(|v| v.notes.iter().map(|n| n.path.clone()).collect())
            .unwrap_or_default();

        let mut linked: Vec<(PathBuf, usize, String)> = Vec::new();
        let mut unlinked: Vec<(PathBuf, usize, String)> = Vec::new();
        for path in paths {
            if path == active_path {
                continue;
            }
            let Some(content) = self.read_note(&path) else {
                continue;
            };
            // Linked: some line has a [[wikilink]] to this note.
            let mut linked_here = false;
            for (i, line) in content.lines().enumerate() {
                let hit = markdown::wikilinks(line).into_iter().any(|l| {
                    let base = l
                        .split('#')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase();
                    needles.iter().any(|n| n == &base)
                });
                if hit {
                    linked.push((path.clone(), i + 1, line.trim().to_string()));
                    linked_here = true;
                    break;
                }
            }
            if linked_here {
                continue;
            }
            // Unlinked mention: the title/alias appears as plain text.
            for (i, line) in content.lines().enumerate() {
                let ll = line.to_ascii_lowercase();
                let mention = needles
                    .iter()
                    .any(|t| ll.contains(t) && !ll.contains(&format!("[[{t}")));
                if mention {
                    let snippet: String = line.trim().chars().take(120).collect();
                    unlinked.push((path.clone(), i + 1, snippet));
                    break;
                }
            }
        }
        linked.sort_by(|a, b| a.0.cmp(&b.0));
        unlinked.sort_by(|a, b| a.0.cmp(&b.0));
        self.backlinks = linked;
        self.unlinked = unlinked;
    }

    fn resolve_link(&self, target: &str) -> Option<PathBuf> {
        self.index.resolve(target)
    }

    // ------------------------------------------------------------- palette

    fn open_palette(&mut self) {
        self.palette = Some(PaletteState {
            query: String::new(),
            selected: 0,
            focus: true,
        });
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        if self.palette.is_none() {
            return;
        }
        let query = self
            .palette
            .as_ref()
            .map(|p| p.query.clone())
            .unwrap_or_default();

        // Commands first, then every note (titled by frontmatter when present).
        let mut items: Vec<(PaletteItem, String)> = Command::ALL
            .iter()
            .map(|c| (PaletteItem::Command(*c), c.label().to_string()))
            .collect();
        let titles = self.titles.clone();
        let notes: Vec<(PathBuf, String)> = self
            .vault
            .as_ref()
            .map(|v| {
                v.notes
                    .iter()
                    .map(|n| {
                        let t = titles
                            .get(&n.path)
                            .cloned()
                            .unwrap_or_else(|| n.title.clone());
                        (n.path.clone(), t)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (p, t) in notes {
            items.push((PaletteItem::Note(p), t));
        }

        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, (_, text))| palette::score(&query, text).map(|s| (s, i)))
            .collect();
        scored.sort_by_key(|x| std::cmp::Reverse(x.0));
        let results: Vec<usize> = scored.into_iter().take(12).map(|(_, i)| i).collect();

        let mut run: Option<PaletteItem> = None;
        let mut close = false;
        let mut move_sel = 0i32;

        egui::Window::new("Command palette")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 120.0))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                let state = self.palette.as_mut().unwrap();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text("Type a note or command…")
                        .desired_width(f32::INFINITY),
                );
                if state.focus {
                    resp.request_focus();
                    state.focus = false;
                }
                ui.separator();
                if results.is_empty() {
                    ui.weak("No matches");
                }
                let selected = state.selected;
                for (n, &idx) in results.iter().enumerate() {
                    let (item, label) = &items[idx];
                    let sel = n == selected;
                    let icon = match item {
                        PaletteItem::Note(_) => Icon::File,
                        PaletteItem::Command(c) => c.icon(),
                    };
                    let clicked = ui
                        .horizontal(|ui| {
                            icons::icon(ui, icon, 14.0, ui.visuals().weak_text_color());
                            ui.selectable_label(sel, label).clicked()
                        })
                        .inner;
                    if clicked {
                        run = Some(item.clone());
                        close = true;
                    }
                }
                ui.input(|i| {
                    if i.key_pressed(egui::Key::ArrowDown) {
                        move_sel = 1;
                    } else if i.key_pressed(egui::Key::ArrowUp) {
                        move_sel = -1;
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !results.is_empty() {
                    let pick = selected.min(results.len() - 1);
                    run = Some(items[results[pick]].0.clone());
                    close = true;
                }
            });

        if move_sel != 0 {
            if let Some(state) = self.palette.as_mut() {
                let len = results.len().max(1) as i32;
                let ns = (state.selected as i32 + move_sel).clamp(0, len - 1);
                state.selected = ns as usize;
            }
        }
        if let Some(item) = run {
            self.run_palette_item(item, ctx);
        }
        if close {
            self.palette = None;
        } else if let Some(state) = self.palette.as_mut() {
            if !results.is_empty() && state.selected >= results.len() {
                state.selected = results.len() - 1;
            }
        }
    }

    fn run_palette_item(&mut self, item: PaletteItem, ctx: &egui::Context) {
        match item {
            PaletteItem::Note(p) => self.pending_open = Some(p),
            PaletteItem::Command(c) => self.run_command(c, ctx),
        }
    }

    fn run_command(&mut self, c: Command, ctx: &egui::Context) {
        match c {
            Command::NewNote => {
                self.new_note_open = true;
                self.new_note_error = None;
                self.new_note_name.clear();
            }
            Command::NewSubnote => {
                if let Some(tab) = self.tabs.get(self.active) {
                    self.subnote_parent = Some(tab.path.clone());
                    self.subnote_name.clear();
                    self.subnote_open = true;
                    self.subnote_error = None;
                } else {
                    self.status = "Open a note first".into();
                }
            }
            Command::OpenVault => self.open_picker(),
            Command::DailyNote => self.open_daily_note(),
            Command::DailyPrev => self.open_daily_note_offset(-1),
            Command::DailyNext => self.open_daily_note_offset(1),
            Command::DailySettings => self.daily_settings_open = true,
            Command::NewFromTemplate => {
                let dir = self.config.templates_dir.clone();
                let templates = self
                    .vault
                    .as_ref()
                    .map(|v| v.template_paths(&dir))
                    .unwrap_or_default();
                if templates.is_empty() {
                    self.status = format!("No templates in '{dir}/'");
                } else if templates.len() == 1 {
                    self.new_from_template(templates[0].clone());
                } else {
                    self.template_pick = Some(templates);
                }
            }
            Command::NewCanvas => {
                self.new_canvas_open = true;
                self.new_canvas_name.clear();
            }
            Command::OpenCanvas => self.open_canvas(),
            Command::OpenFeeds => self.feeds_open = true,
            Command::OpenPasswords => self.open_passwords(),
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::WindowMode => {
                self.window_mode = self.window_mode.next();
                self.apply_window_mode(ctx);
            }
            Command::ToggleVim => {
                self.config.vim = !self.config.vim;
                if !self.config.vim {
                    self.vim = crate::vim::Vim::new();
                }
                self.config.save();
                self.status = if self.config.vim {
                    "Vim mode on".into()
                } else {
                    "Vim mode off".into()
                };
            }
            Command::TogglePreview => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.view = match tab.view {
                        ViewMode::Edit => ViewMode::Split,
                        ViewMode::Split => ViewMode::Preview,
                        ViewMode::Preview => ViewMode::Live,
                        ViewMode::Live => ViewMode::Edit,
                    };
                }
            }
            Command::TogglePanel => self.show_right = !self.show_right,
            Command::ToggleGraph => self.show_graph = !self.show_graph,
            Command::Lock => self.lock(),
            Command::CloseTab => {
                if !self.tabs.is_empty() {
                    self.close_tab(self.active);
                }
            }
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// Open (creating if needed) today's daily note.
    fn open_daily_note(&mut self) {
        self.open_daily_note_offset(0);
    }

    /// Open the daily note `offset` days from today (Obsidian's prev/next).
    fn open_daily_note_offset(&mut self, offset: i64) {
        let (ty, tm, td) = crate::dates::today();
        let (y, m, d) = crate::dates::add_days(ty, tm, td, offset);
        self.open_daily_note_for(y, m, d);
    }

    /// Open (creating if needed) the daily note for an explicit date.
    fn open_daily_note_for(&mut self, y: i32, m: u32, d: u32) {
        let Some(vault) = self.vault.as_ref() else {
            self.status = "Open a vault first".into();
            return;
        };
        let dir = self.config.daily_dir.clone();
        let fmt = self.config.daily_format.clone();
        let path = vault.daily_note_path(&dir, &fmt, y, m, d);
        if !path.exists() {
            let title = crate::dates::format(&fmt, y, m, d);
            let body = self.daily_note_body(&title, (y, m, d));
            if let Some(crypto) = self.crypto.as_ref() {
                let blob = crypto.encrypt(&body);
                let _ = Vault::write_bytes(&path, &blob);
            }
        }
        self.refresh_vault();
        self.open_note(path);
    }

    /// Body for a new daily note: the configured template (with `{{date}}`,
    /// `{{time}}`, `{{title}}` substituted), or a minimal default.
    fn daily_note_body(&self, title: &str, ctx: (i32, u32, u32)) -> String {
        let tpl = self.config.daily_template.trim();
        if !tpl.is_empty() {
            let resolved = self.index.resolve(tpl).or_else(|| {
                self.vault.as_ref().and_then(|v| {
                    v.notes
                        .iter()
                        .find(|n| {
                            n.rel.to_string_lossy() == tpl || n.title.eq_ignore_ascii_case(tpl)
                        })
                        .map(|n| n.path.clone())
                })
            });
            if let Some(p) = resolved {
                if let Some(text) = self.read_note(&p) {
                    let body = frontmatter::parse(&text).body;
                    return crate::dates::expand(&body, title, ctx, &self.config.daily_format);
                }
            }
        }
        format!("---\ntags: [daily]\n---\n\n# {title}\n\n")
    }

    /// Create a new note from a template, substituting `{{date}}`/`{{title}}`.
    fn new_from_template(&mut self, template: PathBuf) {
        let Some(text) = self.read_note(&template) else {
            self.status = "Could not read template".into();
            return;
        };
        let meta = frontmatter::parse(&text);
        let stem = template
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Note".into());
        let (y, m, d) = crate::dates::today();
        let name = format!("{stem} {}", crate::dates::format("YYYY-MM-DD", y, m, d));
        let body = crate::dates::expand(&meta.body, &name, (y, m, d), "YYYY-MM-DD");
        let contents = match self.crypto.as_ref() {
            Some(c) => c.encrypt(&body),
            None => {
                self.status = "Unlock first".into();
                return;
            }
        };
        let created = self
            .vault
            .as_ref()
            .and_then(|v| v.create_note(&name, &contents).ok());
        match created {
            Some(path) => {
                self.refresh_vault();
                self.open_note(path);
                self.status = format!("Created from template · {stem}");
            }
            None => self.status = "Create from template failed".into(),
        }
    }

    /// Save the open tabs / panel state so it can be restored next launch.
    fn save_workspace(&mut self) {
        self.config.open_tabs = self.tabs.iter().map(|t| t.path.clone()).collect();
        self.config.active_tab = self.active;
        self.config.show_right = self.show_right;
    }

    // -------------------------------------------------------------- canvas

    /// Find and open the first `.canvas` board in the vault.
    fn open_canvas(&mut self) {
        let Some(root) = self.vault.as_ref().map(|v| v.root.clone()) else {
            self.status = "Open a vault first".into();
            return;
        };
        let mut found = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()) == Some("canvas") {
                    found.push(p);
                }
            }
        }
        found.sort();
        match found.len() {
            0 => self.status = "No canvases yet — use 'New canvas'".into(),
            1 => self.open_canvas_path(found[0].clone()),
            _ => self.canvas_pick = Some(found),
        }
    }

    /// Open a specific `.canvas` file.
    fn open_canvas_path(&mut self, path: PathBuf) {
        match self.read_canvas(&path) {
            Some(data) => {
                self.canvas = Some(CanvasView {
                    path,
                    data,
                    offset: egui::Vec2::ZERO,
                    zoom: 1.0,
                    fit: true,
                    sel: std::collections::HashSet::new(),
                    editing: None,
                    edge_from: None,
                    marquee: None,
                    undo: Vec::new(),
                    redo: Vec::new(),
                    edit_undo: None,
                });
                self.show_graph = false;
                self.status = "Opened canvas".into();
            }
            None => self.status = "Could not read canvas".into(),
        }
    }

    /// Create a new empty `.canvas` in the vault and open it.
    fn create_canvas(&mut self, name: &str) {
        let Some(root) = self.vault.as_ref().map(|v| v.root.clone()) else {
            self.status = "Open a vault first".into();
            return;
        };
        let stem = Vault::sanitize_name(name);
        let mut path = root.join(format!("{stem}.canvas"));
        let mut counter = 1;
        while path.exists() {
            path = root.join(format!("{stem} {counter}.canvas"));
            counter += 1;
        }
        let data = canvas::Canvas::default();
        match self.write_canvas(&path, &data) {
            Ok(()) => {
                self.open_canvas_path(path);
                self.status = format!("Created canvas · {stem}");
            }
            Err(e) => self.status = format!("Create canvas failed: {e}"),
        }
    }

    /// Read a canvas (decrypting if it's one of ours, else parsing plain JSON).
    fn read_canvas(&self, path: &Path) -> Option<canvas::Canvas> {
        let bytes = Vault::read_bytes(path).ok()?;
        let json = if crypto::is_encrypted(&bytes) {
            self.crypto.as_ref()?.decrypt(&bytes)?
        } else {
            String::from_utf8(bytes).ok()?
        };
        serde_json::from_str(&json).ok()
    }

    fn write_canvas(&self, path: &Path, data: &canvas::Canvas) -> std::io::Result<()> {
        let crypto = self.crypto.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "vault is locked")
        })?;
        let json = serde_json::to_string_pretty(data)?;
        Vault::write_bytes(path, &crypto.encrypt(&json))
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let Some(mut view) = self.canvas.take() else {
            return;
        };
        let mut close = false;
        let mut save = false;
        let mut add_text = false;
        let mut add_group = false;
        let mut add_file = false;
        let mut del = false;
        let mut undo_click = false;
        let mut redo_click = false;
        let title = view
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        ui.horizontal(|ui| {
            icons::icon(ui, Icon::Graph, 16.0, theme::ACCENT);
            ui.label(RichText::new(format!("Canvas · {title}")).strong());
            ui.separator();
            if icons::button(ui, Icon::Add, 16.0, true, false, "Add text card").clicked() {
                add_text = true;
            }
            if icons::button(ui, Icon::Folder, 16.0, true, false, "Add group").clicked() {
                add_group = true;
            }
            if icons::button(
                ui,
                Icon::File,
                16.0,
                true,
                false,
                "Add file card (first note)",
            )
            .clicked()
            {
                add_file = true;
            }
            if icons::button(ui, Icon::Save, 16.0, true, false, "Save canvas").clicked() {
                save = true;
            }
            if icons::button(
                ui,
                Icon::Trash,
                16.0,
                !view.sel.is_empty(),
                false,
                "Delete selected",
            )
            .clicked()
            {
                del = true;
            }
            if icons::button(
                ui,
                Icon::Undo,
                16.0,
                !view.undo.is_empty(),
                false,
                "Undo (Ctrl+Z)",
            )
            .clicked()
            {
                undo_click = true;
            }
            if icons::button(
                ui,
                Icon::Redo,
                16.0,
                !view.redo.is_empty(),
                false,
                "Redo (Ctrl+Shift+Z)",
            )
            .clicked()
            {
                redo_click = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Close, 16.0, true, false, "Close canvas").clicked() {
                    close = true;
                }
                ui.weak(format!(
                    "{} cards · {} edges · {} sel",
                    view.data.nodes.len(),
                    view.data.edges.len(),
                    view.sel.len()
                ));
            });
        });

        if !view.sel.is_empty() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Color:").small().weak());
                for code in CANVAS_COLORS {
                    let col = canvas_color(code).unwrap();
                    if ui.add(egui::Button::new("  ").fill(col)).clicked() {
                        view.snapshot();
                        for id in view.sel.iter() {
                            if let Some(n) = view.data.node_mut(id) {
                                n.color = Some(code.to_string());
                            }
                        }
                    }
                }
                if ui.small_button("none").clicked() {
                    view.snapshot();
                    for id in view.sel.iter() {
                        if let Some(n) = view.data.node_mut(id) {
                            n.color = None;
                        }
                    }
                }
                ui.label(
                    RichText::new("· double-click edits · drag ◉ to connect")
                        .small()
                        .weak(),
                );
            });
        }
        ui.separator();

        let rect = ui.available_rect_before_wrap();
        let bg = ui.interact(
            rect,
            ui.id().with("canvas-area"),
            egui::Sense::click_and_drag(),
        );
        if view.fit && !view.data.nodes.is_empty() {
            let (minx, miny, maxx, maxy) = view.data.bounds();
            let cw = (maxx - minx).max(1.0);
            let ch = (maxy - miny).max(1.0);
            view.zoom = (rect.width() * 0.8 / cw)
                .min(rect.height() * 0.8 / ch)
                .clamp(0.2, 1.5);
            let c = egui::vec2((minx + maxx) / 2.0, (miny + maxy) / 2.0);
            view.offset = rect.center().to_vec2() - c * view.zoom;
            view.fit = false;
        }

        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let m = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
            let before = (m - rect.min - view.offset) / view.zoom;
            view.zoom = (view.zoom * (scroll * 0.001).exp()).clamp(0.2, 3.0);
            view.offset = m - rect.min - before * view.zoom;
        }
        // Pan with middle or right drag.
        if bg.dragged_by(egui::PointerButton::Middle)
            || bg.dragged_by(egui::PointerButton::Secondary)
        {
            view.offset += bg.drag_delta();
        }

        let offset = view.offset;
        let zoom = view.zoom;
        let to_screen = |p: egui::Vec2| rect.min + offset + p * zoom;
        let to_world = |p: egui::Pos2| (p - rect.min - offset) / zoom;
        let node_rect = |n: &canvas::Node| {
            egui::Rect::from_min_size(
                to_screen(egui::vec2(n.x, n.y)),
                egui::vec2(n.width, n.height) * zoom,
            )
        };

        // Background context menu.
        let mut bg_add: Option<&'static str> = None;
        let menu_pos = bg.interact_pointer_pos();
        bg.context_menu(|ui| {
            if ui.button("New text card").clicked() {
                bg_add = Some("text");
                ui.close_menu();
            }
            if ui.button("New group").clicked() {
                bg_add = Some("group");
                ui.close_menu();
            }
            if ui.button("New link card").clicked() {
                bg_add = Some("link");
                ui.close_menu();
            }
        });
        if let (Some(kind), Some(pos)) = (bg_add, menu_pos) {
            view.snapshot();
            let w = to_world(pos);
            let id = match kind {
                "group" => view.data.add_group(w.x - 160.0, w.y - 110.0, 320.0, 220.0),
                "link" => view
                    .data
                    .add_link_node("https://example.com", w.x - 120.0, w.y - 40.0),
                _ => view.data.add_text_node(w.x - 120.0, w.y - 60.0),
            };
            view.sel.clear();
            view.sel.insert(id);
        }

        // Rubber-band selection: left-drag on empty space.
        let press = ui.input(|i| i.pointer.press_origin());
        let over_node = press
            .map(|p| view.data.nodes.iter().any(|n| node_rect(n).contains(p)))
            .unwrap_or(false);
        if bg.drag_started() && !over_node {
            if let Some(p) = press {
                let w = to_world(p);
                view.marquee = Some((w, w));
                view.sel.clear();
            }
        }
        if view.marquee.is_some() {
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                if let Some(m) = view.marquee.as_mut() {
                    m.1 = to_world(p);
                }
            }
        }
        if bg.drag_stopped() {
            if let Some((a, b)) = view.marquee.take() {
                let mr = egui::Rect::from_two_pos(
                    egui::pos2(a.x.min(b.x), a.y.min(b.y)),
                    egui::pos2(a.x.max(b.x), a.y.max(b.y)),
                );
                let sel: std::collections::HashSet<String> = view
                    .data
                    .nodes
                    .iter()
                    .filter(|n| {
                        mr.intersects(egui::Rect::from_min_size(
                            egui::pos2(n.x, n.y),
                            egui::vec2(n.width, n.height),
                        ))
                    })
                    .map(|n| n.id.clone())
                    .collect();
                view.sel = sel;
            }
        }

        // Edges.
        {
            let painter = ui.painter();
            for e in &view.data.edges {
                let (Some(a), Some(b)) = (view.data.node(&e.from_node), view.data.node(&e.to_node))
                else {
                    continue;
                };
                let pa = to_screen(egui::vec2(a.x + a.width / 2.0, a.y + a.height / 2.0));
                let pb = to_screen(egui::vec2(b.x + b.width / 2.0, b.y + b.height / 2.0));
                painter.line_segment(
                    [pa, pb],
                    egui::Stroke::new(1.6_f32, theme::ACCENT.linear_multiply(0.7)),
                );
                let d = (pb - pa).normalized();
                let perp = egui::vec2(-d.y, d.x);
                painter.add(egui::Shape::convex_polygon(
                    vec![pb, pb - d * 10.0 + perp * 5.0, pb - d * 10.0 - perp * 5.0],
                    theme::ACCENT,
                    egui::Stroke::NONE,
                ));
            }
            // Connector preview.
            if let (Some(from), Some(ptr)) =
                (view.edge_from.clone(), ui.input(|i| i.pointer.hover_pos()))
            {
                if let Some(n) = view.data.node(&from) {
                    let pa = to_screen(egui::vec2(n.x + n.width / 2.0, n.y + n.height / 2.0));
                    painter.line_segment([pa, ptr], egui::Stroke::new(1.5_f32, theme::ACCENT));
                }
            }
        }

        // Nodes.
        let shift = ui.input(|i| i.modifiers.shift);
        let ids: Vec<String> = view.data.nodes.iter().map(|n| n.id.clone()).collect();
        let mut clicked: Option<String> = None;
        let mut dbl: Option<String> = None;
        let mut ctx_del: Option<String> = None;
        let mut ctx_edit: Option<String> = None;
        let mut ctx_color: Option<(String, Option<String>)> = None;
        let mut move_delta: Option<egui::Vec2> = None;
        let mut resize: Option<(String, egui::Vec2)> = None;
        let mut dragged_now = false;
        let mut resized_now = false;
        for id in &ids {
            let Some(node) = view.data.node(id) else {
                continue;
            };
            let nrect = node_rect(node);
            let resp = ui.interact(
                nrect,
                ui.id().with(("cnode", id.clone())),
                egui::Sense::click_and_drag(),
            );
            let selected = view.sel.contains(id);
            let color = node.color.as_deref().and_then(canvas_color);
            let (fill, stroke) = if node.kind == "group" {
                let c = color.unwrap_or(theme::ACCENT);
                (
                    c.linear_multiply(0.08),
                    egui::Stroke::new(1.5_f32, c.linear_multiply(0.5)),
                )
            } else {
                let st = if selected {
                    theme::ACCENT
                } else {
                    color.unwrap_or(theme::ACCENT).linear_multiply(0.5)
                };
                (
                    color.unwrap_or(theme::PANEL),
                    egui::Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, st),
                )
            };
            ui.painter()
                .rect(nrect, egui::Rounding::same(6.0), fill, stroke);
            let label = match node.kind.as_str() {
                "file" => node.file.clone().unwrap_or_default(),
                _ => node.text.clone(),
            };
            ui.painter().text(
                nrect.shrink(6.0).min,
                egui::Align2::LEFT_TOP,
                ellipsize(&label, 400),
                egui::FontId::proportional(13.0 * zoom.clamp(0.6, 1.6)),
                theme::TEXT,
            );
            if resp.clicked() {
                clicked = Some(id.clone());
            }
            if resp.double_clicked() {
                dbl = Some(id.clone());
            }
            if resp.dragged() {
                move_delta = Some(resp.drag_delta());
            }
            if resp.drag_started() {
                dragged_now = true;
            }
            resp.context_menu(|ui| {
                if ui.button("Edit").clicked() {
                    ctx_edit = Some(id.clone());
                    ui.close_menu();
                }
                ui.menu_button("Color", |ui| {
                    for code in CANVAS_COLORS {
                        if ui.button(format!("Color {code}")).clicked() {
                            ctx_color = Some((id.clone(), Some(code.to_string())));
                            ui.close_menu();
                        }
                    }
                    if ui.button("None").clicked() {
                        ctx_color = Some((id.clone(), None));
                        ui.close_menu();
                    }
                });
                if ui.button("Delete").clicked() {
                    ctx_del = Some(id.clone());
                    ui.close_menu();
                }
            });
            if selected && view.sel.len() == 1 {
                let hr = egui::Rect::from_min_size(
                    nrect.max - egui::vec2(12.0, 12.0),
                    egui::vec2(12.0, 12.0),
                );
                let h = ui.interact(
                    hr,
                    ui.id().with(("cresize", id.clone())),
                    egui::Sense::drag(),
                );
                ui.painter().rect_filled(hr, 2.0, theme::ACCENT);
                if h.dragged() {
                    resize = Some((id.clone(), h.drag_delta() / zoom));
                }
                if h.drag_started() {
                    resized_now = true;
                }
            }
            // Connector handle (right-middle).
            let cr = egui::Rect::from_center_size(
                egui::pos2(nrect.max.x, nrect.center().y),
                egui::vec2(10.0, 10.0),
            );
            let ch = ui.interact(cr, ui.id().with(("cconn", id.clone())), egui::Sense::drag());
            ui.painter().circle_filled(cr.center(), 4.0, theme::ACCENT);
            if ch.drag_started() {
                view.edge_from = Some(id.clone());
            }
        }

        // Apply node interactions.
        if dragged_now || resized_now {
            view.snapshot();
        }
        if let Some(id) = clicked {
            if shift {
                if !view.sel.remove(&id) {
                    view.sel.insert(id);
                }
            } else {
                view.sel.clear();
                view.sel.insert(id);
            }
        }
        if let Some(delta) = move_delta {
            for id in view.sel.clone() {
                if let Some(n) = view.data.node_mut(&id) {
                    n.x += delta.x / zoom;
                    n.y += delta.y / zoom;
                }
            }
        }
        if let Some((id, delta)) = resize {
            if let Some(n) = view.data.node_mut(&id) {
                n.width = (n.width + delta.x).max(80.0);
                n.height = (n.height + delta.y).max(48.0);
            }
        }
        if let Some((id, color)) = ctx_color {
            view.snapshot();
            if let Some(n) = view.data.node_mut(&id) {
                n.color = color;
            }
        }
        if let Some(id) = ctx_del {
            view.snapshot();
            view.data.remove_node(&id);
            view.sel.remove(&id);
        }
        if let Some(id) = ctx_edit {
            view.edit_undo = Some((
                view.data.clone(),
                view.data
                    .node(&id)
                    .map(|n| n.text.clone())
                    .unwrap_or_default(),
            ));
            view.editing = Some(id);
        }
        if let Some(id) = dbl {
            let kind = view
                .data
                .node(&id)
                .map(|n| n.kind.clone())
                .unwrap_or_default();
            if kind == "file" || kind == "link" {
                if let Some(n) = view.data.node(&id) {
                    let target = n.file.clone().unwrap_or_else(|| n.text.clone());
                    let stem = Path::new(&target)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| target.clone());
                    if kind == "link" {
                        open_external(&target);
                    } else if let Some(p) = self.index.resolve(&stem) {
                        self.pending_open = Some(p);
                    } else {
                        self.status = format!("No note '{target}'");
                    }
                }
            } else {
                view.edit_undo = Some((
                    view.data.clone(),
                    view.data
                        .node(&id)
                        .map(|n| n.text.clone())
                        .unwrap_or_default(),
                ));
                view.editing = Some(id);
            }
        }

        // Finish a connector drag.
        if view.edge_from.is_some() && ui.input(|i| i.pointer.any_released()) {
            let from = view.edge_from.take().unwrap();
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                let target = view
                    .data
                    .nodes
                    .iter()
                    .find(|n| n.id != from && node_rect(n).contains(p))
                    .map(|n| n.id.clone());
                if let Some(to) = target {
                    if !view.data.connected(&from, &to) {
                        view.snapshot();
                        view.data.connect(&from, &to);
                    }
                }
            }
        }

        // Draw marquee.
        if let Some((a, b)) = view.marquee {
            let sa = to_screen(a);
            let sb = to_screen(b);
            let mr = egui::Rect::from_two_pos(sa, sb);
            ui.painter()
                .rect_filled(mr, 0.0, theme::ACCENT.linear_multiply(0.15));
            ui.painter()
                .rect_stroke(mr, 0.0, egui::Stroke::new(1.0_f32, theme::ACCENT));
        }

        // Inline text editor for the node being edited.
        if let Some(id) = view.editing.clone() {
            if let Some(node) = view.data.node(&id) {
                let nr = node_rect(node);
                let mut txt = node.text.clone();
                let te = ui.put(
                    nr.shrink(4.0),
                    egui::TextEdit::multiline(&mut txt)
                        .desired_width(nr.width() - 12.0)
                        .desired_rows(3),
                );
                if te.changed() {
                    if let Some(n) = view.data.node_mut(&id) {
                        n.text = txt;
                    }
                }
                let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                if esc || (te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    // Record the edit for undo only if the text actually changed.
                    if let Some((snap, orig)) = view.edit_undo.take() {
                        let now = view
                            .data
                            .node(&id)
                            .map(|n| n.text.clone())
                            .unwrap_or_default();
                        if now != orig {
                            view.undo.push(snap);
                            if view.undo.len() > 100 {
                                view.undo.remove(0);
                            }
                            view.redo.clear();
                        }
                    }
                    view.editing = None;
                } else if !te.has_focus() {
                    te.request_focus();
                }
            } else {
                view.editing = None;
            }
        }

        // Turf actions.
        if add_text {
            view.snapshot();
            let c = to_world(rect.center());
            let id = view.data.add_text_node(c.x - 120.0, c.y - 60.0);
            view.sel.clear();
            view.sel.insert(id);
        }
        if add_group {
            view.snapshot();
            let c = to_world(rect.center());
            let id = view.data.add_group(c.x - 150.0, c.y - 100.0, 320.0, 220.0);
            view.sel.clear();
            view.sel.insert(id);
        }
        if add_file {
            let first = self
                .vault
                .as_ref()
                .and_then(|v| v.notes.first().map(|n| n.rel.to_string_lossy().to_string()));
            if let Some(f) = first {
                view.snapshot();
                let c = to_world(rect.center());
                let id = view.data.add_file_node(&f, c.x - 120.0, c.y - 40.0);
                view.sel.clear();
                view.sel.insert(id);
            } else {
                self.status = "No notes to add".into();
            }
        }
        // Keyboard: delete selection, set colors.
        if !view.editing.is_some() {
            if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
                && !view.sel.is_empty()
            {
                view.snapshot();
                for id in view.sel.clone() {
                    view.data.remove_node(&id);
                }
                view.sel.clear();
            }
            for code in CANVAS_COLORS {
                let key = match code {
                    "1" => egui::Key::Num1,
                    "2" => egui::Key::Num2,
                    "3" => egui::Key::Num3,
                    "4" => egui::Key::Num4,
                    "5" => egui::Key::Num5,
                    _ => egui::Key::Num6,
                };
                if ui.input(|i| i.key_pressed(key)) {
                    if !view.sel.is_empty() {
                        view.snapshot();
                    }
                    for id in view.sel.clone() {
                        if let Some(n) = view.data.node_mut(&id) {
                            n.color = Some(code.to_string());
                        }
                    }
                }
            }
        }
        if del {
            view.snapshot();
            for id in view.sel.clone() {
                view.data.remove_node(&id);
            }
            view.sel.clear();
        }
        if undo_click {
            view.undo();
        }
        if redo_click {
            view.redo();
        }
        if save {
            match self.write_canvas(&view.path, &view.data) {
                Ok(()) => self.status = "Canvas saved".into(),
                Err(e) => self.status = format!("Canvas save failed: {e}"),
            }
        }
        if close {
            // Save before closing so nothing is lost.
            if let Err(e) = self.write_canvas(&view.path, &view.data) {
                self.status = format!("Canvas save failed: {e}");
            }
            self.canvas = None;
        } else {
            self.canvas = Some(view);
        }
    }

    // --------------------------------------------------------------- feeds

    /// Refresh every feed in a background thread.
    fn refresh_feeds(&mut self) {
        if self.feed_refreshing {
            return;
        }
        let urls: Vec<(u32, String)> = self
            .feeds
            .feeds
            .iter()
            .map(|f| (f.id, f.url.clone()))
            .collect();
        if urls.is_empty() {
            self.status = "No feeds yet — add one".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.feed_rx = Some(rx);
        self.feed_refreshing = true;
        std::thread::spawn(move || {
            for (id, url) in urls {
                let res = feeds::fetch(&url);
                if tx.send((id, res)).is_err() {
                    break;
                }
            }
        });
        self.status = "Refreshing feeds…".into();
    }

    fn poll_feeds(&mut self) {
        let mut finished = false;
        if let Some(rx) = &self.feed_rx {
            loop {
                match rx.try_recv() {
                    Ok((id, res)) => {
                        if let Some(f) = self.feeds.find_mut(id) {
                            match res {
                                Ok((title, items)) => {
                                    if f.title.trim().is_empty() && !title.is_empty() {
                                        f.title = title;
                                    }
                                    f.items = items;
                                    f.error.clear();
                                }
                                Err(e) => f.error = e,
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.feed_rx = None;
            self.feed_refreshing = false;
            self.feeds.save();
            self.status = "Feeds updated".into();
        }
    }

    /// Start fetching the full text of an article (in the background).
    fn load_full_article(&mut self, link: &str) {
        if link.is_empty() || self.feed_full_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.feed_full_rx = Some(rx);
        self.feed_full_loading = Some(link.to_string());
        let link = link.to_string();
        std::thread::spawn(move || {
            let res = feeds::fetch_article(&link);
            let _ = tx.send((link, res));
        });
        self.status = "Loading full article…".into();
    }

    fn poll_full_article(&mut self) {
        let mut done = false;
        if let Some(rx) = &self.feed_full_rx {
            match rx.try_recv() {
                Ok((link, res)) => {
                    match res {
                        Ok(text) => {
                            for f in &mut self.feeds.feeds {
                                for it in &mut f.items {
                                    if it.link == link {
                                        it.full = text.clone();
                                    }
                                }
                            }
                            for it in &mut self.feeds.saved {
                                if it.link == link {
                                    it.full = text.clone();
                                }
                            }
                            self.feeds.save();
                            self.status = "Loaded full article".into();
                        }
                        Err(e) => self.status = format!("Could not load article: {e}"),
                    }
                    done = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => done = true,
            }
        }
        if done {
            self.feed_full_rx = None;
            self.feed_full_loading = None;
        }
    }

    fn feeds_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::Rss, 16.0, theme::ACCENT);
            ui.label(RichText::new("RSS feeds").strong());
            ui.separator();
            if icons::button(
                ui,
                Icon::Refresh,
                16.0,
                !self.feed_refreshing,
                self.feed_refreshing,
                "Refresh all feeds",
            )
            .clicked()
            {
                self.refresh_feeds();
            }
            if icons::button(ui, Icon::Add, 16.0, true, false, "Add feed").clicked() {
                self.feed_add_open = true;
                self.feed_url.clear();
                self.feed_title.clear();
                self.feed_category.clear();
            }
            ui.separator();
            icons::icon(ui, Icon::Search, 14.0, ui.visuals().weak_text_color());
            ui.add(
                egui::TextEdit::singleline(&mut self.feed_search)
                    .hint_text("search all articles…")
                    .desired_width(240.0),
            );
            if !self.feed_search.is_empty()
                && icons::button(ui, Icon::Close, 14.0, true, false, "Clear search").clicked()
            {
                self.feed_search.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Close, 16.0, true, false, "Close feeds").clicked() {
                    self.feeds_open = false;
                }
                ui.weak(format!("{} feed(s)", self.feeds.feeds.len()));
            });
        });
        ui.separator();

        egui::SidePanel::left("feeds_list")
            .resizable(true)
            .default_width(260.0)
            .show_inside(ui, |ui| self.feed_list_ui(ui));
        self.feed_articles_ui(ui);
    }

    fn feed_list_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let cats = self.feeds.categories();
        let mut groups: Vec<(String, Vec<u32>)> = Vec::new();
        for c in cats {
            let ids = self
                .feeds
                .feeds
                .iter()
                .filter(|f| f.category == c)
                .map(|f| f.id)
                .collect();
            groups.push((c, ids));
        }
        let uncat: Vec<u32> = self
            .feeds
            .feeds
            .iter()
            .filter(|f| f.category.trim().is_empty())
            .map(|f| f.id)
            .collect();
        if !uncat.is_empty() {
            groups.push((String::new(), uncat));
        }

        let mut select: Option<u32> = None;
        let mut edit: Option<u32> = None;
        let mut del: Option<u32> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Saved articles (pseudo-feed), shown as icon + count.
                let saved_sel = self.feed_sel == Some(0);
                let count = self.feeds.saved.len();
                let saved_color = if saved_sel {
                    theme::ACCENT
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Bookmark, 13.0, saved_color);
                    if ui
                        .selectable_label(saved_sel, RichText::new(count.to_string()).strong())
                        .on_hover_text("Saved articles")
                        .clicked()
                    {
                        select = Some(0);
                    }
                });
                ui.separator();
                if self.feeds.feeds.is_empty() {
                    ui.add_space(6.0);
                    ui.weak("No feeds yet. Use ＋ to add one.");
                }
                for (cat, ids) in groups {
                    let heading = if cat.trim().is_empty() {
                        "Uncategorized".to_string()
                    } else {
                        cat.clone()
                    };
                    egui::CollapsingHeader::new(RichText::new(heading).strong())
                        .default_open(true)
                        .show(ui, |ui| {
                            for id in ids {
                                let Some(f) = self.feeds.find(id) else {
                                    continue;
                                };
                                let title = if f.title.trim().is_empty() {
                                    f.url.clone()
                                } else {
                                    f.title.clone()
                                };
                                let sel = self.feed_sel == Some(id);
                                let row = egui::Rect::from_min_size(
                                    ui.next_widget_position(),
                                    egui::vec2(ui.available_width(), 20.0),
                                );
                                let hovered = ui
                                    .input(|i| i.pointer.hover_pos())
                                    .is_some_and(|p| row.contains(p));
                                ui.horizontal(|ui| {
                                    icons::icon(
                                        ui,
                                        Icon::Rss,
                                        13.0,
                                        if sel {
                                            theme::ACCENT
                                        } else {
                                            ui.visuals().weak_text_color()
                                        },
                                    );
                                    if ui.selectable_label(sel, RichText::new(title)).clicked() {
                                        select = Some(id);
                                    }
                                    if hovered {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if icons::button(
                                                    ui,
                                                    Icon::Trash,
                                                    13.0,
                                                    true,
                                                    false,
                                                    "Delete feed",
                                                )
                                                .clicked()
                                                {
                                                    del = Some(id);
                                                }
                                                if icons::button(
                                                    ui,
                                                    Icon::Edit,
                                                    13.0,
                                                    true,
                                                    false,
                                                    "Edit feed",
                                                )
                                                .clicked()
                                                {
                                                    edit = Some(id);
                                                }
                                            },
                                        );
                                    }
                                });
                            }
                        });
                }
            });

        if let Some(id) = select {
            self.feed_sel = Some(id);
            self.feed_article = None;
        }
        if let Some(id) = edit {
            if let Some(f) = self.feeds.find(id) {
                self.feed_edit_id = Some(id);
                self.feed_url = f.url.clone();
                self.feed_title = f.title.clone();
                self.feed_category = f.category.clone();
            }
        }
        if let Some(id) = del {
            self.feed_delete_id = Some(id);
        }
    }

    fn feed_articles_ui(&mut self, ui: &mut egui::Ui) {
        // Global search across every feed's articles.
        if !self.feed_search.trim().is_empty() {
            let q = self.feed_search.trim().to_ascii_lowercase();
            let mut hits: Vec<(u32, usize, String, String)> = Vec::new();
            for f in &self.feeds.feeds {
                for (i, it) in f.items.iter().enumerate() {
                    if it.title.to_ascii_lowercase().contains(&q)
                        || it.text.to_ascii_lowercase().contains(&q)
                    {
                        let feed = if f.title.trim().is_empty() {
                            f.url.clone()
                        } else {
                            f.title.clone()
                        };
                        hits.push((f.id, i, it.title.clone(), feed));
                    }
                }
            }
            ui.label(
                RichText::new(format!("{} result(s)", hits.len()))
                    .weak()
                    .small(),
            );
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (fid, idx, title, feed) in hits {
                        let clicked = ui
                            .horizontal(|ui| {
                                icons::icon(ui, Icon::Rss, 13.0, ui.visuals().weak_text_color());
                                ui.add(egui::Button::new(title).frame(false)).clicked()
                            })
                            .inner;
                        if clicked {
                            self.feed_sel = Some(fid);
                            self.feed_article = Some(idx);
                        }
                        ui.label(RichText::new(feed).weak().small());
                        ui.separator();
                    }
                });
            return;
        }

        let Some(fid) = self.feed_sel else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.weak("Select a feed to read its articles"));
            return;
        };

        // Article list.
        egui::SidePanel::left("feed_articles")
            .resizable(true)
            .default_width(320.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let items: Vec<(usize, String, String)> = if fid == 0 {
                            self.feeds
                                .saved
                                .iter()
                                .enumerate()
                                .map(|(i, it)| (i, it.title.clone(), it.date.clone()))
                                .collect()
                        } else {
                            self.feeds
                                .find(fid)
                                .map(|f| {
                                    f.items
                                        .iter()
                                        .enumerate()
                                        .map(|(i, it)| (i, it.title.clone(), it.date.clone()))
                                        .collect()
                                })
                                .unwrap_or_default()
                        };
                        if fid != 0 {
                            if let Some(f) = self.feeds.find(fid) {
                                if !f.error.is_empty() {
                                    ui.colored_label(theme::DANGER, f.error.clone());
                                }
                            }
                        }
                        if items.is_empty() {
                            ui.add_space(8.0);
                            ui.weak(if fid == 0 {
                                "No saved articles yet — press the bookmark icon on an article."
                            } else {
                                "No articles — press Refresh."
                            });
                        }
                        for (i, title, date) in items {
                            let sel = self.feed_article == Some(i);
                            if ui
                                .selectable_label(sel, RichText::new(title).strong())
                                .clicked()
                            {
                                self.feed_article = Some(i);
                            }
                            if !date.is_empty() {
                                ui.label(RichText::new(date).weak().small());
                            }
                            ui.separator();
                        }
                    });
            });

        self.feed_reader_ui(ui, fid);
    }

    fn feed_reader_ui(&mut self, ui: &mut egui::Ui, fid: u32) {
        let Some(idx) = self.feed_article else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.weak("Select an article to read"));
            return;
        };
        let item = if fid == 0 {
            self.feeds.saved.get(idx).cloned()
        } else {
            self.feeds.find(fid).and_then(|f| f.items.get(idx).cloned())
        };
        let Some(item) = item else {
            return;
        };
        let is_saved = self.feeds.is_saved(&item);
        let mut save_toggle = false;
        let mut load_full = false;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&item.title).size(22.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(
                            ui,
                            Icon::Bookmark,
                            20.0,
                            true,
                            is_saved,
                            if is_saved {
                                "Saved — click to remove"
                            } else {
                                "Save article"
                            },
                        )
                        .clicked()
                        {
                            save_toggle = true;
                        }
                    });
                });
                if !item.date.is_empty() {
                    ui.add_space(2.0);
                    ui.label(RichText::new(&item.date).weak().small());
                }
                ui.add_space(12.0);

                // Full article text (paragraph-styled).
                let body = if item.full.is_empty() {
                    item.text.clone()
                } else {
                    item.full.clone()
                };
                if body.is_empty() {
                    ui.weak("No text content in this item.");
                } else {
                    // A comfortable reading column (max ~720 px, centred) with
                    // generous paragraph spacing.
                    let size = self.config.font_size + 2.0;
                    let width = ui.available_width();
                    let col = width.min(720.0);
                    ui.horizontal(|ui| {
                        ui.add_space(((width - col) * 0.5).max(0.0));
                        ui.vertical(|ui| {
                            ui.set_max_width(col);
                            for para in body.split('\n') {
                                if para.trim().is_empty() {
                                    ui.add_space(6.0);
                                } else {
                                    ui.add(
                                        egui::Label::new(RichText::new(para).size(size)).wrap(true),
                                    );
                                    ui.add_space(10.0);
                                }
                            }
                        });
                    });
                }

                // Offer to fetch the full article when we only have a summary.
                if item.full.is_empty() && !item.link.is_empty() {
                    ui.add_space(2.0);
                    let loading = self.feed_full_loading.as_deref() == Some(item.link.as_str());
                    if icons::button(
                        ui,
                        Icon::Rss,
                        16.0,
                        !loading,
                        false,
                        "Load the full article text",
                    )
                    .clicked()
                    {
                        load_full = true;
                    }
                    ui.label(
                        RichText::new(if loading {
                            "Loading full article…"
                        } else {
                            "Load full article"
                        })
                        .small()
                        .weak(),
                    );
                    ui.add_space(10.0);
                }

                // The article link, below the text, as a modern clickable row.
                ui.add_space(6.0);
                if !item.link.is_empty() {
                    link_row(ui, &item.link, "Read the full article");
                    ui.add_space(6.0);
                }
                ui.add_space(12.0);
            });

        if save_toggle {
            self.feeds.toggle_saved(&item);
            self.feeds.save();
            if fid == 0 && is_saved {
                // Removed from the saved list while viewing it.
                self.feed_article = None;
            }
        }
        if load_full {
            self.load_full_article(&item.link.clone());
        }
    }

    /// Add / edit / delete feed dialogs.
    fn feeds_dialogs(&mut self, ctx: &egui::Context) {
        // Add or edit.
        if self.feed_add_open || self.feed_edit_id.is_some() {
            let editing = self.feed_edit_id.is_some();
            let mut submit = false;
            let mut cancel = false;
            egui::Window::new("Feed")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(
                        RichText::new(if editing { "Edit feed" } else { "Add feed" }).strong(),
                    );
                    ui.add_space(4.0);
                    ui.label(RichText::new("Feed URL").small().weak());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feed_url)
                            .hint_text("https://example.com/feed.xml")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.label(RichText::new("Title (optional)").small().weak());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feed_title)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.label(RichText::new("Category (optional)").small().weak());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feed_category)
                            .hint_text("News, Tech, …")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if icons::button(ui, Icon::Check, 20.0, true, false, "Save").clicked() {
                            submit = true;
                        }
                        if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            let _ = editing;
            if submit {
                let url = self.feed_url.trim().to_string();
                if url.is_empty() {
                    self.status = "Feed URL is required".into();
                } else if let Some(id) = self.feed_edit_id {
                    if let Some(f) = self.feeds.find_mut(id) {
                        f.url = url;
                        f.title = self.feed_title.clone();
                        f.category = self.feed_category.clone();
                        f.error.clear();
                    }
                    self.feeds.save();
                    self.feed_edit_id = None;
                    self.refresh_feeds();
                } else {
                    self.feeds
                        .add(&url, self.feed_title.trim(), self.feed_category.trim());
                    self.feeds.save();
                    self.feed_add_open = false;
                    self.refresh_feeds();
                }
            }
            if cancel {
                self.feed_add_open = false;
                self.feed_edit_id = None;
            }
        }

        // Delete confirmation.
        if let Some(id) = self.feed_delete_id {
            let name = self
                .feeds
                .find(id)
                .map(|f| {
                    if f.title.trim().is_empty() {
                        f.url.clone()
                    } else {
                        f.title.clone()
                    }
                })
                .unwrap_or_default();
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Delete feed")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("Remove feed '{name}'?"));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if icons::danger_button(ui, Icon::Trash, 20.0, true, "Delete").clicked() {
                            confirm = true;
                        }
                        if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                self.feeds.remove(id);
                self.feeds.save();
                if self.feed_sel == Some(id) {
                    self.feed_sel = None;
                    self.feed_article = None;
                }
                self.feed_delete_id = None;
            } else if cancel {
                self.feed_delete_id = None;
            }
        }
    }

    // ------------------------------------------------------------ attachments

    fn open_attach_picker(&mut self, kind: AttachKind) {
        self.attach_picker = Some(FilePicker::new(kind.start_dir(), kind));
    }

    /// Copy a file into `<vault>/assets/` and reference it from the active note.
    fn attach_file(&mut self, src: &Path) {
        let Some(root) = self.vault.as_ref().map(|v| v.root.clone()) else {
            self.status = "Open a vault first".into();
            return;
        };
        if self.tabs.is_empty() {
            self.status = "Open a note first".into();
            return;
        }
        let assets = root.join("assets");
        if let Err(e) = fs::create_dir_all(&assets) {
            self.status = format!("Attach failed: {e}");
            return;
        }
        let name = src
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let stem = Path::new(&name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = Path::new(&name)
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut dest = assets.join(&name);
        let mut counter = 1;
        while dest.exists() {
            let fname = if ext.is_empty() {
                format!("{stem} {counter}")
            } else {
                format!("{stem} {counter}.{ext}")
            };
            dest = assets.join(fname);
            counter += 1;
        }
        if let Err(e) = fs::copy(src, &dest) {
            self.status = format!("Attach failed: {e}");
            return;
        }
        let fname = dest
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let rel = format!("assets/{fname}");
        let idx = self.active;
        let before = self.tabs[idx].text.clone();
        self.tabs[idx]
            .text
            .push_str(&format!("\n\n![{name}]({rel})\n"));
        // Record the insertion so undo/redo works with attachments too.
        if self.tabs[idx].text != before {
            self.tabs[idx].undo.push(before);
            if self.tabs[idx].undo.len() > 500 {
                self.tabs[idx].undo.remove(0);
            }
            self.tabs[idx].redo.clear();
        }
        let path = self.tabs[idx].path.clone();
        let text = self.tabs[idx].text.clone();
        match self.write_note(&path, &text) {
            Ok(()) => {
                self.tabs[idx].saved = text;
                self.status = format!("Attached {rel}");
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// Render a note's attached images inline (resizable / movable) and its
    /// audio / video as inline players that play without an external window.
    fn media_strip(&mut self, ui: &mut egui::Ui, text: &str) {
        let Some(root) = self.vault.as_ref().map(|v| v.root.clone()) else {
            return;
        };
        let targets = media_links(text);
        if targets.is_empty() {
            return;
        }
        let mut shown = 0;
        for rel in targets {
            let Some(path) = vault_file(&root, &rel) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if is_image_ext(&ext) {
                self.image_widget(ui, &path);
                shown += 1;
            } else if is_audio_ext(&ext) {
                self.audio_widget(ui, &path);
                shown += 1;
            } else if is_video_ext(&ext) {
                self.video_widget(ui, &path);
                shown += 1;
            }
        }
        if shown > 0 {
            ui.separator();
        }
    }

    /// Get (decoding on first use) the GPU texture for an image attachment.
    fn media_texture(&mut self, ctx: &egui::Context, path: &Path) -> Option<egui::TextureHandle> {
        if let Some(tex) = self.media_textures.get(path) {
            return Some(tex.clone());
        }
        let image = load_color_image(path)?;
        let tex = ctx.load_texture(
            format!("media:{}", path.display()),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.media_textures.insert(path.to_path_buf(), tex.clone());
        Some(tex)
    }

    /// Draw an image inline. Drag the image to move it; drag the bottom-right
    /// handle to resize. The layout is remembered per attachment.
    fn image_widget(&mut self, ui: &mut egui::Ui, path: &Path) {
        let key = path.to_string_lossy().to_string();
        let mut layout = self.media_layout.get(&key);
        let avail = ui.available_width().max(64.0);

        let Some(tex) = self.media_texture(ui.ctx(), path) else {
            // Could not decode: show a small placeholder so the layout is stable.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(avail, 120.0), egui::Sense::hover());
            let weak = ui.visuals().weak_text_color();
            let bg = ui.visuals().faint_bg_color;
            let painter = ui.painter();
            painter.rect_filled(rect, egui::Rounding::same(8.0), bg);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Image could not be shown",
                egui::FontId::proportional(13.0),
                weak,
            );
            ui.add_space(6.0);
            return;
        };

        let size = tex.size_vec2();
        let aspect = if size.x > 0.0 { size.y / size.x } else { 0.75 };
        // Cap the default height so a tall image can't fill the whole viewport.
        let max_h = (ui.ctx().screen_rect().height() * 0.75).max(160.0);
        let mut w = if layout.w <= 1.0 {
            size.x.min(avail)
        } else {
            layout.w.clamp(48.0, avail * 4.0)
        };
        let mut h = (w * aspect).max(24.0);
        if h > max_h {
            w *= max_h / h;
            h = max_h;
        }

        // Reserve a band tall enough for the image plus any downward offset.
        let down = layout.oy.max(0.0);
        let (band, _) =
            ui.allocate_exact_size(egui::vec2(avail, h + down + 4.0), egui::Sense::hover());
        let img_rect = egui::Rect::from_min_size(
            egui::pos2(band.min.x + layout.ox, band.min.y + layout.oy),
            egui::vec2(w, h),
        );

        // Move by dragging the image body.
        let drag_id = ui.id().with(("imgmove", &key));
        let drag = ui.interact(img_rect, drag_id, egui::Sense::click_and_drag());
        if drag.dragged() {
            let d = drag.drag_delta();
            layout.ox += d.x;
            layout.oy += d.y;
        }
        if drag.hovered() || drag.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        // Resize by dragging the bottom-right handle.
        let hs = 15.0;
        let hrect =
            egui::Rect::from_min_size(img_rect.max - egui::vec2(hs, hs), egui::vec2(hs, hs));
        let hdrag = ui.interact(hrect, drag_id.with("resize"), egui::Sense::drag());
        if hdrag.dragged() {
            let nw = (w + hdrag.drag_delta().x).clamp(48.0, avail * 4.0);
            layout.w = nw;
        }
        if hdrag.hovered() || hdrag.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        }

        egui::Image::new(egui::load::SizedTexture::from_handle(&tex))
            .fit_to_exact_size(egui::vec2(w, h))
            .rounding(egui::Rounding::same(8.0))
            .paint_at(ui, img_rect);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(img_rect.max - egui::vec2(11.0, 11.0), egui::vec2(9.0, 9.0)),
            egui::Rounding::same(2.0),
            theme::ACCENT,
        );

        if drag.drag_stopped() || hdrag.drag_stopped() {
            self.media_layout.set(&key, layout);
            self.media_layout.save();
        }
        ui.add_space(6.0);
    }

    /// Draw an inline audio player (play / pause / stop, scrubber, volume).
    fn audio_widget(&mut self, ui: &mut egui::Ui, path: &Path) {
        let active = self.player_path.as_path() == path;
        if active {
            self.player_rendered = true;
        }
        let action = self.media_controls(ui, path, false);
        ui.add_space(6.0);
        self.apply_transport(path, action, false);
    }

    /// Draw an inline video player (controls, scrubber, volume, then the frame).
    fn video_widget(&mut self, ui: &mut egui::Ui, path: &Path) {
        let active = self.player_path.as_path() == path;
        if active {
            self.player_rendered = true;
            self.update_video_texture(ui.ctx());
        }
        let tex = if active {
            self.player_tex.clone()
        } else {
            None
        };
        let action = self.media_controls(ui, path, true);
        ui.add_space(6.0);

        let avail = ui.available_width().max(64.0);
        match &tex {
            Some(tex) => {
                let size = tex.size_vec2();
                let w = size.x.min(avail);
                let h = (w * (size.y / size.x.max(1.0))).max(1.0);
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                        .fit_to_exact_size(egui::vec2(w, h))
                        .rounding(egui::Rounding::same(8.0)),
                );
            }
            None => {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(avail, 200.0), egui::Sense::hover());
                let bg = ui.visuals().faint_bg_color;
                let weak = ui.visuals().weak_text_color();
                let painter = ui.painter();
                painter.rect_filled(rect, egui::Rounding::same(8.0), bg);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if active {
                        "Buffering…"
                    } else {
                        "Press play to watch"
                    },
                    egui::FontId::proportional(14.0),
                    weak,
                );
            }
        }
        ui.add_space(6.0);
        self.apply_transport(path, action, true);
    }

    /// The shared transport box: play / pause, stop, file name, time, a
    /// click-to-seek scrubber, and a live volume slider.
    fn media_controls(&mut self, ui: &mut egui::Ui, path: &Path, video: bool) -> Option<Transport> {
        let active = self.player_path.as_path() == path;
        let (elapsed, duration, paused) = self.player_progress(active);
        let mut action = None;
        let mut volume = self.media_layout.volume;
        let mut vol_released = false;
        media_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let playing = active && !paused;
                if icons::button(
                    ui,
                    if playing { Icon::Pause } else { Icon::Play },
                    16.0,
                    true,
                    playing,
                    if active { "Pause / resume" } else { "Play" },
                )
                .clicked()
                {
                    action = Some(if active {
                        Transport::Toggle
                    } else {
                        Transport::Start
                    });
                }
                if active && icons::button(ui, Icon::Stop, 16.0, true, false, "Stop").clicked() {
                    action = Some(Transport::Stop);
                }
                icons::icon(
                    ui,
                    if video { Icon::Video } else { Icon::Mic },
                    15.0,
                    theme::LINK,
                );
                if let Some(n) = path.file_name() {
                    ui.label(RichText::new(n.to_string_lossy().to_string()).strong());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if active {
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                fmt_time(elapsed),
                                fmt_time(duration)
                            ))
                            .weak()
                            .small(),
                        );
                    }
                });
            });
            ui.add_space(4.0);
            if active {
                if let Some(frac) = scrubber(ui, elapsed, duration) {
                    action = Some(Transport::Seek(frac));
                }
            } else {
                inactive_bar(ui);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                icons::icon(ui, Icon::Volume, 14.0, ui.visuals().weak_text_color());
                let resp = ui.add(
                    egui::Slider::new(&mut volume, 0.0..=1.5)
                        .show_value(false)
                        .handle_shape(egui::style::HandleShape::Circle),
                );
                ui.label(
                    RichText::new(format!("{}%", (volume * 100.0).round() as i32))
                        .weak()
                        .small(),
                );
                if resp.drag_stopped() || resp.lost_focus() {
                    vol_released = true;
                }
            });
        });
        if (volume - self.media_layout.volume).abs() > 1.0e-4 {
            self.media_layout.volume = volume;
            self.set_volume(volume);
            if vol_released {
                self.media_layout.save();
            }
        } else if vol_released {
            self.media_layout.save();
        }
        action
    }

    /// Progress (elapsed, duration, paused) of the active player, if `active`.
    fn player_progress(&self, active: bool) -> (f32, f32, bool) {
        if active {
            if let Some(p) = &self.player {
                return (p.elapsed(), p.duration(), p.is_paused());
            }
        }
        (0.0, 0.0, false)
    }

    /// Set the playback volume (applied live to the running player).
    fn set_volume(&mut self, v: f32) {
        if let Some(p) = self.player.as_ref() {
            p.set_volume(v);
        }
    }

    /// Begin playing `path` (audio when `video` is false), stopping anything
    /// already playing.
    fn start_media(&mut self, path: &Path, video: bool) {
        self.stop_media();
        let info = media::probe(path);
        let volume = self.media_layout.volume;
        let player = if video {
            media::Video::open(path, media::VIDEO_WIDTH, info, 0.0, volume)
                .map(media::Player::Video)
        } else {
            media::Sound::start(path, info.duration, 0.0, volume).map(media::Player::Audio)
        };
        match player {
            Some(p) => {
                self.player = Some(p);
                self.player_path = path.to_path_buf();
                self.player_tex = None;
                // Mark as rendered so the lifecycle check doesn't stop it before
                // the next frame draws the controls.
                self.player_rendered = true;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.status = format!("Playing {name}");
            }
            None => {
                self.status = "Could not play: is ffmpeg / pw-cat / aplay installed?".into();
            }
        }
    }

    /// Stop any in-app media playback.
    fn stop_media(&mut self) {
        if let Some(mut p) = self.player.take() {
            p.stop();
        }
        self.player_path = PathBuf::new();
        self.player_tex = None;
    }

    /// Advance the media lifecycle once per frame: stop finished playback, stop
    /// playback that was not drawn last frame (note changed / locked), and keep
    /// animating while something is playing.
    fn tick_media(&mut self, ctx: &egui::Context) {
        let finished = if let Some(p) = self.player.as_mut() {
            p.finished()
        } else {
            false
        };
        if finished || (self.player.is_some() && !self.player_rendered) {
            self.stop_media();
        }
        self.player_rendered = false;
        if self.player.is_some() {
            ctx.request_repaint();
        }
    }

    /// Apply a play/pause/stop action chosen by the inline controls.
    fn apply_transport(&mut self, path: &Path, action: Option<Transport>, video: bool) {
        match action {
            Some(Transport::Start) => self.start_media(path, video),
            Some(Transport::Toggle) => {
                if let Some(p) = self.player.as_mut() {
                    if p.is_paused() {
                        p.resume();
                    } else {
                        p.pause();
                    }
                }
            }
            Some(Transport::Stop) => self.stop_media(),
            Some(Transport::Seek(frac)) => {
                if let Some(p) = self.player.as_mut() {
                    let d = p.duration();
                    if d > 0.0 {
                        p.seek(frac * d);
                    }
                }
            }
            None => {}
        }
    }

    /// Upload the newest decoded video frame into `player_tex`.
    fn update_video_texture(&mut self, ctx: &egui::Context) {
        let Self {
            player, player_tex, ..
        } = self;
        if let Some(media::Player::Video(v)) = player.as_mut() {
            if let Some(frame) = v.frame() {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    &frame.rgba,
                );
                match player_tex {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => {
                        *player_tex = Some(ctx.load_texture(
                            "encrypted-note-video",
                            image,
                            egui::TextureOptions::LINEAR,
                        ))
                    }
                }
            }
        }
    }

    // ----------------------------------------------------------------- gate

    /// Drop the key and all decrypted state, returning to the unlock screen.
    fn lock(&mut self) {
        self.save_all();
        self.stop_media();
        // Clear decrypted secrets from memory.
        self.passwords_open = false;
        self.passwords = passwords::Store::default();
        self.pw_editor = None;
        self.pw_sel = None;
        self.pw_reveal.clear();
        self.pw_gen_value.clear();
        self.media_textures.clear();
        self.crypto = None;
        self.tabs.clear();
        self.active = 0;
        self.backlinks.clear();
        self.last_active = None;
        self.gate = Gate::Unlock {
            password: String::new(),
            error: None,
        };
        self.status = "Locked".into();
    }

    /// After a successful unlock, reopen the last vault.
    fn after_unlock(&mut self) {
        if let Some(path) = self.config.last_vault.clone() {
            if path.is_dir() {
                self.open_vault(path);
            }
        }
    }

    /// Validate and install a new master password on first run.
    fn try_create_password(&mut self) {
        let (password, confirm) = match &self.gate {
            Gate::CreatePassword {
                password, confirm, ..
            } => (password.clone(), confirm.clone()),
            _ => return,
        };
        let problems = Keyfile::password_problems(&password);
        let error = if !problems.is_empty() {
            Some(format!("Password {}", problems.join(", ")))
        } else if password != confirm {
            Some("Passwords do not match".to_string())
        } else {
            None
        };
        if let Some(error) = error {
            if let Gate::CreatePassword { error: slot, .. } = &mut self.gate {
                *slot = Some(error);
            }
            return;
        }
        match VaultCrypto::create(&password) {
            Ok((keyfile, crypto)) => {
                if let Err(e) = keyfile.save() {
                    if let Gate::CreatePassword { error: slot, .. } = &mut self.gate {
                        *slot = Some(format!("Could not save keyfile: {e}"));
                    }
                    return;
                }
                self.crypto = Some(crypto);
                self.config.reset_lockout();
                self.config.save();
                // Do not open the vault yet: show the "record your password"
                // popup first. The vault opens when the user presses OK.
                self.show_password_warning = true;
                self.status = "Master password set".into();
            }
            Err(e) => {
                if let Gate::CreatePassword { error: slot, .. } = &mut self.gate {
                    *slot = Some(format!("Key derivation failed: {e}"));
                }
            }
        }
    }

    /// Verify an entered password against the keyfile.
    fn try_unlock(&mut self) {
        let password = match &self.gate {
            Gate::Unlock { password, .. } => password.clone(),
            _ => return,
        };
        let now = config::now_unix();
        if self.config.lockout_remaining(now).is_some() {
            return; // Still locked out.
        }
        let keyfile = match Keyfile::load() {
            Some(kf) => kf,
            None => {
                if let Gate::Unlock { error, .. } = &mut self.gate {
                    *error = Some("Keyfile missing; cannot unlock".into());
                }
                return;
            }
        };
        match VaultCrypto::unlock(&password, &keyfile) {
            Some(crypto) => {
                self.crypto = Some(crypto);
                self.config.reset_lockout();
                self.config.save();
                self.gate = Gate::Open;
                self.status = "Unlocked".into();
                self.after_unlock();
            }
            None => self.register_failed_attempt(),
        }
    }

    /// Record a failed unlock and, if the threshold is reached, arm a lockout
    /// with an exponentially increasing duration.
    fn register_failed_attempt(&mut self) {
        const THRESHOLD: u32 = 5;
        let message = match self.config.register_failure(THRESHOLD) {
            Some(minutes) => {
                format!("Too many incorrect attempts — locked for {minutes} minute(s).")
            }
            None => {
                let left = THRESHOLD.saturating_sub(self.config.failed_attempts);
                format!("Incorrect password. {left} attempt(s) before lockout.")
            }
        };
        self.gate = Gate::Unlock {
            password: String::new(),
            error: Some(message),
        };
        self.config.save();
    }

    /// Close the "record your password" popup and open the vault. On first run
    /// there's no vault yet, so we prompt the user to choose a folder.
    fn dismiss_password_warning(&mut self) {
        self.show_password_warning = false;
        self.gate = Gate::Open;
        self.after_unlock();
        if self.vault.is_none() {
            self.open_picker();
            self.status = "Choose a folder to use as your vault".into();
        }
    }

    /// The post-setup popup: a modern card warning that the master password
    /// cannot be recovered, with an OK icon at the bottom-right that opens the
    /// vault.
    fn password_warning_ui(&mut self, ctx: &egui::Context) {
        let mut ok = false;
        let shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 12.0),
            blur: 34.0,
            spread: 0.0,
            color: Color32::from_black_alpha(190),
        };
        let card = egui::Frame::none()
            .fill(theme::PANEL)
            .rounding(egui::Rounding::same(18.0))
            .stroke(egui::Stroke::new(
                1.0_f32,
                theme::ACCENT.linear_multiply(0.6),
            ))
            .shadow(shadow)
            .inner_margin(egui::Margin::same(30.0));

        egui::CentralPanel::default().show(ctx, |ui| {
            // Dim the background so the popup reads as a modal.
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_black_alpha(150));
            ui.add_space(96.0);
            ui.vertical_centered(|ui| {
                card.show(ui, |ui| {
                    ui.set_width(430.0);
                    ui.vertical_centered(|ui| {
                        icons::icon(ui, Icon::Warning, 44.0, Color32::from_rgb(0xf9, 0xe2, 0xaf));
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("Record your master password")
                                .size(22.0)
                                .strong(),
                        );
                    });
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(
                            "Store this password somewhere safe right now — a password manager, \
                             or written down and kept secure. It is the only key to your notes.",
                        )
                        .size(14.0),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "If you lose it, your notes cannot be recovered. There is no reset \
                             and no one who can unlock them for you.",
                        )
                        .size(14.0)
                        .color(theme::DANGER),
                    );
                    ui.add_space(20.0);
                    // OK icon, bottom-right edge of the card.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(
                            ui,
                            Icon::Check,
                            28.0,
                            true,
                            true,
                            "OK — choose your vault folder",
                        )
                        .clicked()
                        {
                            ok = true;
                        }
                    });
                });
            });
        });

        if ok {
            self.dismiss_password_warning();
        }
    }

    /// Full-window password screen shown while locked.
    fn gate_ui(&mut self, ctx: &egui::Context) {
        let now = config::now_unix();
        let locked = self.config.lockout_remaining(now);
        let total =
            (Config::lockout_minutes(self.config.lockout_step.saturating_sub(1)) * 60).max(1);
        let fraction = locked
            .map(|s| (s as f32 / total as f32).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let time = ctx.input(|i| i.time);
        let mut action = GateAction::None;
        let mut forgot = false;
        let mut generate = false;

        let card = egui::Frame::none()
            .fill(theme::WINDOW_FILL)
            .rounding(egui::Rounding::same(18.0))
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .shadow(egui::epaint::Shadow {
                offset: egui::vec2(0.0, 18.0),
                blur: 44.0,
                spread: 0.0,
                color: egui::Color32::from_black_alpha(170),
            })
            .inner_margin(egui::Margin::same(30.0));

        let mut show_pw = self.gate_show;
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::BASE))
            .show(ctx, |ui| {
                gate_background(ui.painter(), ui.max_rect(), time);
            });

        egui::Area::new(egui::Id::new("gate_card"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                card.show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.vertical_centered(|ui| {
                        let (lrect, _) = ui.allocate_exact_size(
                            egui::vec2(84.0, 84.0),
                            egui::Sense::hover(),
                        );
                        let c = lrect.center();
                        ui.painter()
                            .circle_filled(c, 42.0, theme::PRIMARY.gamma_multiply(0.55));
                        ui.painter().circle_stroke(
                            c,
                            35.0,
                            egui::Stroke::new(1.5_f32, theme::PRIMARY_HOVER),
                        );
                        ui.put(
                            lrect.shrink(16.0),
                            egui::Image::new(egui::include_image!("../assets/logo.svg")),
                        );
                        ui.add_space(12.0);
                        ui.label(RichText::new("Encrypted Note").size(28.0).strong());
                        ui.add_space(2.0);
                        ui.label(RichText::new("AES-256 encrypted markdown vault").weak());
                    });
                    ui.add_space(22.0);

                    match &mut self.gate {
                        Gate::CreatePassword {
                            password,
                            confirm,
                            error,
                        } => {
                            ui.label(RichText::new("Create a master password").strong());
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "16+ characters with upper- and lower-case letters, a number and a special character. No words — avoid 4 letters in a row.",
                                )
                                .weak()
                                .small(),
                            );
                            ui.add_space(12.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut *password)
                                    .id(egui::Id::new("gate_pw"))
                                    .password(true)
                                    .hint_text("Master password")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(6.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut *confirm)
                                    .password(true)
                                    .hint_text("Confirm password")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add_space(10.0);
                            policy_checklist(ui, password);
                            if let Some(err) = error {
                                ui.add_space(6.0);
                                ui.label(RichText::new(err.clone()).color(theme::DANGER).small());
                            }
                            ui.add_space(16.0);
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - 8.0) * 0.5;
                                if ui
                                    .add_sized(
                                        egui::vec2(w, 40.0),
                                        egui::Button::image_and_text(
                                            Icon::Check.image(18.0, Color32::WHITE),
                                            "Set password",
                                        )
                                        .fill(theme::PRIMARY)
                                        .rounding(egui::Rounding::same(10.0)),
                                    )
                                    .clicked()
                                {
                                    action = GateAction::Create;
                                }
                                if ui
                                    .add_sized(
                                        egui::vec2(w, 40.0),
                                        egui::Button::image_and_text(
                                            Icon::Generate.image(18.0, theme::LINK),
                                            "Generate",
                                        )
                                        .rounding(egui::Rounding::same(10.0)),
                                    )
                                    .clicked()
                                {
                                    generate = true;
                                }
                            });
                        }
                        Gate::Unlock { password, error } => {
                            if let Some(secs) = locked {
                                ui.vertical_centered(|ui| {
                                    hourglass(ui, 92.0, fraction, time);
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new("Too many incorrect attempts")
                                            .color(theme::DANGER)
                                            .strong(),
                                    );
                                    ui.add_space(4.0);
                                    ui.label(RichText::new("Try again in").weak().small());
                                    ui.label(RichText::new(fmt_duration(secs)).size(26.0).strong());
                                });
                            } else {
                                ui.label(RichText::new("Enter your master password").strong());
                                ui.add_space(12.0);
                                let resp = ui
                                    .horizontal(|ui| {
                                        icons::icon(
                                            ui,
                                            Icon::Key,
                                            16.0,
                                            ui.visuals().weak_text_color(),
                                        );
                                        let w = ui.available_width() - 30.0;
                                        let r = ui.add(
                                            egui::TextEdit::singleline(&mut *password)
                                                .id(egui::Id::new("gate_pw"))
                                                .password(!show_pw)
                                                .hint_text("Master password")
                                                .desired_width(w),
                                        );
                                        if icons::button(
                                            ui,
                                            if show_pw { Icon::EyeOff } else { Icon::Eye },
                                            18.0,
                                            true,
                                            show_pw,
                                            "Show / hide",
                                        )
                                        .clicked()
                                        {
                                            show_pw = !show_pw;
                                        }
                                        r
                                    })
                                    .inner;
                                if let Some(err) = error {
                                    ui.add_space(6.0);
                                    ui.label(RichText::new(err.clone()).color(theme::DANGER).small());
                                }
                                ui.add_space(14.0);
                                let enter = resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                // A modern, icon-only primary button: the icon is
                                // centred on the accent (purple) pill.
                                let (brect, bresp) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), 44.0),
                                    egui::Sense::click(),
                                );
                                let fill = if bresp.hovered() {
                                    theme::PRIMARY_HOVER
                                } else {
                                    theme::PRIMARY
                                };
                                ui.painter()
                                    .rect_filled(brect, egui::Rounding::same(12.0), fill);
                                let icon = 22.0;
                                Icon::CheckCircle.image(icon, Color32::WHITE).paint_at(
                                    ui,
                                    egui::Rect::from_center_size(
                                        brect.center(),
                                        egui::vec2(icon, icon),
                                    ),
                                );
                                let bresp = bresp
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text("Unlock");
                                if bresp.clicked() || enter {
                                    action = GateAction::Unlock;
                                }
                            }
                            ui.add_space(16.0);
                            ui.vertical_centered(|ui| {
                                ui.horizontal(|ui| {
                                    if icons::button(
                                        ui,
                                        Icon::Warning,
                                        15.0,
                                        locked.is_none(),
                                        false,
                                        "Forgot password? Erase everything.",
                                    )
                                    .clicked()
                                    {
                                        forgot = true;
                                    }
                                    ui.label(
                                        RichText::new("Forgot password?")
                                            .color(theme::LINK)
                                            .small(),
                                    );
                                });
                            });
                        }
                        Gate::Open => {}
                    }
                });
            });
        self.gate_show = show_pw;

        // Keep the sand draining smoothly while locked out.
        if locked.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }

        // Make sure the password field is focused so typing works immediately —
        // important when the window opens fullscreen.
        if !self.confirm_forgot
            && !matches!(self.gate, Gate::Open)
            && ctx.memory(|m| m.focused().is_none())
        {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("gate_pw")));
        }

        match action {
            GateAction::Create => self.try_create_password(),
            GateAction::Unlock => self.try_unlock(),
            GateAction::None => {}
        }
        if forgot {
            self.confirm_forgot = true;
        }
        if generate {
            if let Gate::CreatePassword {
                password, confirm, ..
            } = &mut self.gate
            {
                let pw = crypto::generate_password();
                *password = pw.clone();
                *confirm = pw;
            }
        }

        // Erase-everything confirmation.
        if self.confirm_forgot {
            let mut confirm = false;
            let mut cancel = false;
            egui::Window::new("Erase everything?")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(430.0);
                    ui.horizontal(|ui| {
                        icons::icon(ui, Icon::Warning, 18.0, theme::DANGER);
                        ui.colored_label(
                            theme::DANGER,
                            RichText::new("Everything will be lost. This cannot be undone.")
                                .strong(),
                        );
                    });
                    ui.add_space(8.0);
                    ui.label(
                        "All notes in your vault will be permanently deleted, along with your \
                         master password. There is no way to recover them. You will start fresh \
                         with an empty vault and a new password.",
                    );
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        if icons::danger_button(
                            ui,
                            Icon::Trash,
                            24.0,
                            true,
                            "Yes, delete everything",
                        )
                        .clicked()
                        {
                            confirm = true;
                        }
                        if icons::button(ui, Icon::Close, 24.0, true, false, "Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                self.forgot_password();
            } else if cancel {
                self.confirm_forgot = false;
            }
        }
    }

    /// Erase everything: delete the vault's notes and the keyfile, reset app
    /// state (lockout, recents), and drop back to password creation so the user
    /// ends up with an empty vault under a brand-new password.
    fn forgot_password(&mut self) {
        // 1. Delete every note in the last known vault.
        if let Some(root) = self.config.last_vault.clone() {
            if let Ok(vault) = Vault::open(root) {
                for note in &vault.notes {
                    let _ = fs::remove_file(&note.path);
                }
            }
        }
        // 2. Delete the keyfile (salt + password verifier).
        let _ = fs::remove_file(Keyfile::path());

        // 3. Reset configuration but keep pointing at the (now empty) vault so
        //    the user lands in an empty vault once the new password is set.
        let last = self.config.last_vault.clone();
        let mut cfg = Config {
            last_vault: last.clone(),
            ..Default::default()
        };
        if let Some(path) = last {
            cfg.recent_vaults.push(path);
        }
        self.config = cfg;
        self.config.save();

        self.crypto = None;
        self.vault = None;
        self.tree = TreeNode::default();
        self.tabs.clear();
        self.active = 0;
        self.backlinks.clear();
        self.last_active = None;
        self.confirm_forgot = false;
        self.show_password_warning = false;
        self.gate = Gate::CreatePassword {
            password: String::new(),
            confirm: String::new(),
            error: None,
        };
        self.status = "Everything erased. Create a new password to start an empty vault.".into();
    }

    // ---------------------------------------------------- password manager

    /// Open or close the password manager.
    fn toggle_passwords(&mut self) {
        if self.passwords_open {
            self.passwords_open = false;
        } else {
            self.open_passwords();
        }
    }

    fn open_passwords(&mut self) {
        self.passwords = self.load_passwords();
        self.passwords_open = true;
        self.pw_sel = None;
        self.pw_search.clear();
        self.pw_category = None;
        self.pw_expanded.clear();
    }

    /// Decrypt and load the password store (empty if none exists yet).
    fn load_passwords(&self) -> passwords::Store {
        let Ok(bytes) = Vault::read_bytes(&passwords::store_path()) else {
            return passwords::Store::default();
        };
        let json = if crypto::is_encrypted(&bytes) {
            match self.crypto.as_ref().and_then(|c| c.decrypt(&bytes)) {
                Some(j) => j,
                None => return passwords::Store::default(),
            }
        } else {
            match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => return passwords::Store::default(),
            }
        };
        serde_json::from_str(&json).unwrap_or_default()
    }

    /// Encrypt and persist the password store.
    fn save_passwords(&self) {
        let Some(crypto) = self.crypto.as_ref() else {
            return;
        };
        if let Ok(json) = serde_json::to_string_pretty(&self.passwords) {
            let path = passwords::store_path();
            let _ = Vault::write_bytes(&path, &crypto.encrypt(&json));
            restrict_permissions(&path);
        }
    }

    /// Start a fresh entry in the editor.
    fn pw_new(&mut self) {
        self.pw_editor = Some(PwEditor {
            draft: passwords::Entry::default(),
            is_new: true,
            show: true,
            new_folder: false,
            new_folder_name: String::new(),
        });
        self.favicon_url.clear();
        self.favicon_error = None;
    }

    /// Kick off a background fetch of a favicon URL.
    fn start_favicon_fetch(&mut self, url: &str) {
        if self.favicon_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.favicon_rx = Some(rx);
        self.favicon_error = None;
        let url = url.to_string();
        std::thread::spawn(move || {
            let res = feeds::http_get(&url);
            let _ = tx.send((url, res));
        });
    }

    /// Poll favicon fetches and, after a short pause, fetch a newly-pasted URL.
    fn tick_favicon(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.favicon_rx {
            match rx.try_recv() {
                Ok((url, res)) => {
                    match res {
                        Ok(bytes) => match decode_image_bytes(&bytes) {
                            Some(img) => {
                                let tex = ctx.load_texture(
                                    format!("favicon:{url}"),
                                    img,
                                    egui::TextureOptions::LINEAR,
                                );
                                self.favicon_tex.insert(url, tex);
                                self.favicon_error = None;
                            }
                            None => {
                                self.favicon_error = Some("Could not decode that image".into());
                            }
                        },
                        Err(e) => self.favicon_error = Some(format!("Fetch failed: {e}")),
                    }
                    self.favicon_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.favicon_rx = None,
            }
        }

        let url = self.favicon_url.trim().to_string();
        let now = ctx.input(|i| i.time);
        if url != self.favicon_last {
            self.favicon_last = url;
            self.favicon_change_at = now;
        } else if !self.favicon_last.is_empty()
            && !self.favicon_tex.contains_key(&self.favicon_last)
            && self.favicon_rx.is_none()
            && now - self.favicon_change_at > 0.6
            && self.favicon_last.starts_with("http")
        {
            let url = self.favicon_last.clone();
            self.start_favicon_fetch(&url);
        }

        if self.favicon_rx.is_some() {
            ctx.request_repaint();
        }
    }

    fn passwords_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::Passwords, 16.0, theme::ACCENT);
            ui.label(RichText::new("Passwords").strong());
            ui.separator();
            if icons::button(ui, Icon::Add, 16.0, true, false, "New entry").clicked() {
                self.pw_new();
            }
            if icons::button(ui, Icon::Generate, 16.0, true, false, "Password generator").clicked()
            {
                self.pw_gen_open = true;
                self.pw_gen_value = passwords::generate(
                    self.pw_gen_len,
                    self.pw_gen_upper,
                    self.pw_gen_lower,
                    self.pw_gen_digits,
                    self.pw_gen_special,
                );
            }
            if icons::button(ui, Icon::Upload, 16.0, true, false, "Import / export JSON").clicked()
            {
                self.pw_io_open = true;
            }
            ui.separator();
            icons::icon(ui, Icon::Search, 14.0, ui.visuals().weak_text_color());
            ui.add(
                egui::TextEdit::singleline(&mut self.pw_search)
                    .hint_text("search entries…")
                    .desired_width(220.0),
            );
            if !self.pw_search.is_empty()
                && icons::button(ui, Icon::Close, 14.0, true, false, "Clear search").clicked()
            {
                self.pw_search.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Close, 16.0, true, false, "Close passwords").clicked() {
                    self.passwords_open = false;
                }
                ui.weak(format!(
                    "{} entr{}",
                    self.passwords.entries.len(),
                    if self.passwords.entries.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                ));
            });
        });
        ui.separator();

        egui::SidePanel::left("pw_list")
            .resizable(true)
            .default_width(280.0)
            .show_inside(ui, |ui| self.pw_list_ui(ui));
        self.pw_detail_ui(ui);

        self.pw_editor_ui(&ctx);
        self.pw_generator_ui(&ctx);
        self.pw_io_ui(&ctx);
        self.pw_new_folder_ui(&ctx);
        self.pw_folder_dialog_ui(&ctx);
        self.pw_delete_confirm_ui(&ctx);
    }

    fn pw_list_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let total = self.passwords.entries.len();
        let favs = self.passwords.entries.iter().filter(|e| e.favorite).count();
        let weak = self.passwords.weakest();
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::Star, 13.0, theme::ACCENT);
            ui.weak(format!("{favs} favorite · {weak} weak of {total}"));
        });
        ui.horizontal(|ui| {
            let fav_sel = self.pw_fav_only;
            if icons::button(ui, Icon::Star, 14.0, true, fav_sel, "Favorites only").clicked() {
                self.pw_fav_only = !self.pw_fav_only;
            }
            ui.label(RichText::new("Favorites only").small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::FolderPlus, 14.0, true, false, "New folder").clicked() {
                    self.pw_new_folder_open = true;
                    self.pw_new_folder_name.clear();
                }
            });
        });
        ui.separator();

        let query = self.pw_search.clone();
        let fav_only = self.pw_fav_only;
        let searching = !query.trim().is_empty() || fav_only;
        let folders = self.passwords.all_folders();
        let uncat = self.passwords.filtered(&query, Some(""), fav_only);

        let mut select: Option<u64> = None;
        let mut edit: Option<u64> = None;
        let mut del_entry: Option<u64> = None;
        let mut toggle: Option<String> = None;
        let mut rename: Option<String> = None;
        let mut delete: Option<String> = None;
        let mut shown_any = false;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for f in &folders {
                    let ids = self.passwords.filtered(&query, Some(f.as_str()), fav_only);
                    // While searching, hide folders with no matches.
                    if searching && ids.is_empty() {
                        continue;
                    }
                    let expanded = searching || self.pw_expanded.contains(f);
                    let sel = self.pw_category.as_deref() == Some(f.as_str());
                    let row = egui::Rect::from_min_size(
                        ui.next_widget_position(),
                        egui::vec2(ui.available_width(), 20.0),
                    );
                    let hovered = ui
                        .input(|i| i.pointer.hover_pos())
                        .is_some_and(|p| row.contains(p));
                    ui.horizontal(|ui| {
                        let chev = if expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        };
                        if icons::button(ui, chev, 14.0, true, false, "Expand / collapse").clicked()
                        {
                            toggle = Some(f.clone());
                        }
                        icons::icon(
                            ui,
                            Icon::Folder,
                            13.0,
                            if sel {
                                theme::ACCENT
                            } else {
                                ui.visuals().weak_text_color()
                            },
                        );
                        if ui
                            .selectable_label(sel, RichText::new(f).strong())
                            .clicked()
                        {
                            toggle = Some(f.clone());
                        }
                        if hovered {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if icons::button(
                                        ui,
                                        Icon::Trash,
                                        13.0,
                                        true,
                                        false,
                                        "Delete folder",
                                    )
                                    .clicked()
                                    {
                                        delete = Some(f.clone());
                                    }
                                    if icons::button(
                                        ui,
                                        Icon::Edit,
                                        13.0,
                                        true,
                                        false,
                                        "Rename folder",
                                    )
                                    .clicked()
                                    {
                                        rename = Some(f.clone());
                                    }
                                },
                            );
                        }
                    });
                    shown_any = true;
                    if expanded {
                        ui.indent(("pw_folder", f.as_str()), |ui| {
                            if ids.is_empty() {
                                ui.label(RichText::new("(empty)").weak().small());
                            }
                            for id in ids {
                                match self.pw_entry_row(ui, id) {
                                    Some(PwRowAction::Select) => select = Some(id),
                                    Some(PwRowAction::Edit) => edit = Some(id),
                                    Some(PwRowAction::Delete) => del_entry = Some(id),
                                    None => {}
                                }
                            }
                        });
                    }
                }

                // Uncategorized entries as their own node.
                if !uncat.is_empty() {
                    let expanded = searching || self.pw_expanded.contains("");
                    let sel = self.pw_category.as_deref() == Some("");
                    ui.horizontal(|ui| {
                        let chev = if expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        };
                        if icons::button(ui, chev, 14.0, true, false, "Expand / collapse").clicked()
                        {
                            toggle = Some(String::new());
                        }
                        icons::icon(
                            ui,
                            Icon::Folder,
                            13.0,
                            if sel {
                                theme::ACCENT
                            } else {
                                ui.visuals().weak_text_color()
                            },
                        );
                        if ui
                            .selectable_label(sel, RichText::new("Uncategorized").strong())
                            .clicked()
                        {
                            toggle = Some(String::new());
                        }
                    });
                    shown_any = true;
                    if expanded {
                        ui.indent("pw_uncat", |ui| {
                            for id in uncat {
                                match self.pw_entry_row(ui, id) {
                                    Some(PwRowAction::Select) => select = Some(id),
                                    Some(PwRowAction::Edit) => edit = Some(id),
                                    Some(PwRowAction::Delete) => del_entry = Some(id),
                                    None => {}
                                }
                            }
                        });
                    }
                }

                if !shown_any {
                    ui.add_space(8.0);
                    ui.weak("No matching entries. Press ＋ to add one.");
                }
            });

        if let Some(f) = toggle {
            if self.pw_expanded.contains(&f) {
                self.pw_expanded.remove(&f);
            } else {
                self.pw_expanded.insert(f.clone());
            }
            self.pw_category = Some(f);
        }
        if let Some(id) = select {
            self.pw_sel = Some(id);
        }
        if let Some(id) = edit {
            if let Some(entry) = self.passwords.get(id).cloned() {
                self.favicon_url = entry.favicon.clone();
                self.favicon_error = None;
                self.pw_editor = Some(PwEditor {
                    draft: entry,
                    is_new: false,
                    show: false,
                    new_folder: false,
                    new_folder_name: String::new(),
                });
            }
        }
        if let Some(id) = del_entry {
            self.pw_confirm_delete = Some(id);
        }
        if let Some(f) = rename {
            self.pw_folder_rename_name = f.clone();
            self.pw_folder_rename = Some(f);
        }
        if let Some(f) = delete {
            self.pw_folder_delete = Some(f);
        }
    }

    /// One credential row inside the folder tree. Returns the chosen action.
    fn pw_entry_row(&mut self, ui: &mut egui::Ui, id: u64) -> Option<PwRowAction> {
        let e = self.passwords.get(id)?;
        let title = if e.title.trim().is_empty() {
            "(untitled)".to_string()
        } else {
            e.title.clone()
        };
        let user = e.username.clone();
        let cat = e.category.clone();
        let icon = e.icon.clone();
        let fav_tex = self.favicon_tex.get(e.favicon.trim()).cloned();
        let fav = e.favorite;
        let sel = self.pw_sel == Some(id);
        let row = egui::Rect::from_min_size(
            ui.next_widget_position(),
            egui::vec2(ui.available_width(), 20.0),
        );
        let hovered = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| row.contains(p));
        let mut action = None;
        ui.horizontal(|ui| {
            if let Some(t) = &fav_tex {
                draw_tex(ui, t, 15.0);
            } else if icon.trim().is_empty() {
                icons::icon(
                    ui,
                    Icon::Passwords,
                    13.0,
                    if sel {
                        theme::ACCENT
                    } else {
                        ui.visuals().weak_text_color()
                    },
                );
            } else {
                ui.label(RichText::new(icon.clone()).size(15.0));
            }
            let clicked = ui
                .vertical(|ui| {
                    if ui
                        .selectable_label(sel, RichText::new(&title).strong())
                        .clicked()
                    {
                        return true;
                    }
                    let sub = if user.trim().is_empty() {
                        cat.clone()
                    } else {
                        user.clone()
                    };
                    if !sub.is_empty() {
                        ui.label(RichText::new(sub).weak().small());
                    }
                    false
                })
                .inner;
            if fav {
                icons::icon(ui, Icon::Star, 12.0, theme::ACCENT);
            }
            if hovered {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Trash, 13.0, true, false, "Delete entry").clicked() {
                        action = Some(PwRowAction::Delete);
                    }
                    if icons::button(ui, Icon::Edit, 13.0, true, false, "Edit entry").clicked() {
                        action = Some(PwRowAction::Edit);
                    }
                });
            }
            if clicked {
                action = Some(PwRowAction::Select);
            }
        });
        action
    }

    fn pw_detail_ui(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.pw_sel else {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                icons::icon(ui, Icon::Passwords, 30.0, ui.visuals().weak_text_color());
                ui.label(RichText::new("Select an entry or create a new one").weak());
                if icons::button(ui, Icon::Add, 22.0, true, false, "New entry").clicked() {
                    self.pw_new();
                }
            });
            return;
        };
        let Some(entry) = self.passwords.get(id).cloned() else {
            self.pw_sel = None;
            return;
        };
        let revealed = self.pw_reveal.contains(&id);
        let mut act = PwActions::default();
        let now = passwords::now_unix();
        let fav_tex = self.favicon_tex.get(entry.favicon.trim()).cloned();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if let Some(t) = &fav_tex {
                        draw_tex(ui, t, 24.0);
                    } else if !entry.icon.trim().is_empty() {
                        ui.label(RichText::new(entry.icon.clone()).size(24.0));
                    }
                    ui.label(
                        RichText::new(if entry.title.trim().is_empty() {
                            "(untitled)"
                        } else {
                            &entry.title
                        })
                        .size(22.0)
                        .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Trash, 18.0, true, false, "Delete").clicked() {
                            act.del = true;
                        }
                        if icons::button(ui, Icon::Edit, 18.0, true, false, "Edit").clicked() {
                            act.edit = true;
                        }
                        if icons::button(ui, Icon::Star, 18.0, true, entry.favorite, "Favorite")
                            .clicked()
                        {
                            act.fav = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if !entry.category.trim().is_empty() {
                        pw_chip(ui, &entry.category);
                    }
                    ui.label(
                        RichText::new(format!("Updated {}", passwords::fmt_day(entry.updated)))
                            .weak()
                            .small(),
                    );
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                if pw_copy_row(ui, "Username", &entry.username) {
                    act.copy = Some(("username", entry.username.clone()));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Password").small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(
                            ui,
                            Icon::Copy,
                            15.0,
                            !entry.password.is_empty(),
                            false,
                            "Copy password",
                        )
                        .clicked()
                        {
                            act.copy = Some(("password", entry.password.clone()));
                        }
                        if icons::button(
                            ui,
                            if revealed { Icon::EyeOff } else { Icon::Eye },
                            15.0,
                            !entry.password.is_empty(),
                            revealed,
                            "Show / hide",
                        )
                        .clicked()
                        {
                            act.toggle_reveal = true;
                        }
                    });
                });
                let shown = if entry.password.is_empty() {
                    "—".to_string()
                } else if revealed {
                    entry.password.clone()
                } else {
                    "•".repeat(entry.password.chars().count().min(32))
                };
                ui.label(RichText::new(shown).monospace());
                if !entry.password.is_empty() {
                    let (score, label) = passwords::strength(&entry.password);
                    pw_strength_bar(ui, score, label, passwords::entropy_bits(&entry.password));
                }

                if !entry.totp.trim().is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Two-factor code").small().weak());
                    match passwords::totp(&entry.totp, now) {
                        Some(code) => {
                            ui.horizontal(|ui| {
                                let spaced: String = code
                                    .chars()
                                    .enumerate()
                                    .flat_map(|(i, c)| {
                                        if i > 0 && i % 3 == 0 {
                                            vec![' ', c]
                                        } else {
                                            vec![c]
                                        }
                                    })
                                    .collect();
                                ui.label(RichText::new(spaced).size(20.0).monospace().strong());
                                if icons::button(ui, Icon::Copy, 15.0, true, false, "Copy code")
                                    .clicked()
                                {
                                    act.copy = Some(("code", code.clone()));
                                }
                                ui.weak(format!("{}s", passwords::totp_remaining(now)));
                            });
                            if let Some(next) = passwords::totp(&entry.totp, now + 30) {
                                let spaced: String = next
                                    .chars()
                                    .enumerate()
                                    .flat_map(|(i, c)| {
                                        if i > 0 && i % 3 == 0 {
                                            vec![' ', c]
                                        } else {
                                            vec![c]
                                        }
                                    })
                                    .collect();
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("Next").small().weak());
                                    ui.label(RichText::new(spaced).monospace().weak());
                                });
                            }
                        }
                        None => {
                            ui.label(RichText::new("Invalid TOTP secret").color(theme::DANGER));
                        }
                    }
                }

                if !entry.url.trim().is_empty() {
                    ui.add_space(8.0);
                    if pw_copy_row(ui, "Website", &entry.url) {
                        act.copy = Some(("URL", entry.url.clone()));
                    }
                    if ui
                        .link(RichText::new("Open in browser").color(theme::LINK))
                        .clicked()
                    {
                        act.open_url = Some(entry.url.clone());
                    }
                }

                if !entry.notes.trim().is_empty() {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Notes").small().weak());
                    ui.label(&entry.notes);
                }

                ui.add_space(16.0);
            });

        // Apply actions.
        if act.toggle_reveal {
            if revealed {
                self.pw_reveal.remove(&id);
            } else {
                self.pw_reveal.insert(id);
            }
        }
        if act.fav {
            if let Some(e) = self.passwords.get_mut(id) {
                e.favorite = !e.favorite;
                e.updated = now;
            }
            self.save_passwords();
        }
        if act.edit {
            self.favicon_url = entry.favicon.clone();
            self.favicon_error = None;
            self.pw_editor = Some(PwEditor {
                draft: entry.clone(),
                is_new: false,
                show: revealed,
                new_folder: false,
                new_folder_name: String::new(),
            });
        }
        if act.del {
            self.pw_confirm_delete = Some(id);
        }
        if let Some(url) = act.open_url {
            open_external(&url);
        }
        if let Some((what, value)) = act.copy {
            ui.ctx().copy_text(value);
            self.status = format!("Copied {what}");
        }

        // A live TOTP countdown needs a repaint each second.
        if !entry.totp.trim().is_empty() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    /// Delete-confirmation dialog (any entry, not just the selected one).
    fn pw_delete_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(id) = self.pw_confirm_delete else {
            return;
        };
        let mut open = true;
        let mut confirm = false;
        egui::Window::new("pw_delete")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Warning, 18.0, theme::DANGER);
                    ui.label(RichText::new("Delete this entry?").strong());
                });
                ui.add_space(4.0);
                ui.weak("This cannot be undone.");
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                        open = false;
                    }
                    if icons::button(ui, Icon::Trash, 18.0, true, false, "Delete entry").clicked() {
                        confirm = true;
                    }
                });
            });
        if confirm {
            self.passwords.remove(id);
            self.pw_reveal.remove(&id);
            if self.pw_sel == Some(id) {
                self.pw_sel = None;
            }
            self.pw_confirm_delete = None;
            self.save_passwords();
            self.status = "Entry deleted".into();
        } else if !open {
            self.pw_confirm_delete = None;
        }
    }

    fn pw_editor_ui(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.pw_editor.take() else {
            return;
        };
        let mut keep = true;
        let mut save = false;
        let mut cancel = false;
        let mut use_gen = false;
        let mut force_favicon = false;

        egui::Window::new("pw_editor")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Passwords, 16.0, theme::ACCENT);
                    ui.label(
                        RichText::new(if editor.is_new {
                            "New password"
                        } else {
                            "Edit password"
                        })
                        .strong(),
                    );
                });
                ui.separator();
                pw_field(ui, "Title", &mut editor.draft.title);
                ui.label(RichText::new("Icon").small().weak());
                ui.horizontal_wrapped(|ui| {
                    for ic in PW_ICONS {
                        let sel = editor.draft.icon == *ic;
                        if ui
                            .selectable_label(sel, RichText::new(*ic).size(18.0))
                            .clicked()
                        {
                            editor.draft.icon = if sel {
                                String::new()
                            } else {
                                (*ic).to_string()
                            };
                        }
                    }
                    if !editor.draft.icon.is_empty()
                        && icons::button(ui, Icon::Close, 14.0, true, false, "Clear icon").clicked()
                    {
                        editor.draft.icon.clear();
                    }
                });
                ui.label(
                    RichText::new("Icon from a website (paste a favicon image URL)")
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 30.0;
                    ui.add(
                        egui::TextEdit::singleline(&mut self.favicon_url)
                            .hint_text("https://youtube.com/favicon.ico")
                            .desired_width(w),
                    );
                    if icons::button(ui, Icon::Refresh, 16.0, true, false, "Fetch favicon")
                        .clicked()
                    {
                        force_favicon = true;
                    }
                });
                let fav_url = self.favicon_url.trim().to_string();
                if let Some(tex) = self.favicon_tex.get(&fav_url).cloned() {
                    ui.horizontal(|ui| {
                        draw_tex(ui, &tex, 20.0);
                        ui.label(RichText::new("Favicon loaded").weak().small());
                    });
                } else if !fav_url.is_empty()
                    && self.favicon_last == fav_url
                    && self.favicon_rx.is_some()
                {
                    ui.label(RichText::new("Fetching favicon…").weak().small());
                } else if let Some(err) = &self.favicon_error {
                    ui.label(RichText::new(err.clone()).color(theme::DANGER).small());
                }
                ui.label(RichText::new("Username").small().weak());
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 30.0;
                    ui.add(egui::TextEdit::singleline(&mut editor.draft.username).desired_width(w));
                    if icons::button(ui, Icon::Dice, 16.0, true, false, "Random username").clicked()
                    {
                        editor.draft.username = passwords::generate_username();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Password").small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Generate, 15.0, true, false, "Generate")
                            .clicked()
                        {
                            use_gen = true;
                        }
                        if icons::button(
                            ui,
                            if editor.show { Icon::EyeOff } else { Icon::Eye },
                            15.0,
                            true,
                            editor.show,
                            "Show / hide",
                        )
                        .clicked()
                        {
                            editor.show = !editor.show;
                        }
                    });
                });
                ui.add(
                    egui::TextEdit::singleline(&mut editor.draft.password)
                        .password(!editor.show)
                        .desired_width(f32::INFINITY),
                );
                let (score, label) = passwords::strength(&editor.draft.password);
                pw_strength_bar(
                    ui,
                    score,
                    label,
                    passwords::entropy_bits(&editor.draft.password),
                );
                pw_field(ui, "Website", &mut editor.draft.url);
                ui.label(RichText::new("Folder").small().weak());
                let folders = self.passwords.all_folders();
                egui::ComboBox::from_id_source("pw_folder")
                    .selected_text(if editor.draft.category.trim().is_empty() {
                        "(none)".to_string()
                    } else {
                        editor.draft.category.clone()
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut editor.draft.category, String::new(), "(none)");
                        for f in &folders {
                            ui.selectable_value(&mut editor.draft.category, f.clone(), f);
                        }
                        if ui.selectable_label(false, "＋ New folder…").clicked() {
                            editor.new_folder = true;
                        }
                    });
                if editor.new_folder {
                    ui.horizontal(|ui| {
                        let w = ui.available_width() - 30.0;
                        ui.add(
                            egui::TextEdit::singleline(&mut editor.new_folder_name)
                                .hint_text("New folder name")
                                .desired_width(w),
                        );
                        if icons::button(ui, Icon::Check, 16.0, true, false, "Create folder")
                            .clicked()
                        {
                            let name = editor.new_folder_name.trim().to_string();
                            if !name.is_empty() {
                                self.passwords.create_folder(&name);
                                editor.draft.category = name;
                                editor.new_folder_name.clear();
                                editor.new_folder = false;
                            }
                        }
                    });
                }
                pw_field(ui, "TOTP secret (base32)", &mut editor.draft.totp);
                ui.label(RichText::new("Notes").small().weak());
                ui.add(
                    egui::TextEdit::multiline(&mut editor.draft.notes)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if editor.draft.title.trim().is_empty() {
                    ui.label(RichText::new("Enter a title.").color(theme::DANGER).small());
                }
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Check, 22.0, true, false, "Save").clicked() {
                        save = true;
                    }
                    if icons::button(ui, Icon::Close, 22.0, true, false, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if force_favicon {
            let u = self.favicon_url.trim().to_string();
            if !u.is_empty() {
                self.favicon_tex.remove(&u);
                self.favicon_last.clear();
                self.start_favicon_fetch(&u);
            }
        }
        // The favicon URL is app state; write it back into the draft.
        editor.draft.favicon = self.favicon_url.trim().to_string();
        if use_gen {
            editor.draft.password = passwords::generate(
                self.pw_gen_len,
                self.pw_gen_upper,
                self.pw_gen_lower,
                self.pw_gen_digits,
                self.pw_gen_special,
            );
            editor.show = true;
        }
        if save {
            if editor.draft.title.trim().is_empty() {
                self.status = "Enter a title".into();
            } else {
                let now = passwords::now_unix();
                editor.draft.updated = now;
                if editor.is_new {
                    editor.draft.created = now;
                    let id = self.passwords.add(editor.draft.clone());
                    self.pw_sel = Some(id);
                } else {
                    let id = editor.draft.id;
                    if let Some(e) = self.passwords.get_mut(id) {
                        *e = editor.draft.clone();
                    }
                    self.pw_sel = Some(id);
                }
                self.save_passwords();
                let cat = editor.draft.category.trim().to_string();
                if !cat.is_empty() {
                    self.pw_expanded.insert(cat);
                }
                self.status = "Saved".into();
                keep = false;
            }
        }
        if cancel {
            keep = false;
        }
        if keep {
            self.pw_editor = Some(editor);
        }
    }

    fn pw_generator_ui(&mut self, ctx: &egui::Context) {
        if !self.pw_gen_open {
            return;
        }
        let mut open = true;
        let mut regen = false;
        let mut changed = false;
        let mut copy = false;
        egui::Window::new("pw_gen")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(340.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Generate, 16.0, theme::ACCENT);
                    ui.label(RichText::new("Password generator").strong());
                });
                ui.separator();
                changed |= ui
                    .add(egui::Slider::new(&mut self.pw_gen_len, 8..=128).text("Length"))
                    .changed();
                ui.horizontal_wrapped(|ui| {
                    changed |= ui.checkbox(&mut self.pw_gen_upper, "A-Z").changed();
                    changed |= ui.checkbox(&mut self.pw_gen_lower, "a-z").changed();
                    changed |= ui.checkbox(&mut self.pw_gen_digits, "0-9").changed();
                    changed |= ui.checkbox(&mut self.pw_gen_special, "!@#").changed();
                });
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.pw_gen_value)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
                let (score, label) = passwords::strength(&self.pw_gen_value);
                pw_strength_bar(
                    ui,
                    score,
                    label,
                    passwords::entropy_bits(&self.pw_gen_value),
                );
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, 16.0, true, false, "Close").clicked() {
                        open = false;
                    }
                    if icons::button(ui, Icon::Copy, 16.0, true, false, "Copy").clicked() {
                        copy = true;
                    }
                    if icons::button(ui, Icon::Refresh, 16.0, true, false, "Regenerate").clicked() {
                        regen = true;
                    }
                });
            });
        if changed || regen {
            self.pw_gen_value = passwords::generate(
                self.pw_gen_len,
                self.pw_gen_upper,
                self.pw_gen_lower,
                self.pw_gen_digits,
                self.pw_gen_special,
            );
        }
        if copy {
            ctx.copy_text(self.pw_gen_value.clone());
            self.status = "Copied generated password".into();
        }
        if !open {
            self.pw_gen_open = false;
        }
    }

    /// Import / export the password store as JSON.
    fn pw_io_ui(&mut self, ctx: &egui::Context) {
        if !self.pw_io_open {
            return;
        }
        let mut open = true;
        let mut do_export = false;
        let mut do_import = false;
        egui::Window::new("pw_io")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Upload, 16.0, theme::ACCENT);
                    ui.label(RichText::new("Encrypted import / export").strong());
                });
                ui.separator();
                ui.label(RichText::new("File").small().weak());
                ui.add(
                    egui::TextEdit::singleline(&mut self.pw_io_path)
                        .hint_text("~/passwords-export.json")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Password (encrypts the export / unlocks an encrypted import)")
                        .small()
                        .weak(),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.pw_io_password)
                        .password(true)
                        .hint_text("16+ chars, mixed case, a number and a symbol")
                        .desired_width(f32::INFINITY),
                );
                if !self.pw_io_password.is_empty() {
                    ui.add_space(4.0);
                    policy_checklist(ui, &self.pw_io_password);
                }
                if let Some(err) = &self.pw_io_error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(err.clone()).color(theme::DANGER).small());
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Export writes the store encrypted (AES-256-GCM with an Argon2id key from the password). Import merges entries from an encrypted backup, plaintext JSON, a JSON array, a { \"Title\": \"secret\" } map, or an otpauth:// URI.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, 16.0, true, false, "Close").clicked() {
                        open = false;
                    }
                    if icons::button(ui, Icon::Download, 16.0, true, false, "Import from file").clicked() {
                        do_import = true;
                    }
                    if icons::button(ui, Icon::Upload, 16.0, true, false, "Export to file").clicked() {
                        do_export = true;
                    }
                });
            });
        if do_export {
            self.pw_export();
        }
        if do_import {
            self.pw_import();
        }
        if !open {
            self.pw_io_open = false;
        }
    }

    /// Create a new folder.
    fn pw_new_folder_ui(&mut self, ctx: &egui::Context) {
        if !self.pw_new_folder_open {
            return;
        }
        let mut open = true;
        let mut create = false;
        egui::Window::new("pw_new_folder")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(320.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::FolderPlus, 16.0, theme::ACCENT);
                    ui.label(RichText::new("New folder").strong());
                });
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(&mut self.pw_new_folder_name)
                        .hint_text("Folder name")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                        open = false;
                    }
                    if icons::button(
                        ui,
                        Icon::Check,
                        18.0,
                        !self.pw_new_folder_name.trim().is_empty(),
                        false,
                        "Create folder",
                    )
                    .clicked()
                    {
                        create = true;
                    }
                });
            });
        if create {
            let name = self.pw_new_folder_name.trim().to_string();
            if !name.is_empty() {
                if self.passwords.create_folder(&name) {
                    self.status = format!("Folder \"{name}\" created");
                }
                self.pw_category = Some(name.clone());
                self.pw_expanded.insert(name);
                self.save_passwords();
                self.pw_new_folder_open = false;
                self.pw_new_folder_name.clear();
            }
        }
        if !open {
            self.pw_new_folder_open = false;
        }
    }

    /// Rename / delete folder dialogs.
    fn pw_folder_dialog_ui(&mut self, ctx: &egui::Context) {
        if let Some(old) = self.pw_folder_rename.clone() {
            let mut open = true;
            let mut apply = false;
            egui::Window::new("pw_folder_rename")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(320.0);
                    ui.horizontal(|ui| {
                        icons::icon(ui, Icon::Edit, 16.0, theme::ACCENT);
                        ui.label(RichText::new("Rename folder").strong());
                    });
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.pw_folder_rename_name)
                            .hint_text("Folder name")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                            open = false;
                        }
                        if icons::button(
                            ui,
                            Icon::Check,
                            18.0,
                            !self.pw_folder_rename_name.trim().is_empty(),
                            false,
                            "Rename",
                        )
                        .clicked()
                        {
                            apply = true;
                        }
                    });
                });
            if apply {
                let new = self.pw_folder_rename_name.trim().to_string();
                if self.passwords.rename_folder(&old, &new) {
                    if self.pw_expanded.remove(&old) {
                        self.pw_expanded.insert(new.clone());
                    }
                    if self.pw_category.as_deref() == Some(old.as_str()) {
                        self.pw_category = Some(new.clone());
                    }
                    self.save_passwords();
                    self.status = format!("Renamed folder to \"{new}\"");
                } else {
                    self.status = "Could not rename (empty or already exists)".into();
                }
                self.pw_folder_rename = None;
            } else if !open {
                self.pw_folder_rename = None;
            }
        }

        if let Some(name) = self.pw_folder_delete.clone() {
            let mut open = true;
            let mut confirm = false;
            egui::Window::new("pw_folder_delete")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(340.0);
                    ui.horizontal(|ui| {
                        icons::icon(ui, Icon::Warning, 18.0, theme::DANGER);
                        ui.label(RichText::new(format!("Delete folder \"{name}\"?")).strong());
                    });
                    ui.add_space(4.0);
                    ui.weak("Its entries move to Uncategorized; no entries are deleted.");
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                            open = false;
                        }
                        if icons::button(ui, Icon::Trash, 18.0, true, false, "Delete folder")
                            .clicked()
                        {
                            confirm = true;
                        }
                    });
                });
            if confirm {
                self.passwords.delete_folder(&name);
                self.pw_expanded.remove(&name);
                if self.pw_category.as_deref() == Some(name.as_str()) {
                    self.pw_category = None;
                }
                self.save_passwords();
                self.status = format!("Deleted folder \"{name}\"");
                self.pw_folder_delete = None;
            } else if !open {
                self.pw_folder_delete = None;
            }
        }
    }

    /// Encrypt the password store under the dialog's password and write it.
    fn pw_export(&mut self) {
        let path = expand_tilde(&self.pw_io_path);
        if path.as_os_str().is_empty() {
            self.pw_io_error = Some("Enter a file path".into());
            return;
        }
        let problems = Keyfile::password_problems(&self.pw_io_password);
        if !problems.is_empty() {
            self.pw_io_error = Some(format!("Export password: {}", problems.join(", ")));
            return;
        }
        let json = match serde_json::to_string_pretty(&self.passwords) {
            Ok(j) => j,
            Err(e) => {
                self.pw_io_error = Some(format!("Export failed: {e}"));
                return;
            }
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let blob = crypto::encrypt_with_password(&self.pw_io_password, json.as_bytes());
        match std::fs::write(&path, blob) {
            Ok(()) => {
                restrict_permissions(&path);
                self.pw_io_error = None;
                self.status = format!(
                    "Exported {} entr{} (encrypted) to {}",
                    self.passwords.entries.len(),
                    if self.passwords.entries.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    path.display()
                );
            }
            Err(e) => self.pw_io_error = Some(format!("Export failed: {e}")),
        }
    }

    /// Merge entries from the chosen file (decrypting it first if needed).
    fn pw_import(&mut self) {
        let path = expand_tilde(&self.pw_io_path);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.pw_io_error = Some(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let text = if crypto::is_password_encrypted(&bytes) {
            if self.pw_io_password.is_empty() {
                self.pw_io_error = Some("This backup is encrypted — enter its password".into());
                return;
            }
            match crypto::decrypt_with_password(&self.pw_io_password, &bytes) {
                Some(pt) => match String::from_utf8(pt) {
                    Ok(s) => s,
                    Err(_) => {
                        self.pw_io_error = Some("Decrypted data is not text".into());
                        return;
                    }
                },
                None => {
                    self.pw_io_error = Some("Wrong password (or the file is damaged)".into());
                    return;
                }
            }
        } else {
            match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    self.pw_io_error = Some("Not a text/JSON file".into());
                    return;
                }
            }
        };
        match passwords::parse_import(&text) {
            Ok(entries) => {
                let n = entries.len();
                let now = passwords::now_unix();
                let mut first = None;
                for mut e in entries {
                    e.id = 0;
                    if e.created == 0 {
                        e.created = now;
                    }
                    e.updated = now;
                    if !e.category.trim().is_empty() {
                        self.passwords.create_folder(&e.category);
                    }
                    let id = self.passwords.add(e);
                    if first.is_none() {
                        first = Some(id);
                    }
                }
                self.save_passwords();
                if let Some(id) = first {
                    self.pw_sel = Some(id);
                }
                self.pw_io_error = None;
                self.status = format!("Imported {n} entr{}", if n == 1 { "y" } else { "ies" });
            }
            Err(e) => self.pw_io_error = Some(format!("Import failed: {e}")),
        }
    }

    // -------------------------------------------------------------- UI parts

    fn top_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style())
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(3.0, 6.0));
        egui::SidePanel::left("tool_bar")
            .frame(frame)
            .exact_width(56.0)
            .resizable(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            // Core actions — larger icons, stacked top → bottom.
                            if icons::button(ui, Icon::FolderOpen, 26.0, true, false, "Open vault")
                                .clicked()
                            {
                                self.open_picker();
                            }
                            if icons::button(ui, Icon::Add, 26.0, true, false, "New note").clicked()
                            {
                                self.new_note_open = true;
                                self.new_note_error = None;
                                self.new_note_name.clear();
                            }
                            if icons::button(
                                ui,
                                Icon::Save,
                                26.0,
                                !self.tabs.is_empty(),
                                false,
                                "Save",
                            )
                            .clicked()
                            {
                                self.save_active();
                            }
                            if icons::button(ui, Icon::Lock, 26.0, true, false, "Lock now")
                                .clicked()
                            {
                                self.lock();
                            }
                        });

                        // View toggles pinned to the bottom-left.
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                            let view = self.tabs.get(self.active).map(|t| t.view);
                            let enabled = view.is_some();
                            if icons::button(
                                ui,
                                Icon::PanelRight,
                                18.0,
                                true,
                                self.show_right,
                                "Toggle side panel",
                            )
                            .clicked()
                            {
                                self.show_right = !self.show_right;
                            }
                            ui.separator();
                            if icons::button(
                                ui,
                                Icon::Edit,
                                18.0,
                                enabled,
                                view == Some(ViewMode::Edit),
                                "Edit",
                            )
                            .clicked()
                            {
                                self.set_view(ViewMode::Edit);
                            }
                            if icons::button(
                                ui,
                                Icon::Split,
                                18.0,
                                enabled,
                                view == Some(ViewMode::Split),
                                "Split view",
                            )
                            .clicked()
                            {
                                self.set_view(ViewMode::Split);
                            }
                            if icons::button(
                                ui,
                                Icon::Preview,
                                18.0,
                                enabled,
                                view == Some(ViewMode::Preview),
                                "Preview",
                            )
                            .clicked()
                            {
                                self.set_view(ViewMode::Preview);
                            }
                            if icons::button(
                                ui,
                                Icon::Live,
                                18.0,
                                enabled,
                                view == Some(ViewMode::Live),
                                "Live preview",
                            )
                            .clicked()
                            {
                                self.set_view(ViewMode::Live);
                            }
                        });
                    });
            });
    }

    /// Set the active tab's view mode.
    fn set_view(&mut self, view: ViewMode) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.view = view;
        }
    }

    fn open_picker(&mut self) {
        let start = self
            .vault
            .as_ref()
            .map(|v| v.root.clone())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        self.picker = Some(DirPicker::new(start));
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style())
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(10.0, 10.0));
        egui::SidePanel::left("left_panel")
            .frame(frame)
            .resizable(true)
            .default_width(260.0)
            .width_range(180.0..=420.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    icons::icon(ui, Icon::Search, 15.0, ui.visuals().weak_text_color());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Search notes…")
                            .desired_width(f32::INFINITY),
                    );
                    if self.focus_search {
                        resp.request_focus();
                        self.focus_search = false;
                    }
                    if resp.changed() {
                        self.show_search_results = !self.search.trim().is_empty();
                    }
                });

                if self.show_search_results {
                    ui.separator();
                    self.search_results_ui(ui);
                    return;
                }

                ui.separator();
                if let Some(tag) = self.tag_filter.clone() {
                    // Tag-filtered view.
                    ui.horizontal(|ui| {
                        icons::icon(ui, Icon::Tag, 15.0, theme::ACCENT);
                        ui.label(RichText::new(format!("#{tag}")).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::button(ui, Icon::Close, 15.0, true, false, "Clear tag filter")
                                .clicked()
                            {
                                self.tag_filter = None;
                            }
                        });
                    });
                    let paths = self.index.notes_with_tag(&tag);
                    let selected = self.tabs.get(self.active).map(|t| t.path.clone());
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if paths.is_empty() {
                                ui.add_space(6.0);
                                ui.weak("No notes with this tag");
                            }
                            for p in paths {
                                let title = self.title_of(&p);
                                let is_sel = selected.as_deref() == Some(p.as_path());
                                let clicked = ui
                                    .horizontal(|ui| {
                                        icons::icon(
                                            ui,
                                            Icon::File,
                                            14.0,
                                            ui.visuals().weak_text_color(),
                                        );
                                        ui.selectable_label(is_sel, RichText::new(title)).clicked()
                                    })
                                    .inner;
                                if clicked {
                                    self.pending_open = Some(p.clone());
                                }
                            }
                        });
                } else {
                    ui.horizontal(|ui| {
                        icons::icon(ui, Icon::Folder, 15.0, theme::ACCENT);
                        ui.label(RichText::new("Vault").strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::button(ui, Icon::Add, 16.0, true, false, "New note").clicked()
                            {
                                self.new_note_open = true;
                                self.new_note_error = None;
                                self.new_note_name.clear();
                            }
                        });
                    });

                    let tree = self.tree.clone();
                    let selected = self.tabs.get(self.active).map(|t| t.path.clone());
                    let titles = self.titles.clone();
                    let mut actions = TreeActions::default();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if self.vault.is_none() {
                                ui.add_space(8.0);
                                ui.weak("No vault open. Use the folder icon in the toolbar.");
                                return;
                            }
                            show_tree(
                                ui,
                                &tree,
                                selected.as_deref(),
                                &mut self.expanded_dirs,
                                &titles,
                                &mut actions,
                            );
                        });
                    if actions.open.is_some() {
                        self.pending_open = actions.open;
                    }
                    if let Some(pos) = actions.pos {
                        self.dialog_pos = Some(pos);
                    }
                    if let Some(path) = actions.rename {
                        self.rename_name = path
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        self.rename_target = Some(path);
                    }
                    if let Some(path) = actions.subnote {
                        self.subnote_parent = Some(path);
                        self.subnote_name.clear();
                        self.subnote_open = true;
                        self.subnote_error = None;
                    }
                    if actions.delete.is_some() {
                        self.confirm_delete = actions.delete;
                    }
                }
            });
    }

    fn search_results_ui(&mut self, ui: &mut egui::Ui) {
        let q = search::Query::parse(&self.search);
        let mut matches: Vec<(PathBuf, String, String)> = Vec::new();
        {
            let titles = &self.titles;
            let metas = &self.metas;
            let files: Vec<PathBuf> = self
                .vault
                .as_ref()
                .map(|v| v.notes.iter().map(|n| n.path.clone()).collect())
                .unwrap_or_default();
            for path in &files {
                let title = titles.get(path).cloned().unwrap_or_else(|| {
                    path.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                });
                let meta = metas.get(path);
                let tags: &[String] = meta.map(|m| m.tags.as_slice()).unwrap_or(&[]);
                let body = meta.map(|m| m.body.as_str()).unwrap_or("");
                let path_str = path.to_string_lossy().to_string();
                let doc = search::Doc {
                    title: &title,
                    path: &path_str,
                    tags,
                    content: body,
                };
                if q.matches(&doc) {
                    let snippet = q.snippet(&doc).map(|(_, l)| l).unwrap_or_default();
                    matches.push((path.clone(), title, snippet));
                }
            }
        }

        ui.label(
            RichText::new(format!("{} result(s)", matches.len()))
                .weak()
                .small(),
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (path, title, snippet) in matches {
                    let clicked = ui
                        .horizontal(|ui| {
                            icons::icon(ui, Icon::File, 14.0, ui.visuals().weak_text_color());
                            ui.add(egui::Button::new(RichText::new(&title).strong()).frame(false))
                                .clicked()
                        })
                        .inner;
                    if clicked {
                        self.pending_open = Some(path.clone());
                        self.show_search_results = false;
                    }
                    if !snippet.is_empty() {
                        ui.label(RichText::new(snippet).weak().small());
                    }
                    ui.separator();
                }
            });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        if !self.show_right {
            return;
        }
        let frame = egui::Frame::side_top_panel(&ctx.style())
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(12.0, 12.0));
        egui::SidePanel::right("right_panel")
            .frame(frame)
            .resizable(true)
            .default_width(280.0)
            .width_range(200.0..=460.0)
            .show(ctx, |ui| {
                // Quick actions, above the calendar (left → right).
                ui.horizontal(|ui| {
                    if icons::button(ui, Icon::Refresh, 16.0, true, false, "Refresh vault")
                        .clicked()
                    {
                        self.refresh_vault();
                    }
                    if icons::button(
                        ui,
                        Icon::Search,
                        16.0,
                        true,
                        false,
                        "Command palette (Ctrl+P)",
                    )
                    .clicked()
                    {
                        self.open_palette();
                    }
                    if icons::button(ui, Icon::Graph, 16.0, true, self.show_graph, "Graph view")
                        .clicked()
                    {
                        self.show_graph = !self.show_graph;
                    }
                    if icons::button(ui, Icon::Rss, 16.0, true, self.feeds_open, "RSS feeds")
                        .clicked()
                    {
                        self.feeds_open = !self.feeds_open;
                    }
                    if icons::button(
                        ui,
                        Icon::Passwords,
                        16.0,
                        true,
                        self.passwords_open,
                        "Password manager",
                    )
                    .clicked()
                    {
                        self.toggle_passwords();
                    }
                });
                ui.separator();

                // Calendar (daily notes)
                egui::CollapsingHeader::new("Calendar")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if icons::button(
                                ui,
                                Icon::ChevronLeft,
                                16.0,
                                true,
                                false,
                                "Previous month",
                            )
                            .clicked()
                            {
                                let (y, m) =
                                    crate::dates::add_months(self.cal_year, self.cal_month, -1);
                                self.cal_year = y;
                                self.cal_month = m;
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{} {}",
                                    crate::dates::month_name(self.cal_month),
                                    self.cal_year
                                ))
                                .strong(),
                            );
                            if icons::button(
                                ui,
                                Icon::ChevronRight,
                                16.0,
                                true,
                                false,
                                "Next month",
                            )
                            .clicked()
                            {
                                let (y, m) =
                                    crate::dates::add_months(self.cal_year, self.cal_month, 1);
                                self.cal_year = y;
                                self.cal_month = m;
                            }
                            if icons::button(ui, Icon::Today, 16.0, true, false, "Today").clicked()
                            {
                                let (y, m, _) = crate::dates::today();
                                self.cal_year = y;
                                self.cal_month = m;
                            }
                        });
                        ui.horizontal(|ui| {
                            for w in crate::dates::WEEKDAY_SHORT {
                                ui.add_sized(
                                    [22.0, 14.0],
                                    egui::Label::new(RichText::new(w).small().weak()),
                                );
                            }
                        });
                        let (ty, tm, td) = crate::dates::today();
                        let dir = self.config.daily_dir.clone();
                        let fmt = self.config.daily_format.clone();
                        let root = self.vault.as_ref().map(|v| v.root.clone());
                        let grid = crate::dates::month_grid(self.cal_year, self.cal_month);
                        let mut pick: Option<u32> = None;
                        for week in grid {
                            ui.horizontal(|ui| {
                                for cell in week {
                                    match cell {
                                        None => {
                                            ui.add_sized([22.0, 22.0], egui::Label::new(""));
                                        }
                                        Some(day) => {
                                            let exists = root
                                                .as_ref()
                                                .map(|r| {
                                                    let mut p = r.clone();
                                                    if !dir.trim().is_empty() {
                                                        p.push(dir.trim());
                                                    }
                                                    p.join(format!(
                                                        "{}.md",
                                                        crate::dates::format(
                                                            &fmt,
                                                            self.cal_year,
                                                            self.cal_month,
                                                            day
                                                        )
                                                    ))
                                                    .exists()
                                                })
                                                .unwrap_or(false);
                                            let is_today = (ty, tm, td)
                                                == (self.cal_year, self.cal_month, day);
                                            let mut rt = RichText::new(day.to_string());
                                            if exists {
                                                rt = rt.color(theme::ACCENT).strong();
                                            } else if is_today {
                                                rt = rt.underline();
                                            }
                                            let resp = ui.add_sized(
                                                [22.0, 22.0],
                                                egui::Button::new(rt).frame(false),
                                            );
                                            let resp = if is_today {
                                                resp.on_hover_text("Today")
                                            } else {
                                                resp
                                            };
                                            if resp.clicked() {
                                                pick = Some(day);
                                            }
                                        }
                                    }
                                }
                            });
                        }
                        if let Some(day) = pick {
                            self.pending_daily = Some((self.cal_year, self.cal_month, day));
                        }
                    });
                ui.separator();

                let Some(active) = self.tabs.get(self.active) else {
                    ui.add_space(8.0);
                    ui.weak("Open a note to see its outline and links.");
                    return;
                };
                let text = active.text.clone();
                let title = active.title.clone();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&title).strong().size(16.0));
                        ui.separator();

                        // Tags (vault-wide)
                        egui::CollapsingHeader::new("Tags")
                            .default_open(false)
                            .show(ui, |ui| {
                                let tags = self.index.all_tags();
                                if tags.is_empty() {
                                    ui.weak("No tags yet");
                                }
                                for (tag, count) in tags {
                                    let color = ui.visuals().hyperlink_color;
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(ui, Icon::Tag, 13.0, color);
                                            ui.add(
                                                egui::Button::new(
                                                    RichText::new(format!("#{tag}  ({count})"))
                                                        .color(color),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        self.tag_filter = Some(tag.clone());
                                        self.show_search_results = false;
                                        self.search.clear();
                                    }
                                }
                            });

                        // Outline
                        egui::CollapsingHeader::new("Outline")
                            .default_open(true)
                            .show(ui, |ui| {
                                let heads = markdown::headings(&text);
                                if heads.is_empty() {
                                    ui.weak("No headings");
                                }
                                for (level, heading) in heads {
                                    let indent = ((level.saturating_sub(1)) as f32) * 10.0;
                                    ui.horizontal(|ui| {
                                        ui.add_space(indent);
                                        ui.label(RichText::new(&heading).small());
                                    });
                                }
                            });

                        // Outgoing links
                        egui::CollapsingHeader::new("Outgoing links")
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut links = markdown::wikilinks(&text);
                                links.sort();
                                links.dedup();
                                if links.is_empty() {
                                    ui.weak("No [[wikilinks]]");
                                }
                                for link in links {
                                    let resolved = self.resolve_link(&link).is_some();
                                    let color = if resolved {
                                        ui.visuals().hyperlink_color
                                    } else {
                                        ui.visuals().warn_fg_color
                                    };
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(ui, Icon::Link, 14.0, color);
                                            ui.add(
                                                egui::Button::new(
                                                    RichText::new(&link).color(color),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        if let Some(path) = self.resolve_link(&link) {
                                            self.pending_open = Some(path);
                                        } else {
                                            self.status = format!("No note named '{link}'");
                                        }
                                    }
                                }
                            });

                        // Backlinks
                        let count = self.backlinks.len();
                        egui::CollapsingHeader::new(format!("Backlinks ({count})"))
                            .default_open(true)
                            .show(ui, |ui| {
                                if self.backlinks.is_empty() {
                                    ui.weak("No notes link here yet");
                                }
                                let links = self.backlinks.clone();
                                for (path, line_no, snippet) in links {
                                    let name = path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let link_color = ui.visuals().hyperlink_color;
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(ui, Icon::Backlink, 14.0, link_color);
                                            ui.add(
                                                egui::Button::new(
                                                    RichText::new(format!("{name} : {line_no}"))
                                                        .color(link_color),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        self.pending_open = Some(path);
                                    }
                                    ui.label(RichText::new(snippet).weak().small());
                                }
                            });

                        // Unlinked mentions
                        let ucount = self.unlinked.len();
                        egui::CollapsingHeader::new(format!("Unlinked mentions ({ucount})"))
                            .default_open(false)
                            .show(ui, |ui| {
                                if self.unlinked.is_empty() {
                                    ui.weak("No unlinked mentions");
                                }
                                let links = self.unlinked.clone();
                                for (path, line_no, snippet) in links {
                                    let name = path
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let color = ui.visuals().hyperlink_color;
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(
                                                ui,
                                                Icon::Search,
                                                14.0,
                                                ui.visuals().weak_text_color(),
                                            );
                                            ui.add(
                                                egui::Button::new(
                                                    RichText::new(format!("{name} : {line_no}"))
                                                        .color(color),
                                                )
                                                .frame(false),
                                            )
                                            .clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        self.pending_open = Some(path);
                                    }
                                    ui.label(RichText::new(snippet).weak().small());
                                }
                            });
                    });
            });
    }

    fn central_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            // The password manager and RSS reader don't need an open vault.
            if self.passwords_open {
                self.passwords_ui(ui);
                return;
            }

            if self.vault.is_none() {
                self.welcome_ui(ui);
                return;
            }

            if self.feeds_open {
                self.feeds_ui(ui);
                return;
            }

            if self.show_graph {
                self.graph_ui(ui);
                return;
            }

            if self.canvas.is_some() {
                self.canvas_ui(ui);
                return;
            }

            // Tab strip.
            if self.tabs.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    icons::icon(ui, Icon::File, 26.0, ui.visuals().weak_text_color());
                    ui.label(RichText::new("Select a note from the sidebar").weak());
                    if icons::button(ui, Icon::Add, 22.0, true, false, "New note").clicked() {
                        self.new_note_open = true;
                        self.new_note_error = None;
                        self.new_note_name.clear();
                    }
                });
                return;
            }

            self.tab_strip(ui);

            // Attachment toolbar — above the editor, below the tabs.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Attach:").small().weak());
                if icons::button(ui, Icon::Image, 16.0, true, false, "Attach an image").clicked() {
                    self.open_attach_picker(AttachKind::Image);
                }
                if icons::button(
                    ui,
                    Icon::Mic,
                    16.0,
                    true,
                    false,
                    "Attach a voice recording / audio",
                )
                .clicked()
                {
                    self.open_attach_picker(AttachKind::Audio);
                }
                if icons::button(ui, Icon::Video, 16.0, true, false, "Attach a video").clicked() {
                    self.open_attach_picker(AttachKind::Video);
                }
                ui.label(
                    RichText::new("images, voice notes, videos — shown & played in the preview")
                        .small()
                        .weak(),
                );
            });

            let idx = self.active;
            if idx >= self.tabs.len() {
                return;
            }
            let view = self.tabs[idx].view;

            if view == ViewMode::Split {
                egui::SidePanel::right("preview_pane")
                    .resizable(true)
                    .default_width((ui.available_width() * 0.5).max(200.0))
                    .show_inside(ui, |ui| {
                        self.preview_ui(ui, idx);
                    });
            }

            match view {
                ViewMode::Edit | ViewMode::Split => self.editor_ui(ui, idx),
                ViewMode::Preview => self.preview_ui(ui, idx),
                ViewMode::Live => self.live_ui(ui, idx),
            }
        });
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut to_close: Option<usize> = None;
        let mut select: Option<usize> = None;
        // The selected tab is filled with a dark grey rather than the accent
        // (violet) selection colour, which reads as pink.
        ui.visuals_mut().selection.bg_fill = theme::WIDGET_ACTIVE;
        ui.visuals_mut().selection.stroke = egui::Stroke::NONE;
        ui.horizontal_wrapped(|ui| {
            let count = self.tabs.len();
            for i in 0..count {
                let dirty = self.tabs[i].dirty();
                let title = self.tabs[i].title.clone();
                let selected = i == self.active;
                if dirty {
                    icons::icon(ui, Icon::Dot, 8.0, theme::ACCENT);
                }
                if ui.selectable_label(selected, title).clicked() {
                    select = Some(i);
                }
                if icons::button(ui, Icon::Close, 12.0, true, false, "Close tab").clicked() {
                    to_close = Some(i);
                }
                ui.add_space(8.0);
            }
        });
        if let Some(i) = select {
            self.active = i;
        }
        if let Some(i) = to_close {
            self.close_tab(i);
        }
        ui.separator();
    }

    /// Render the force-directed graph view of notes and their links, with
    /// local-graph and tag/orphan filters.
    fn graph_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            icons::icon(ui, Icon::Graph, 16.0, theme::ACCENT);
            ui.label(RichText::new("Graph").strong());
            ui.separator();
            if icons::button(
                ui,
                Icon::Dot,
                14.0,
                true,
                self.graph_local,
                "Local graph (near the active note)",
            )
            .clicked()
            {
                self.graph_local = !self.graph_local;
            }
            if self.graph_local {
                if icons::button(ui, Icon::Close, 12.0, true, false, "Shallower").clicked() {
                    self.graph_depth = self.graph_depth.saturating_sub(1).max(1);
                }
                ui.label(RichText::new(format!("depth {}", self.graph_depth)).small());
                if icons::button(ui, Icon::Add, 12.0, true, false, "Deeper").clicked() {
                    self.graph_depth = (self.graph_depth + 1).min(6);
                }
            }
            if icons::button(
                ui,
                Icon::File,
                14.0,
                true,
                self.graph_show_orphans,
                "Show notes with no links",
            )
            .clicked()
            {
                self.graph_show_orphans = !self.graph_show_orphans;
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.graph_tag)
                    .hint_text("tag filter")
                    .desired_width(120.0),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Close, 16.0, true, false, "Close graph").clicked() {
                    self.show_graph = false;
                }
                if icons::button(ui, Icon::Refresh, 14.0, true, false, "Reset view").clicked() {
                    self.graph_zoom = 1.0;
                    self.graph_pan = egui::Vec2::ZERO;
                }
            });
        });
        ui.separator();
        if self.graph_nodes.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.weak("No notes to graph"));
            return;
        }
        let rect = ui.available_rect_before_wrap();
        let base = rect.width().min(rect.height()) * 0.45;
        let line = ui.visuals().hyperlink_color;
        let weak = ui.visuals().weak_text_color();
        let nodes = self.graph_nodes.clone();
        let pos = self.graph_pos.clone();
        let edges = self.graph_edges.clone();
        let active = self.tabs.get(self.active).map(|t| t.path.clone());

        let n = nodes.len();
        let active_idx = active
            .as_ref()
            .and_then(|a| nodes.iter().position(|p| p == a));
        let local_mask = if self.graph_local {
            active_idx
                .map(|idx| graph::neighborhood(n, &edges, idx, self.graph_depth))
                .unwrap_or_else(|| vec![true; n])
        } else {
            vec![true; n]
        };
        let tag = self.graph_tag.trim().to_ascii_lowercase();
        let tag_mask: Vec<bool> = nodes
            .iter()
            .map(|p| {
                if tag.is_empty() {
                    true
                } else {
                    self.metas
                        .get(p)
                        .map(|m| {
                            m.tags
                                .iter()
                                .any(|t| t.to_ascii_lowercase().starts_with(&tag))
                        })
                        .unwrap_or(false)
                }
            })
            .collect();
        let mut degree = vec![0usize; n];
        for (a, b) in &edges {
            degree[*a] += 1;
            degree[*b] += 1;
        }
        let visible: Vec<bool> = (0..n)
            .map(|i| local_mask[i] && tag_mask[i] && (self.graph_show_orphans || degree[i] > 0))
            .collect();

        // Pan / zoom navigation (like Obsidian's graph).
        let nav = ui.interact(
            rect,
            ui.id().with("graph_nav"),
            egui::Sense::click_and_drag(),
        );
        if nav.dragged() {
            self.graph_pan += nav.drag_delta();
        }
        if nav.hovered() {
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.graph_zoom = (self.graph_zoom * (scroll * 0.0015).exp()).clamp(0.2, 5.0);
            }
        }
        let center = rect.center() + self.graph_pan;
        let scale = base * self.graph_zoom;

        {
            // Straight links, so linked notes meet at clean junctions.
            let painter = ui.painter();
            for (a, b) in &edges {
                if !visible[*a] || !visible[*b] {
                    continue;
                }
                let (Some(pa_n), Some(pb_n)) = (pos.get(*a), pos.get(*b)) else {
                    continue;
                };
                let pa = center + egui::vec2(pa_n.0 * scale, pa_n.1 * scale);
                let pb = center + egui::vec2(pb_n.0 * scale, pb_n.1 * scale);
                let hot = active_idx == Some(*a) || active_idx == Some(*b);
                let stroke = egui::Stroke::new(
                    if hot { 1.6_f32 } else { 1.0_f32 },
                    line.linear_multiply(if hot { 0.55 } else { 0.25 }),
                );
                painter.line_segment([pa, pb], stroke);
            }
        }

        let mut clicked: Option<PathBuf> = None;
        for (i, path) in nodes.iter().enumerate() {
            if !visible[i] {
                continue;
            }
            let Some(p) = pos.get(i) else { continue };
            let point = center + egui::vec2(p.0 * scale, p.1 * scale);
            if !rect.expand(80.0).contains(point) {
                continue;
            }
            // Node size scales with how many links it has (like Obsidian).
            let r = 4.0 + (degree[i] as f32).sqrt() * 1.8;
            let node_rect = egui::Rect::from_center_size(point, egui::vec2(r * 2.0, r * 2.0));
            let resp = ui.interact(node_rect, ui.id().with(("gnode", i)), egui::Sense::click());
            let is_active = active.as_deref() == Some(path.as_path());
            let color = if is_active {
                theme::ACCENT
            } else if resp.hovered() {
                theme::TEXT
            } else {
                weak
            };
            let title = self.title_of(path);
            // Labels only when zoomed in, hovered, or the active note — like
            // Obsidian, which hides labels when the graph is zoomed out.
            let show_label = is_active || resp.hovered() || self.graph_zoom >= 1.25;
            {
                let painter = ui.painter();
                painter.circle_filled(point, r * 2.0, color.gamma_multiply(0.10));
                painter.circle_filled(point, r, color);
                if show_label {
                    painter.text(
                        point + egui::vec2(r + 4.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &title,
                        egui::FontId::proportional(11.0),
                        if is_active { theme::TEXT } else { weak },
                    );
                }
            }
            if resp.on_hover_text(title).clicked() {
                clicked = Some(path.clone());
            }
        }
        if let Some(p) = clicked {
            self.show_graph = false;
            self.pending_open = Some(p);
        }
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        if self.config.vim {
            self.vim_editor_ui(ui, idx);
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_source(("editor", idx))
            .show(ui, |ui| {
                let tab = &mut self.tabs[idx];
                let before = tab.text.clone();
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut tab.text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(30)
                        .lock_focus(true),
                );
                if resp.changed() && tab.text != before {
                    tab.undo.push(before);
                    if tab.undo.len() > 500 {
                        tab.undo.remove(0);
                    }
                    tab.redo.clear();
                }
            });
    }

    /// Send the viewport commands for the current window mode.
    fn send_window_mode(&self, ctx: &egui::Context) {
        match self.window_mode {
            WindowMode::Fullscreen => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                // Nudge the compositor for keyboard focus; a fullscreen window can
                // otherwise open unfocused.
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            WindowMode::Maximized => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            WindowMode::Windowed => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            }
        }
    }

    /// Apply the current window mode to the viewport and persist it (F11 / palette).
    fn apply_window_mode(&mut self, ctx: &egui::Context) {
        self.send_window_mode(ctx);
        self.config.window_mode = self.window_mode.as_str().to_string();
        self.config.save();
        self.status = format!("Window: {}", self.window_mode.label());
    }

    /// Undo the active tab's last edit.
    fn undo(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Some(prev) = tab.undo.pop() {
                tab.redo.push(std::mem::replace(&mut tab.text, prev));
            }
        }
    }

    /// Undo the last canvas edit (when a canvas is open).
    fn canvas_undo(&mut self) {
        if let Some(view) = self.canvas.as_mut() {
            view.undo();
        }
    }

    /// Redo the last canvas edit (when a canvas is open).
    fn canvas_redo(&mut self) {
        if let Some(view) = self.canvas.as_mut() {
            view.redo();
        }
    }

    /// Redo the active tab's last undone edit.
    fn redo(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Some(next) = tab.redo.pop() {
                tab.undo.push(std::mem::replace(&mut tab.text, next));
            }
        }
    }

    /// A Vim-style editor: renders the buffer with our own caret and feeds keys
    /// to the `vim` engine. Only used when `config.vim` is on.
    fn vim_editor_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        let size = self.config.font_size;
        let font = egui::FontId::monospace(size);
        let char_w = ui.fonts(|f| f.glyph_width(&font, ' ')).max(1.0);
        let line_h = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);

        // Intercept keys only when no other widget (search box, dialog) has focus.
        if ui.memory(|m| m.focused().is_none()) {
            let keys = collect_vim_keys(ui.ctx());
            if !keys.is_empty() {
                let before = self.tabs[idx].text.clone();
                let mut text = std::mem::take(&mut self.tabs[idx].text);
                for k in keys {
                    self.vim.handle(k, &mut text);
                }
                if text != before {
                    let tab = &mut self.tabs[idx];
                    tab.undo.push(before);
                    if tab.undo.len() > 500 {
                        tab.undo.remove(0);
                    }
                    tab.redo.clear();
                }
                self.tabs[idx].text = text;
                let len = self.tabs[idx].text.chars().count();
                self.vim.cursor = self.vim.cursor.min(len);
            }
        }

        let text = self.tabs[idx].text.clone();
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let max_cols = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(1)
            .max(1);
        let content = egui::vec2(
            max_cols as f32 * char_w + char_w * 4.0,
            lines.len().max(1) as f32 * line_h + line_h,
        );

        let mode = self.vim.mode;
        let mut click: Option<egui::Vec2> = None;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .id_source(("vim", idx))
            .show(ui, |ui| {
                let (rect, resp) = ui.allocate_exact_size(content, egui::Sense::click());
                let painter = ui.painter_at(rect);
                for (i, line) in lines.iter().enumerate() {
                    let pos = rect.min + egui::vec2(0.0, i as f32 * line_h);
                    painter.text(pos, egui::Align2::LEFT_TOP, line, font.clone(), theme::TEXT);
                }
                let (cl, cc) = char_to_linecol(&text, self.vim.cursor);
                let caret = egui::Rect::from_min_size(
                    rect.min + egui::vec2(cc as f32 * char_w, cl as f32 * line_h),
                    egui::vec2(char_w, line_h),
                );
                let caret_color = if mode == vim::Mode::Insert {
                    theme::TEXT
                } else {
                    theme::ACCENT
                };
                painter.rect_filled(caret, 1.0, caret_color.linear_multiply(0.55));
                ui.scroll_to_rect(caret, None);
                if resp.clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        click = Some(p - rect.min);
                    }
                }
            });

        if let Some(rel) = click {
            let line = ((rel.y / line_h) as usize).min(lines.len().saturating_sub(1));
            let col = (rel.x / char_w).round().max(0.0) as usize;
            let chars: Vec<char> = self.tabs[idx].text.chars().collect();
            self.vim.cursor = linecol_to_char(&chars, line, col);
        }

        ui.weak(self.vim.status());
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        let raw = self.tabs[idx].text.clone();
        // Hide any frontmatter block from the rendered preview.
        let body = frontmatter::parse(&raw).body;
        let size = self.config.font_size;
        let mut wiki: Vec<String> = Vec::new();
        let mut external: Vec<String> = Vec::new();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_source(("preview", idx))
            .show(ui, |ui| {
                // Attached images / audio / video scroll with the note.
                self.media_strip(ui, &body);
                let this: &Self = self;
                let resolver = move |target: &str| -> Option<(String, String)> {
                    let path = this.index.resolve(target)?;
                    let text = this.read_note(&path)?;
                    let meta = frontmatter::parse(&text);
                    let title = meta.title.unwrap_or_else(|| this.title_of(&path));
                    Some((title, meta.body))
                };
                ui.add_space(4.0);
                let clicked =
                    markdown::render(ui, &body, size, &resolver, &render_special_fence, 0);
                for c in clicked {
                    match c {
                        markdown::LinkClick::Wiki(t) => wiki.push(t),
                        markdown::LinkClick::External(u) => external.push(u),
                    }
                }
            });
        for target in wiki {
            if let Some(path) = self.index.resolve(&target) {
                self.pending_open = Some(path);
            } else {
                self.status = format!("No note named '{target}'");
            }
        }
        for url in external {
            open_external(&url);
        }
    }

    /// Live Preview: the note is rendered as markdown, but the active line is
    /// inline-editable. Up/Down (while editing) move the active line; Enter
    /// splits it; Backspace at the start of a line joins it with the previous.
    fn live_ui(&mut self, ui: &mut egui::Ui, idx: usize) {
        let size = self.config.font_size;
        let raw = self.tabs[idx].text.clone();
        let lines: Vec<String> = raw.split('\n').map(String::from).collect();
        let active = self.live_line.min(lines.len().saturating_sub(1));

        let no_embed = |_t: &str| -> Option<(String, String)> { None };
        let no_fence = |_ui: &mut egui::Ui, _l: &str, _c: &str| -> bool { false };

        let mut new_text: Option<String> = None;
        let mut new_active: Option<usize> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_source(("live", idx))
            .show(ui, |ui| {
                self.media_strip(ui, &raw);
                ui.add_space(4.0);
                let mut buffer: Vec<String> = Vec::new();
                for (i, line) in lines.iter().enumerate() {
                    if i == active {
                        if !buffer.is_empty() {
                            let md = buffer.join("\n");
                            markdown::render(ui, &md, size, &no_embed, &no_fence, 0);
                            buffer.clear();
                        }
                        let mut tmp = line.clone();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut tmp)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                        if resp.changed() {
                            let mut ls = lines.clone();
                            ls[i] = tmp.clone();
                            new_text = Some(ls.join("\n"));
                        }
                        if resp.has_focus() {
                            let (enter, up, down, backspace) = ui.input(|inp| {
                                (
                                    inp.key_pressed(egui::Key::Enter),
                                    inp.key_pressed(egui::Key::ArrowUp),
                                    inp.key_pressed(egui::Key::ArrowDown),
                                    inp.key_pressed(egui::Key::Backspace),
                                )
                            });
                            if enter {
                                let mut ls = lines.clone();
                                ls[i] = tmp.clone();
                                ls.insert(i + 1, String::new());
                                new_text = Some(ls.join("\n"));
                                new_active = Some(i + 1);
                            } else if up && i > 0 {
                                new_active = Some(i - 1);
                            } else if down && i + 1 < lines.len() {
                                new_active = Some(i + 1);
                            } else if backspace && tmp.is_empty() && i > 0 {
                                let mut ls = lines.clone();
                                ls.remove(i);
                                new_text = Some(ls.join("\n"));
                                new_active = Some(i - 1);
                            }
                        }
                    } else {
                        buffer.push(line.clone());
                    }
                }
                if !buffer.is_empty() {
                    let md = buffer.join("\n");
                    markdown::render(ui, &md, size, &no_embed, &no_fence, 0);
                }
            });

        if let Some(t) = new_text {
            self.tabs[idx].text = t;
        }
        if let Some(a) = new_active {
            self.live_line = a;
        }
    }

    fn welcome_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.add(
                egui::Image::new(egui::include_image!("../assets/logo.svg"))
                    .fit_to_exact_size(egui::vec2(104.0, 104.0)),
            );
            ui.add_space(6.0);
            ui.label(RichText::new("Encrypted Note").size(34.0).strong());
            ui.label(RichText::new("AES-256 encrypted markdown vault").weak());
            ui.add_space(20.0);
            if icons::button(ui, Icon::FolderOpen, 30.0, true, false, "Open a vault").clicked() {
                self.open_picker();
            }
            ui.label(RichText::new("Open a vault").weak().small());
            if !self.config.recent_vaults.is_empty() {
                ui.add_space(14.0);
                ui.label(RichText::new("Recent vaults").strong());
                let recents = self.config.recent_vaults.clone();
                for path in recents {
                    let clicked = ui
                        .horizontal(|ui| {
                            icons::icon(ui, Icon::Folder, 14.0, ui.visuals().weak_text_color());
                            ui.add(egui::Button::new(path.display().to_string()).frame(false))
                                .clicked()
                        })
                        .inner;
                    if clicked {
                        self.pending_open_vault = Some(path);
                    }
                }
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // Directory picker.
        if self.picker.is_some() {
            let mut open = true;
            let mut chosen: Option<PathBuf> = None;
            let mut close = false;
            {
                let picker = self.picker.as_mut().unwrap();
                egui::Window::new("Open vault")
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([560.0, 420.0])
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            if icons::button(ui, Icon::ArrowUp, 18.0, true, false, "Up one level")
                                .clicked()
                            {
                                picker.go_up();
                            }
                            if icons::button(ui, Icon::Home, 18.0, true, false, "Home").clicked() {
                                if let Some(home) = std::env::var_os("HOME") {
                                    picker.enter(PathBuf::from(home));
                                }
                            }
                            ui.label(
                                RichText::new(picker.current.display().to_string()).monospace(),
                            );
                        });
                        ui.separator();

                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut picker.manual)
                                    .hint_text("/home/user/Notes")
                                    .desired_width(300.0),
                            );
                            if icons::button(ui, Icon::Check, 18.0, true, false, "Go to path")
                                .clicked()
                            {
                                let p = PathBuf::from(picker.manual.trim());
                                if p.is_dir() {
                                    picker.enter(p);
                                } else {
                                    picker.error = Some("Not a directory".into());
                                }
                            }
                        });

                        if let Some(err) = &picker.error {
                            ui.colored_label(theme::DANGER, err);
                        }

                        let dirs = picker.subdirs.clone();
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .max_height(240.0)
                            .show(ui, |ui| {
                                for dir in dirs {
                                    let name = dir
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(
                                                ui,
                                                Icon::Folder,
                                                14.0,
                                                ui.visuals().weak_text_color(),
                                            );
                                            ui.add(
                                                egui::Button::new(name).frame(false).min_size(
                                                    egui::vec2(ui.available_width(), 0.0),
                                                ),
                                            )
                                            .clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        picker.enter(dir);
                                    }
                                }
                            });

                        ui.separator();
                        ui.horizontal(|ui| {
                            if icons::button(ui, Icon::Check, 20.0, true, false, "Open this folder")
                                .clicked()
                            {
                                chosen = Some(picker.current.clone());
                                close = true;
                            }
                            if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked()
                            {
                                close = true;
                            }
                        });
                    });
            }
            if let Some(path) = chosen {
                self.open_vault(path);
            }
            if close || !open {
                self.picker = None;
            }
        }

        // New note dialog.
        if self.new_note_open {
            let mut open = self.new_note_open;
            egui::Window::new("New note")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(RichText::new("New note").strong());
                    ui.add_space(4.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.new_note_name).hint_text("Note name"),
                    );
                    if resp.changed() {
                        self.new_note_error = None;
                    }
                    if let Some(err) = &self.new_note_error {
                        ui.add_space(2.0);
                        ui.colored_label(theme::DANGER, err.clone());
                    }
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if icons::button(ui, Icon::Check, 20.0, true, false, "Create note")
                            .clicked()
                            || enter
                        {
                            let name = self.new_note_name.clone();
                            if self.create_note(&name) {
                                self.new_note_open = false;
                                self.new_note_error = None;
                            } else {
                                self.new_note_error = Some(
                                    "You can't create an untitled note — enter a name.".into(),
                                );
                            }
                        }
                        if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                            self.new_note_open = false;
                            self.new_note_error = None;
                        }
                    });
                });
            if !open {
                self.new_note_open = false;
            }
        }
        // New canvas dialog.
        if self.new_canvas_open {
            let mut open = self.new_canvas_open;
            egui::Window::new("New canvas")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(RichText::new("New canvas").strong());
                    ui.add_space(4.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.new_canvas_name)
                            .hint_text("Canvas name"),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if icons::button(ui, Icon::Check, 20.0, true, false, "Create canvas")
                            .clicked()
                            || enter
                        {
                            let name = self.new_canvas_name.clone();
                            self.create_canvas(&name);
                            self.new_canvas_open = false;
                        }
                        if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                            self.new_canvas_open = false;
                        }
                    });
                });
            if !open {
                self.new_canvas_open = false;
            }
        }
        // New sub-note dialog.
        if self.subnote_open {
            let parent_name = self
                .subnote_parent
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut open = self.subnote_open;
            let mut win = egui::Window::new(format!("New sub-note under '{parent_name}'"))
                .title_bar(false)
                .collapsible(false)
                .resizable(false);
            if let Some(pos) = self.dialog_pos {
                win = win.fixed_pos(pos);
            }
            win.open(&mut open).show(ctx, |ui| {
                ui.label(RichText::new(format!("New sub-note under '{parent_name}'")).strong());
                ui.add_space(4.0);
                let resp =
                    ui.add(egui::TextEdit::singleline(&mut self.subnote_name).hint_text("Name"));
                if resp.changed() {
                    self.subnote_error = None;
                }
                if let Some(err) = &self.subnote_error {
                    ui.add_space(2.0);
                    ui.colored_label(theme::DANGER, err.clone());
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if icons::button(ui, Icon::Check, 20.0, true, false, "Create sub-note")
                        .clicked()
                        || enter
                    {
                        let name = self.subnote_name.clone();
                        let ok = self
                            .subnote_parent
                            .clone()
                            .map(|p| self.create_subnote(&p, &name))
                            .unwrap_or(false);
                        if ok {
                            self.subnote_open = false;
                            self.subnote_error = None;
                        } else {
                            self.subnote_error =
                                Some("You can't create an untitled note — enter a name.".into());
                        }
                    }
                    if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                        self.subnote_open = false;
                        self.subnote_error = None;
                    }
                });
            });
            if !open {
                self.subnote_open = false;
            }
        }
        // Rename dialog.
        if self.rename_target.is_some() {
            let mut open = true;
            let mut submit = false;
            let mut cancel = false;
            let mut win = egui::Window::new("Rename note")
                .title_bar(false)
                .collapsible(false)
                .resizable(false);
            if let Some(pos) = self.dialog_pos {
                win = win.fixed_pos(pos);
            }
            win.open(&mut open).show(ctx, |ui| {
                ui.label(RichText::new("Rename note").strong());
                ui.add_space(4.0);
                let resp = ui.text_edit_singleline(&mut self.rename_name);
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if icons::button(ui, Icon::Check, 20.0, true, false, "Rename").clicked()
                        || enter
                    {
                        submit = true;
                    }
                    if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
            if submit {
                if let Some(old) = self.rename_target.take() {
                    let name = self.rename_name.clone();
                    self.rename_note(old, &name);
                }
            }
            if cancel || !open {
                self.rename_target = None;
            }
        }

        // Delete confirmation.
        if self.confirm_delete.is_some() {
            let name = self
                .confirm_delete
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut open = true;
            let mut confirm = false;
            let mut win = egui::Window::new("Delete note")
                .title_bar(false)
                .collapsible(false)
                .resizable(false);
            if let Some(pos) = self.dialog_pos {
                win = win.fixed_pos(pos);
            }
            win.open(&mut open).show(ctx, |ui| {
                ui.label(format!("Permanently delete '{name}'?"));
                ui.horizontal(|ui| {
                    if icons::danger_button(ui, Icon::Trash, 20.0, true, "Delete").clicked() {
                        confirm = true;
                    }
                    if icons::button(ui, Icon::Close, 20.0, true, false, "Cancel").clicked() {
                        self.confirm_delete = None;
                    }
                });
            });
            if confirm {
                if let Some(path) = self.confirm_delete.take() {
                    self.delete_note(path);
                }
            } else if !open {
                self.confirm_delete = None;
            }
        }

        // Template chooser.
        if let Some(templates) = self.template_pick.clone() {
            let mut open = true;
            let mut pick: Option<PathBuf> = None;
            egui::Window::new("New note from template")
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(RichText::new("New note from a template").strong());
                    ui.add_space(4.0);
                    for t in &templates {
                        let name = t
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let clicked = ui
                            .horizontal(|ui| {
                                icons::icon(ui, Icon::File, 14.0, ui.visuals().weak_text_color());
                                ui.add(egui::Button::new(name).frame(false)).clicked()
                            })
                            .inner;
                        if clicked {
                            pick = Some(t.clone());
                        }
                    }
                });
            if let Some(t) = pick {
                self.new_from_template(t);
                self.template_pick = None;
            } else if !open {
                self.template_pick = None;
            }
        }

        // Canvas chooser.
        if let Some(canvases) = self.canvas_pick.clone() {
            let root = self
                .vault
                .as_ref()
                .map(|v| v.root.clone())
                .unwrap_or_default();
            let items: Vec<(PathBuf, String)> = canvases
                .iter()
                .map(|c| {
                    let rel = c.strip_prefix(&root).unwrap_or(c).display().to_string();
                    (c.clone(), rel)
                })
                .collect();
            let mut open = true;
            let mut pick: Option<PathBuf> = None;
            egui::Window::new("Open canvas")
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(RichText::new("Open canvas").strong());
                    ui.add_space(4.0);
                    for (path, rel) in &items {
                        let clicked = ui
                            .horizontal(|ui| {
                                icons::icon(ui, Icon::Graph, 14.0, ui.visuals().weak_text_color());
                                ui.add(egui::Button::new(rel).frame(false)).clicked()
                            })
                            .inner;
                        if clicked {
                            pick = Some(path.clone());
                        }
                    }
                    ui.add_space(6.0);
                    if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                        self.canvas_pick = None;
                    }
                });
            if let Some(p) = pick {
                self.open_canvas_path(p);
                self.canvas_pick = None;
            } else if !open {
                self.canvas_pick = None;
            }
        }

        // Attachment file chooser.
        if self.attach_picker.is_some() {
            let mut chosen: Option<PathBuf> = None;
            let mut close = false;
            {
                let picker = self.attach_picker.as_mut().unwrap();
                egui::Window::new("Attach file")
                    .title_bar(false)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([560.0, 420.0])
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!("Attach a {}", picker.kind.label())).strong(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if icons::button(ui, Icon::ArrowUp, 16.0, true, false, "Up").clicked() {
                                picker.go_up();
                            }
                            if icons::button(ui, Icon::Home, 16.0, true, false, "Home").clicked() {
                                if let Some(home) = std::env::var_os("HOME") {
                                    picker.enter(PathBuf::from(home));
                                }
                            }
                            ui.label(
                                RichText::new(picker.current.display().to_string())
                                    .monospace()
                                    .small(),
                            );
                        });
                        if let Some(err) = &picker.error {
                            ui.colored_label(theme::DANGER, err);
                        }
                        let dirs = picker.dirs.clone();
                        let files = picker.files.clone();
                        let dirs_is_empty = dirs.is_empty();
                        let files_is_empty = files.is_empty();
                        let kind_label = picker.kind.label();
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for d in dirs {
                                    let name = d
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(
                                                ui,
                                                Icon::Folder,
                                                14.0,
                                                ui.visuals().weak_text_color(),
                                            );
                                            ui.add(egui::Button::new(name).frame(false)).clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        picker.enter(d);
                                    }
                                }
                                for f in files {
                                    let name = f
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let clicked = ui
                                        .horizontal(|ui| {
                                            icons::icon(
                                                ui,
                                                Icon::File,
                                                14.0,
                                                ui.visuals().weak_text_color(),
                                            );
                                            ui.add(egui::Button::new(name).frame(false)).clicked()
                                        })
                                        .inner;
                                    if clicked {
                                        chosen = Some(f.clone());
                                    }
                                }
                                if dirs_is_empty && files_is_empty {
                                    ui.add_space(4.0);
                                    ui.weak(format!(
                                        "No {} files in this folder — browse to one.",
                                        kind_label
                                    ));
                                }
                            });
                        ui.add_space(6.0);
                        if icons::button(ui, Icon::Close, 18.0, true, false, "Cancel").clicked() {
                            close = true;
                        }
                    });
            }
            if let Some(p) = chosen {
                self.attach_file(&p);
                self.attach_picker = None;
            } else if close {
                self.attach_picker = None;
            }
        }

        // Daily notes settings.
        if self.daily_settings_open {
            let mut open = true;
            egui::Window::new("Daily notes")
                .title_bar(false)
                .collapsible(false)
                .resizable(true)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(RichText::new("Folder").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.daily_dir)
                            .hint_text("Daily")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Date format").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.daily_format)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(f32::INFINITY),
                    );
                    ui.label(
                        RichText::new("Tokens: YYYY YY MMMM MMM MM M DD D dddd ddd")
                            .weak()
                            .small(),
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Template (note title or path)").strong());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.config.daily_template)
                            .hint_text("Templates/Daily")
                            .desired_width(f32::INFINITY),
                    );
                    ui.label(
                        RichText::new("Placeholders: {{date}} {{time}} {{title}}")
                            .weak()
                            .small(),
                    );
                    ui.add_space(10.0);
                    if ui.button("Done").clicked() {
                        self.config.save();
                        self.daily_settings_open = false;
                    }
                });
            if !open {
                self.config.save();
                self.daily_settings_open = false;
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let save = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
        let new = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::N);
        let find = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
        let open = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let palette = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::P);

        if ctx.input_mut(|i| i.consume_shortcut(&palette)) {
            self.open_palette();
        }

        // Undo / redo.
        let undo = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        let redo = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::Y);
        let redo_shift = egui::KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        let redo_hit =
            ctx.input_mut(|i| i.consume_shortcut(&redo_shift) || i.consume_shortcut(&redo));
        let undo_hit = ctx.input_mut(|i| i.consume_shortcut(&undo));
        // Route undo/redo to the canvas when one is open, else to the editor.
        if self.canvas.is_some() {
            if redo_hit {
                self.canvas_redo();
            } else if undo_hit {
                self.canvas_undo();
            }
        } else if redo_hit {
            self.redo();
        } else if undo_hit {
            self.undo();
        }

        if ctx.input_mut(|i| i.consume_shortcut(&save)) {
            self.save_active();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&new)) {
            self.new_note_open = true;
            self.new_note_error = None;
            self.new_note_name.clear();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&find)) {
            self.focus_search = true;
            self.show_search_results = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&open)) {
            self.open_picker();
        }
        // Ctrl+W closes the active tab.
        let close = egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
        if ctx.input_mut(|i| i.consume_shortcut(&close)) && !self.tabs.is_empty() {
            self.close_tab(self.active);
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let frame = egui::Frame::side_top_panel(&ctx.style())
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(10.0, 4.0));
        egui::TopBottomPanel::bottom("status_bar")
            .frame(frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.status.is_empty() {
                        let vault = self
                            .vault
                            .as_ref()
                            .map(|v| format!("{} note(s)", v.notes.len()))
                            .unwrap_or_else(|| "no vault".into());
                        ui.weak(vault);
                    } else {
                        ui.weak(self.status.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(tab) = self.tabs.get(self.active) {
                            let words = tab.text.split_whitespace().count();
                            let chars = tab.text.chars().count();
                            ui.weak(format!("{words} words · {chars} chars"));
                        }
                    });
                });
            });
    }
}

impl eframe::App for EncryptedNoteApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // On the first frame, report which GPU we're rendering with.
        if !self.logged_renderer {
            self.logged_renderer = true;
            if let Some(rs) = frame.wgpu_render_state() {
                let info = rs.adapter.get_info();
                eprintln!(
                    "Encrypted Note: GPU rendering — backend={:?}, device='{}', driver='{} {}'",
                    info.backend, info.name, info.driver, info.driver_info
                );
            }
        }
        // Keep the window title in sync — but only send the command when it
        // actually changes. Sending it every frame spams the compositor with
        // reconfigure requests, which can make a fullscreen window unresponsive.
        let desired_title = if self.show_password_warning {
            "Encrypted Note — Save your password".to_string()
        } else if !matches!(self.gate, Gate::Open) {
            "Encrypted Note — Locked".to_string()
        } else if let Some(tab) = self.tabs.get(self.active) {
            let mark = if tab.dirty() { "● " } else { "" };
            format!("{mark}{} — Encrypted Note", tab.title)
        } else {
            "Encrypted Note".to_string()
        };
        if desired_title != self.window_title {
            self.window_title = desired_title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(desired_title));
        }

        // Ask the compositor for keyboard focus once. This must happen *before*
        // the lock-screen early-return below, otherwise a fullscreen window opens
        // on the login page without focus and you can't type.
        if !self.requested_focus {
            self.requested_focus = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        // F11 cycles the window mode: fullscreen → maximized → windowed.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F11)) {
            self.window_mode = self.window_mode.next();
            self.apply_window_mode(ctx);
        }

        self.tick_media(ctx);

        // Repaint on window resize so the UI never shows a stale frame. Compare
        // rounded rects so sub-pixel jitter doesn't cause a repaint storm.
        let screen = ctx.screen_rect();
        let rounded = egui::Rect::from_min_max(
            egui::pos2(screen.min.x.round(), screen.min.y.round()),
            egui::pos2(screen.max.x.round(), screen.max.y.round()),
        );
        if self.last_screen != Some(rounded) {
            self.last_screen = Some(rounded);
            ctx.request_repaint();
        }

        // After creating a password, show the "record your password" popup
        // before anything else. The vault opens when the user presses OK.
        if self.show_password_warning {
            self.password_warning_ui(ctx);
            return;
        }

        // While locked, show only the password screen. Nothing else runs.
        if !matches!(self.gate, Gate::Open) {
            self.gate_ui(ctx);
            return;
        }

        self.handle_shortcuts(ctx);

        // Autosave dirty notes every few seconds.
        let now = ctx.input(|i| i.time);
        if now - self.last_autosave > 3.0 {
            self.last_autosave = now;
            self.save_all();
            self.save_workspace();
            self.config.save();
        }

        self.top_bar(ctx);
        self.status_bar(ctx);
        self.left_panel(ctx);
        self.right_panel(ctx);
        self.central_panel(ctx);
        self.dialogs(ctx);
        self.feeds_dialogs(ctx);
        self.palette_ui(ctx);

        // Poll background feed work.
        self.poll_feeds();
        self.poll_full_article();
        self.tick_favicon(ctx);

        // Apply deferred actions now that all UI borrows have ended.
        if let Some((y, m, d)) = self.pending_daily.take() {
            self.open_daily_note_for(y, m, d);
        }
        if let Some(path) = self.pending_open_vault.take() {
            self.open_vault(path);
        }
        if let Some(path) = self.pending_open.take() {
            self.open_note(path);
            // If the note's title changed since it was opened, refresh backlinks.
            self.last_active = None;
        }

        // Recompute backlinks whenever the active note changes.
        let current = self.tabs.get(self.active).map(|t| t.path.clone());
        if current != self.last_active {
            self.last_active = current;
            self.recompute_backlinks();
        }
    }

    fn on_exit(&mut self) {
        self.save_all();
        self.save_workspace();
        // Persist an open canvas on the way out.
        if let Some(view) = self.canvas.take() {
            let _ = self.write_canvas(&view.path, &view.data);
        }
        self.config.save();
    }
}

/// Actions collected while drawing the tree, applied by the caller afterwards.
#[derive(Default)]
struct TreeActions {
    open: Option<PathBuf>,
    rename: Option<PathBuf>,
    delete: Option<PathBuf>,
    subnote: Option<PathBuf>,
    /// Screen position (right of the note row) to place the dialog at.
    pos: Option<egui::Pos2>,
}

/// Collect every directory rel-path in the tree (used to expand by default).
fn collect_dir_rels(node: &TreeNode, out: &mut HashSet<PathBuf>) {
    for dir in &node.dirs {
        out.insert(dir.rel.clone());
        collect_dir_rels(dir, out);
    }
}

/// Recursively draw a vault tree, collecting any requested actions.
fn show_tree(
    ui: &mut egui::Ui,
    node: &TreeNode,
    selected: Option<&Path>,
    expanded: &mut HashSet<PathBuf>,
    titles: &BTreeMap<PathBuf, String>,
    actions: &mut TreeActions,
) {
    let weak = ui.visuals().weak_text_color();
    for dir in &node.dirs {
        let is_open = expanded.contains(&dir.rel);
        let mut toggle = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let chev = if is_open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            if icons::button(ui, chev, 14.0, true, false, "").clicked() {
                toggle = true;
            }
            icons::icon(ui, Icon::Folder, 15.0, theme::ACCENT);
            ui.label(RichText::new(format!("{}  ({})", dir.name, dir.note_count())).strong());
        });
        if toggle {
            if is_open {
                expanded.remove(&dir.rel);
            } else {
                expanded.insert(dir.rel.clone());
            }
        }
        if is_open {
            ui.indent(dir.rel.clone(), |ui| {
                show_tree(ui, dir, selected, expanded, titles, actions);
            });
        }
    }
    for file in &node.files {
        let is_selected = selected == Some(file.path.as_path());
        let mut open = false;
        // Hover reveals the row's action icons inline (no right-click menu).
        let row = egui::Rect::from_min_size(
            ui.next_widget_position(),
            egui::vec2(ui.available_width(), 20.0),
        );
        let hovered = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| row.contains(p));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            icons::icon(ui, Icon::File, 14.0, weak);
            let label = titles
                .get(&file.path)
                .cloned()
                .unwrap_or_else(|| file.title.clone());
            let resp = ui.selectable_label(is_selected, RichText::new(label));
            if resp.clicked() {
                open = true;
            }
            if hovered {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, Icon::Trash, 13.0, true, false, "Delete").clicked() {
                        actions.delete = Some(file.path.clone());
                        actions.pos = Some(egui::pos2(row.max.x + 6.0, row.min.y));
                    }
                    if icons::button(ui, Icon::Edit, 13.0, true, false, "Rename").clicked() {
                        actions.rename = Some(file.path.clone());
                        actions.pos = Some(egui::pos2(row.max.x + 6.0, row.min.y));
                    }
                    if icons::button(ui, Icon::Add, 13.0, true, false, "New sub-note").clicked() {
                        actions.subnote = Some(file.path.clone());
                        actions.pos = Some(egui::pos2(row.max.x + 6.0, row.min.y));
                    }
                });
            }
        });
        if open {
            actions.open = Some(file.path.clone());
        }
    }
}

/// Draw the live password-policy checklist (icon + colour for each rule).
fn policy_checklist(ui: &mut egui::Ui, password: &str) {
    let checks = [
        (
            password.chars().count() >= crate::crypto::MIN_PASSWORD_LEN,
            "At least 16 characters",
        ),
        (password.chars().any(|c| c.is_ascii_digit()), "A number"),
        (
            password.chars().any(|c| c.is_uppercase()),
            "A capital letter",
        ),
        (
            password.chars().any(|c| c.is_lowercase()),
            "A lower-case letter",
        ),
        (
            password.chars().any(|c| !c.is_alphanumeric()),
            "A special character",
        ),
        (
            !crypto::has_letter_run(password, 4),
            "No words (max 3 letters in a row)",
        ),
    ];
    for (ok, label) in checks {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let (ic, color) = if ok {
                (Icon::Check, theme::SUCCESS)
            } else {
                (Icon::Close, ui.visuals().weak_text_color())
            };
            icons::icon(ui, ic, 13.0, color);
            ui.label(RichText::new(label).small().color(color));
        });
    }
}

/// Format a number of seconds as `Xm YYs` or `Ys`.
fn fmt_duration(secs: u64) -> String {
    let minutes = secs / 60;
    let seconds = secs % 60;
    if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Map a character index to a `(line, col)` pair.
fn char_to_linecol(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for (i, c) in text.chars().enumerate() {
        if i == cursor {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Map a `(line, col)` pair back to a character index.
fn linecol_to_char(chars: &[char], line: usize, col: usize) -> usize {
    let mut l = 0;
    let mut idx = 0;
    while idx < chars.len() && l < line {
        if chars[idx] == '\n' {
            l += 1;
        }
        idx += 1;
    }
    let mut c = 0;
    while idx < chars.len() && chars[idx] != '\n' && c < col {
        c += 1;
        idx += 1;
    }
    idx
}

/// Collect this frame's key/`Text` events as Vim keys.
fn collect_vim_keys(ctx: &egui::Context) -> Vec<vim::Key> {
    let mut out = Vec::new();
    ctx.input(|i| {
        for ev in &i.events {
            match ev {
                egui::Event::Text(t) => {
                    for c in t.chars() {
                        out.push(vim::Key::Char(c));
                    }
                }
                egui::Event::Key {
                    key, pressed: true, ..
                } => {
                    let k = match key {
                        egui::Key::Escape => Some(vim::Key::Esc),
                        egui::Key::Enter => Some(vim::Key::Enter),
                        egui::Key::Backspace => Some(vim::Key::Backspace),
                        egui::Key::Delete => Some(vim::Key::Delete),
                        egui::Key::Tab => Some(vim::Key::Tab),
                        egui::Key::ArrowLeft => Some(vim::Key::Left),
                        egui::Key::ArrowRight => Some(vim::Key::Right),
                        egui::Key::ArrowUp => Some(vim::Key::Up),
                        egui::Key::ArrowDown => Some(vim::Key::Down),
                        egui::Key::Home => Some(vim::Key::Home),
                        egui::Key::End => Some(vim::Key::End),
                        _ => None,
                    };
                    if let Some(k) = k {
                        out.push(k);
                    }
                }
                _ => {}
            }
        }
    });
    out
}

/// A modern, clickable link row (icon + label + domain) opening `url`.
fn link_row(ui: &mut egui::Ui, url: &str, label: &str) {
    let color = theme::LINK;
    let frame = egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .rounding(egui::Rounding::same(8.0));
    ui.vertical(|ui| {
        let inner = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icons::icon(ui, Icon::Link, 15.0, color);
                ui.label(RichText::new(label).color(color).strong());
                ui.label(RichText::new(url_domain(url)).weak().small());
            });
        });
        let resp = ui
            .interact(
                inner.response.rect,
                ui.id().with(("feed-link", url)),
                egui::Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(url);
        if resp.clicked() {
            open_external(url);
        }
    });
}

/// The host part of a URL, for display.
fn url_domain(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

/// Local file targets (`assets/…`) referenced by markdown media links.
fn media_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            if let Some(end_rel) = text[start..].find(')') {
                let url = text[start..start + end_rel].trim().to_string();
                let ext = Path::new(&url)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !url.starts_with("http")
                    && (is_image_ext(&ext) || is_audio_ext(&ext) || is_video_ext(&ext))
                    && !out.contains(&url)
                {
                    out.push(url);
                }
                i = start + end_rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_image_ext(e: &str) -> bool {
    matches!(e, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
}

fn is_audio_ext(e: &str) -> bool {
    matches!(e, "mp3" | "ogg" | "oga" | "wav" | "m4a" | "flac" | "opus")
}

fn is_video_ext(e: &str) -> bool {
    matches!(e, "mp4" | "webm" | "mkv" | "mov" | "avi")
}

/// The rounded frame used for the inline media players.
fn media_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .rounding(egui::Rounding::same(8.0))
}

/// Format a number of seconds as `m:ss`.
fn fmt_time(secs: f32) -> String {
    if !secs.is_finite() || secs < 1.0e-3 {
        return "0:00".into();
    }
    let total = secs as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

/// A clickable, draggable progress bar. Returns the seek fraction when the user
/// clicks it or releases a drag.
fn scrubber(ui: &mut egui::Ui, elapsed: f32, duration: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 16.0),
        egui::Sense::click_and_drag(),
    );
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    let mut frac = if duration > 0.0 {
        (elapsed / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            frac = ((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
        }
    }
    let bg = ui.visuals().widgets.inactive.bg_fill;
    let painter = ui.painter();
    painter.rect_filled(track, egui::Rounding::same(3.0), bg);
    let filled =
        egui::Rect::from_min_size(track.min, egui::vec2(track.width() * frac, track.height()));
    painter.rect_filled(filled, egui::Rounding::same(3.0), theme::ACCENT);
    painter.circle_filled(
        egui::pos2(track.left() + track.width() * frac, track.center().y),
        6.0,
        theme::ACCENT,
    );
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() || resp.drag_stopped() {
        Some(frac)
    } else {
        None
    }
}

/// A dim, non-interactive progress bar shown for media that isn't playing.
fn inactive_bar(ui: &mut egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::hover());
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 6.0));
    let bg = ui.visuals().widgets.inactive.bg_fill;
    ui.painter()
        .rect_filled(track, egui::Rounding::same(3.0), bg);
}

/// Resolve a vault-relative media path, refusing anything that escapes the
/// vault (absolute paths, `..` components, or symlinks pointing outside).
fn vault_file(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return None;
    }
    if p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let candidate = root.join(p);
    let canon = candidate.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    if canon.starts_with(&root_canon) {
        Some(canon)
    } else {
        None
    }
}

/// Decode an image file (raster or SVG) into an egui colour image.
fn load_color_image(path: &Path) -> Option<egui::ColorImage> {
    let bytes = std::fs::read(path).ok()?;
    decode_image_bytes(&bytes)
}

/// Decode image bytes (raster via `image`, or SVG via `resvg`) into an egui image.
fn decode_image_bytes(bytes: &[u8]) -> Option<egui::ColorImage> {
    let head = &bytes[..bytes.len().min(512)];
    let looks_svg = head.windows(4).any(|w| w == b"<svg") || head.windows(5).any(|w| w == b"<?xml");
    if looks_svg {
        return rasterize_svg_bytes(bytes, 256);
    }
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}

/// Rasterise SVG bytes to a colour image about `target_w` pixels wide.
fn rasterize_svg_bytes(data: &[u8], target_w: u32) -> Option<egui::ColorImage> {
    use resvg::usvg::TreeParsing;
    let usvg_tree = resvg::usvg::Tree::from_data(data, &resvg::usvg::Options::default()).ok()?;
    let size = usvg_tree.size.to_int_size();
    let (w0, h0) = (size.width().max(1), size.height().max(1));
    let scale = target_w as f32 / w0 as f32;
    let w = ((w0 as f32 * scale).round() as u32).max(1);
    let h = ((h0 as f32 * scale).round() as u32).max(1);
    let tree = resvg::Tree::from_usvg(&usvg_tree);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    tree.render(
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA; egui wants straight alpha.
    let premul = pixmap.take();
    let mut out = Vec::with_capacity(premul.len());
    for px in premul.as_chunks::<4>().0 {
        let a = px[3];
        if a == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let af = a as f32 / 255.0;
            out.extend_from_slice(&[
                (px[0] as f32 / af).min(255.0) as u8,
                (px[1] as f32 / af).min(255.0) as u8,
                (px[2] as f32 / af).min(255.0) as u8,
                a,
            ]);
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &out,
    ))
}

/// Draw a texture at a square size (used for favicon icons).
fn draw_tex(ui: &mut egui::Ui, tex: &egui::TextureHandle, size: f32) {
    ui.add(
        egui::Image::new(egui::load::SizedTexture::from_handle(tex))
            .fit_to_exact_size(egui::vec2(size, size))
            .rounding(egui::Rounding::same(3.0)),
    );
}

/// A small palette of glyphs an entry can use as its icon.
const PW_ICONS: &[&str] = &[
    "🔑", "🔒", "🌐", "📧", "💳", "🏦", "🛒", "🎮", "💬", "📱", "🖥", "☁", "🐙", "📦", "🎵", "🎬",
    "📸", "🛠", "⚙", "🎓", "❤", "⭐", "🔥", "🚀", "🧾", "💼", "🏠", "🔖",
];

/// A small rounded "chip" used for password categories.
fn pw_chip(ui: &mut egui::Ui, text: &str) {
    egui::Frame::none()
        .fill(theme::WIDGET_ACTIVE)
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(theme::TEXT));
        });
}

/// A four-segment strength meter with a label and an entropy estimate.
fn pw_strength_bar(ui: &mut egui::Ui, score: u8, label: &str, bits: f64) {
    let color = match score {
        0 | 1 => theme::DANGER,
        2 => Color32::from_rgb(0xe0, 0xb0, 0x5f),
        3 => theme::LINK,
        _ => theme::SUCCESS,
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        for i in 0..4 {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 5.0), egui::Sense::hover());
            let fill = if i < score {
                color
            } else {
                ui.visuals().widgets.inactive.bg_fill
            };
            ui.painter()
                .rect_filled(rect, egui::Rounding::same(2.0), fill);
        }
        ui.add_space(4.0);
        ui.label(RichText::new(label).small().color(color));
        ui.label(RichText::new(format!("{bits:.0} bits")).small().weak());
    });
}

/// A labelled single-line text field for the password editor.
fn pw_field(ui: &mut egui::Ui, label: &str, text: &mut String) {
    ui.label(RichText::new(label).small().weak());
    ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
}

/// A labelled value with a copy button on the right. Returns `true` on copy.
fn pw_copy_row(ui: &mut egui::Ui, label: &str, value: &str) -> bool {
    let mut copy = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, Icon::Copy, 15.0, !value.is_empty(), false, "Copy").clicked() {
                copy = true;
            }
        });
    });
    ui.label(RichText::new(if value.is_empty() { "—" } else { value }).monospace());
    copy
}

/// Default file path offered by the password import/export dialog.
fn default_export_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/passwords-export.json")
}

/// Restrict a file to owner read/write only (best-effort, Unix).
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
}

/// Expand a leading `~` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Paint the login screen background: a vertical gradient with a soft, gently
/// pulsing accent glow behind the card.
fn gate_background(painter: &egui::Painter, rect: egui::Rect, time: f64) {
    let top = Color32::from_rgb(0x18, 0x16, 0x24);
    let bottom = Color32::from_rgb(0x0b, 0x0b, 0x11);
    let mut mesh = egui::epaint::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));

    let pulse = 0.045 + 0.018 * (time * 0.9).sin() as f32;
    let center = egui::pos2(rect.center().x, rect.center().y - rect.height() * 0.08);
    for i in (1..=6).rev() {
        let r = rect.height() * 0.10 * i as f32;
        painter.circle_filled(center, r, theme::PRIMARY.gamma_multiply(pulse / i as f32));
    }
}

/// Draw an animated hourglass: sand drains from the top bulb to the bottom one
/// as `fraction` (remaining / total) falls from 1 to 0.
fn hourglass(ui: &mut egui::Ui, size: f32, fraction: f32, time: f64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size * 1.35), egui::Sense::hover());
    let c = rect.center();
    let half = size * 0.30;
    let top = rect.top() + 6.0;
    let bottom = rect.bottom() - 6.0;
    let mid = c.y;
    let frac = fraction.clamp(0.0, 1.0);
    let p = |x: f32, y: f32| egui::pos2(x, y);
    let painter = ui.painter();

    // Soft glow behind the glass.
    painter.circle_filled(c, size * 0.55, theme::ACCENT.gamma_multiply(0.10));

    // Glass bulbs (translucent fill).
    let glass = theme::LINK.gamma_multiply(0.06);
    painter.add(egui::Shape::convex_polygon(
        vec![p(c.x - half, top), p(c.x + half, top), p(c.x, mid)],
        glass,
        egui::Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![p(c.x - half, bottom), p(c.x + half, bottom), p(c.x, mid)],
        glass,
        egui::Stroke::NONE,
    ));

    let sand = theme::ACCENT;
    let ht = mid - top;
    let hb = bottom - mid;

    // Top sand: fills down from the top to `level`.
    let level = top + ht * (1.0 - frac);
    let w0 = half * ((mid - level) / ht).clamp(0.0, 1.0);
    if w0 > 0.5 {
        painter.add(egui::Shape::convex_polygon(
            vec![p(c.x - w0, level), p(c.x + w0, level), p(c.x, mid)],
            sand,
            egui::Stroke::NONE,
        ));
    }

    // Bottom sand: accumulates up from the bottom to `level_b`.
    let level_b = bottom - hb * (1.0 - frac);
    let wb = half * ((level_b - mid) / hb).clamp(0.0, 1.0);
    if wb > 0.5 {
        painter.add(egui::Shape::convex_polygon(
            vec![
                p(c.x - half, bottom),
                p(c.x + half, bottom),
                p(c.x + wb, level_b),
                p(c.x - wb, level_b),
            ],
            sand,
            egui::Stroke::NONE,
        ));
    }

    // Falling stream + a few grains, while the sand is still moving.
    if frac > 0.02 && frac < 0.98 {
        let wobble = (time * 6.0).sin() as f32 * 0.6;
        painter.add(egui::Shape::convex_polygon(
            vec![
                p(c.x - 1.3 + wobble, level),
                p(c.x + 1.3 + wobble, level),
                p(c.x + 1.3, level_b),
                p(c.x - 1.3, level_b),
            ],
            sand,
            egui::Stroke::NONE,
        ));
        for k in 0..3 {
            let t = ((time * 1.3 + k as f64 * 0.34) % 1.0) as f32;
            let gy = level + (level_b - level) * t;
            painter.circle_filled(p(c.x + wobble, gy), 1.3, sand);
        }
    }

    // Outlines + caps on top of the sand.
    let edge = egui::Stroke::new(1.8_f32, theme::LINK.gamma_multiply(0.65));
    painter.add(egui::Shape::closed_line(
        vec![p(c.x - half, top), p(c.x + half, top), p(c.x, mid)],
        edge,
    ));
    painter.add(egui::Shape::closed_line(
        vec![p(c.x - half, bottom), p(c.x + half, bottom), p(c.x, mid)],
        edge,
    ));
    let cap = egui::Stroke::new(2.4_f32, theme::LINK.gamma_multiply(0.7));
    painter.line_segment([p(c.x - half, top), p(c.x + half, top)], cap);
    painter.line_segment([p(c.x - half, bottom), p(c.x + half, bottom)], cap);
}

/// Truncate a string to `max` characters, adding an ellipsis if cut.
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
        s
    }
}

/// Open an external URL with the system handler, restricted to safe schemes.
fn open_external(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

/// Render special fenced blocks (currently Mermaid). Returns `true` if handled.
fn render_special_fence(ui: &mut egui::Ui, lang: &str, code: &str) -> bool {
    if lang.eq_ignore_ascii_case("mermaid") {
        render_mermaid(ui, code);
        return true;
    }
    false
}

fn render_mermaid(ui: &mut egui::Ui, code: &str) {
    // Built-in flowchart rendering first (no external tools needed).
    if crate::mermaid::render(ui, code) {
        ui.add_space(4.0);
        return;
    }
    // Otherwise, try mermaid-cli for richer diagram types.
    if let Some(path) = mermaid_svg(code) {
        ui.add(
            egui::Image::new(format!("file://{}", path.display())).max_width(ui.available_width()),
        );
        ui.add_space(4.0);
        return;
    }
    // Fallback: a labelled source block.
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0_f32,
            ui.visuals().hyperlink_color.linear_multiply(0.5),
        ))
        .inner_margin(egui::Margin::same(8.0))
        .rounding(egui::Rounding::same(6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("mermaid")
                    .strong()
                    .small()
                    .color(ui.visuals().hyperlink_color),
            );
            ui.add(egui::Label::new(RichText::new(code.trim_end()).monospace().small()).wrap(true));
            ui.label(
                RichText::new("(unsupported diagram; showing source)")
                    .weak()
                    .small(),
            );
        });
    ui.add_space(4.0);
}

/// Render a Mermaid diagram to a cached SVG via `mmdc`, if available.
fn mermaid_svg(code: &str) -> Option<PathBuf> {
    // FNV-1a hash of the source, used to cache rendered diagrams.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in code.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let dir = config::dir().join("mermaid-cache");
    let out = dir.join(format!("{hash:016x}.svg"));
    if out.is_file() {
        return Some(out);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let mmd = dir.join(format!("{hash:016x}.mmd"));
    std::fs::write(&mmd, code).ok()?;
    let status = std::process::Command::new("mmdc")
        .arg("-i")
        .arg(&mmd)
        .arg("-o")
        .arg(&out)
        .arg("-b")
        .arg("transparent")
        .status()
        .ok();
    let _ = std::fs::remove_file(&mmd);
    match status {
        Some(s) if s.success() && out.is_file() => Some(out),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_color_image_decodes_a_png() {
        let img = image::RgbaImage::from_pixel(2, 1, image::Rgba([255, 0, 0, 255]));
        let dir = std::env::temp_dir().join(format!("encd-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.png");
        img.save(&path).unwrap();
        let ci = load_color_image(&path).unwrap();
        assert_eq!(ci.size, [2, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_file_confines_to_root() {
        let root = std::env::temp_dir().join(format!("encd-vt-{}", std::process::id()));
        let sub = root.join("assets");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.png"), b"x").unwrap();
        // Relative path inside the vault is allowed.
        assert!(vault_file(&root, "assets/a.png").is_some());
        // Absolute paths and parent traversal are refused.
        assert!(vault_file(&root, "/etc/passwd").is_none());
        assert!(vault_file(&root, "../outside.png").is_none());
        assert!(vault_file(&root, "assets/../../outside.png").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
