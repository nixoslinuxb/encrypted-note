//! The app's icon set.
//!
//! Every icon is a small monochrome SVG file under `assets/icons/`, embedded
//! at compile time and rasterised on the fly by `egui_extras`' SVG loader. The
//! SVGs are authored with white strokes so a single `tint` can recolour them to
//! match the current UI state (normal / hovered / selected / disabled).

use eframe::egui::{self, Color32, Image, ImageButton, Response, Ui, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Add,
    Save,
    FolderOpen,
    Refresh,
    Lock,
    Search,
    Close,
    Check,
    Edit,
    Split,
    Preview,
    Link,
    Backlink,
    Folder,
    File,
    Trash,
    Home,
    ArrowUp,
    ChevronRight,
    ChevronDown,
    ChevronLeft,
    Today,
    Rss,
    Bookmark,
    Image,
    Mic,
    Video,
    Warning,
    Key,
    Generate,
    PanelRight,
    Recent,
    Dot,
    Tag,
    Graph,
    Live,
    Undo,
    Redo,
    Play,
    Pause,
    Stop,
    Volume,
    Passwords,
    Copy,
    Star,
    Eye,
    EyeOff,
    Download,
    Upload,
    Dice,
    FolderPlus,
    CheckCircle,
}

impl Icon {
    /// Every icon, for tests and exhaustiveness.
    #[cfg(test)]
    pub const ALL: [Icon; 52] = [
        Icon::Add,
        Icon::Save,
        Icon::FolderOpen,
        Icon::Refresh,
        Icon::Lock,
        Icon::Search,
        Icon::Close,
        Icon::Check,
        Icon::Edit,
        Icon::Split,
        Icon::Preview,
        Icon::Link,
        Icon::Backlink,
        Icon::Folder,
        Icon::File,
        Icon::Trash,
        Icon::Home,
        Icon::ArrowUp,
        Icon::ChevronRight,
        Icon::ChevronDown,
        Icon::ChevronLeft,
        Icon::Today,
        Icon::Rss,
        Icon::Bookmark,
        Icon::Image,
        Icon::Mic,
        Icon::Video,
        Icon::Warning,
        Icon::Key,
        Icon::Generate,
        Icon::PanelRight,
        Icon::Recent,
        Icon::Dot,
        Icon::Tag,
        Icon::Graph,
        Icon::Live,
        Icon::Undo,
        Icon::Redo,
        Icon::Play,
        Icon::Pause,
        Icon::Stop,
        Icon::Volume,
        Icon::Passwords,
        Icon::Copy,
        Icon::Star,
        Icon::Eye,
        Icon::EyeOff,
        Icon::Download,
        Icon::Upload,
        Icon::Dice,
        Icon::FolderPlus,
        Icon::CheckCircle,
    ];
}

impl Icon {
    /// The embedded SVG source for this icon.
    pub fn source(self) -> egui::ImageSource<'static> {
        use Icon::*;
        match self {
            Add => egui::include_image!("../assets/icons/plus.svg"),
            Save => egui::include_image!("../assets/icons/save.svg"),
            FolderOpen => egui::include_image!("../assets/icons/folder-open.svg"),
            Refresh => egui::include_image!("../assets/icons/refresh.svg"),
            Lock => egui::include_image!("../assets/icons/lock.svg"),
            Search => egui::include_image!("../assets/icons/search.svg"),
            Close => egui::include_image!("../assets/icons/close.svg"),
            Check => egui::include_image!("../assets/icons/check.svg"),
            Edit => egui::include_image!("../assets/icons/edit.svg"),
            Split => egui::include_image!("../assets/icons/split.svg"),
            Preview => egui::include_image!("../assets/icons/preview.svg"),
            Link => egui::include_image!("../assets/icons/link.svg"),
            Backlink => egui::include_image!("../assets/icons/backlink.svg"),
            Folder => egui::include_image!("../assets/icons/folder.svg"),
            File => egui::include_image!("../assets/icons/file.svg"),
            Trash => egui::include_image!("../assets/icons/trash.svg"),
            Home => egui::include_image!("../assets/icons/home.svg"),
            ArrowUp => egui::include_image!("../assets/icons/arrow-up.svg"),
            ChevronRight => egui::include_image!("../assets/icons/chevron-right.svg"),
            ChevronDown => egui::include_image!("../assets/icons/chevron-down.svg"),
            ChevronLeft => egui::include_image!("../assets/icons/chevron-left.svg"),
            Today => egui::include_image!("../assets/icons/today.svg"),
            Rss => egui::include_image!("../assets/icons/rss.svg"),
            Bookmark => egui::include_image!("../assets/icons/bookmark.svg"),
            Image => egui::include_image!("../assets/icons/image.svg"),
            Mic => egui::include_image!("../assets/icons/mic.svg"),
            Video => egui::include_image!("../assets/icons/video.svg"),
            Warning => egui::include_image!("../assets/icons/warning.svg"),
            Key => egui::include_image!("../assets/icons/key.svg"),
            Generate => egui::include_image!("../assets/icons/sparkles.svg"),
            PanelRight => egui::include_image!("../assets/icons/panel-right.svg"),
            Recent => egui::include_image!("../assets/icons/recent.svg"),
            Dot => egui::include_image!("../assets/icons/dot.svg"),
            Tag => egui::include_image!("../assets/icons/tag.svg"),
            Graph => egui::include_image!("../assets/icons/graph.svg"),
            Live => egui::include_image!("../assets/icons/live.svg"),
            Undo => egui::include_image!("../assets/icons/undo.svg"),
            Redo => egui::include_image!("../assets/icons/redo.svg"),
            Play => egui::include_image!("../assets/icons/play.svg"),
            Pause => egui::include_image!("../assets/icons/pause.svg"),
            Stop => egui::include_image!("../assets/icons/stop.svg"),
            Volume => egui::include_image!("../assets/icons/volume.svg"),
            Passwords => egui::include_image!("../assets/icons/passwords.svg"),
            Copy => egui::include_image!("../assets/icons/copy.svg"),
            Star => egui::include_image!("../assets/icons/star.svg"),
            Eye => egui::include_image!("../assets/icons/eye.svg"),
            EyeOff => egui::include_image!("../assets/icons/eye-off.svg"),
            Download => egui::include_image!("../assets/icons/download.svg"),
            Upload => egui::include_image!("../assets/icons/upload.svg"),
            Dice => egui::include_image!("../assets/icons/dice.svg"),
            FolderPlus => egui::include_image!("../assets/icons/folder-plus.svg"),
            CheckCircle => egui::include_image!("../assets/icons/check-circle.svg"),
        }
    }

    /// A tinted image at an exact pixel size.
    pub fn image(self, size: f32, tint: Color32) -> Image<'static> {
        Image::new(self.source())
            .fit_to_exact_size(Vec2::splat(size))
            .tint(tint)
    }
}

/// Draw a non-interactive icon.
pub fn icon(ui: &mut Ui, ic: Icon, size: f32, tint: Color32) -> Response {
    ui.add(ic.image(size, tint))
}

/// Colour an icon takes for a given interaction state.
fn tint_for(ui: &Ui, enabled: bool, selected: bool, danger: bool) -> Color32 {
    if !enabled {
        ui.visuals().weak_text_color()
    } else if danger {
        super::theme::DANGER
    } else if selected {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().text_color()
    }
}

/// A clickable icon button. `title` is shown as a hover tooltip (pass `""` for
/// none). Returns the button's [`Response`].
pub fn button(
    ui: &mut Ui,
    ic: Icon,
    size: f32,
    enabled: bool,
    selected: bool,
    title: &str,
) -> Response {
    button_styled(ui, ic, size, enabled, selected, false, title)
}

/// Like [`button`], but tints the icon with the danger colour.
pub fn danger_button(ui: &mut Ui, ic: Icon, size: f32, enabled: bool, title: &str) -> Response {
    button_styled(ui, ic, size, enabled, false, true, title)
}

fn button_styled(
    ui: &mut Ui,
    ic: Icon,
    size: f32,
    enabled: bool,
    selected: bool,
    danger: bool,
    title: &str,
) -> Response {
    let tint = tint_for(ui, enabled, selected, danger);
    let btn = ImageButton::new(ic.image(size, tint))
        .frame(selected)
        .rounding(egui::Rounding::same(7.0));
    let resp = ui.add_enabled(enabled, btn);
    if title.is_empty() {
        resp
    } else {
        resp.on_hover_text(title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    /// Every icon file must be a valid, synchronous-loadable SVG.
    #[test]
    fn every_icon_is_a_loadable_svg() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        for icon in Icon::ALL {
            let img = icon.image(16.0, egui::Color32::WHITE);
            match img.load_for_size(&ctx, egui::vec2(16.0, 16.0)) {
                Ok(egui::load::TexturePoll::Ready { .. }) => {}
                Ok(egui::load::TexturePoll::Pending { .. }) => {
                    panic!("{icon:?} unexpectedly still pending")
                }
                Err(e) => panic!("{icon:?} failed to load: {e}"),
            }
        }
    }
}
