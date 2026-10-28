//! The app-wide visual theme: a modern, rounded, Obsidian/Catppuccin-ish dark
//! scheme with a violet accent.

use eframe::egui::{self, Color32};

pub const BASE: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const WINDOW_FILL: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub const WIDGET: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const WIDGET_HOVER: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
pub const WIDGET_ACTIVE: Color32 = Color32::from_rgb(0x3d, 0x3d, 0x3d);
pub const ACCENT: Color32 = Color32::from_rgb(0x8a, 0x5c, 0xf5);
/// Neutral dark-grey fill for primary buttons (instead of the purple accent).
pub const PRIMARY: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(0x47, 0x47, 0x47);
pub const TEXT: Color32 = Color32::from_rgb(0xdc, 0xdd, 0xde);
pub const LINK: Color32 = Color32::from_rgb(0xa8, 0x95, 0xff);
pub const BORDER: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
pub const DANGER: Color32 = Color32::from_rgb(0xe0, 0x5f, 0x5f);
pub const SUCCESS: Color32 = Color32::from_rgb(0x7d, 0xdc, 0x8a);

/// Install fonts, spacing, and colours.
pub fn apply(ctx: &egui::Context, font_size: f32) {
    let size = font_size;
    let mut style = (*ctx.style()).clone();

    // Typography.
    use egui::TextStyle::*;
    style
        .text_styles
        .insert(Body, egui::FontId::proportional(size + 1.0));
    style
        .text_styles
        .insert(Monospace, egui::FontId::monospace(size));
    style
        .text_styles
        .insert(Button, egui::FontId::proportional(size));
    style
        .text_styles
        .insert(Small, egui::FontId::proportional(size - 3.0));
    style
        .text_styles
        .insert(Heading, egui::FontId::proportional(size + 7.0));

    // Spacing for a roomier, modern feel.
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(11.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.window_margin = egui::Margin::same(14.0);
    style.spacing.indent = 18.0;

    // Colors.
    let mut v = egui::Visuals::dark();
    v.panel_fill = BASE;
    v.window_fill = WINDOW_FILL;
    v.extreme_bg_color = Color32::from_rgb(0x1a, 0x1a, 0x1a);
    v.faint_bg_color = Color32::from_rgb(0x26, 0x26, 0x26);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = LINK;
    v.warn_fg_color = Color32::from_rgb(0xf9, 0xe2, 0xaf);
    v.error_fg_color = DANGER;
    v.selection.bg_fill = ACCENT.linear_multiply(0.55);
    v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
    v.window_rounding = egui::Rounding::same(8.0);
    v.menu_rounding = egui::Rounding::same(6.0);
    let shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 22.0,
        spread: 0.0,
        color: Color32::from_black_alpha(140),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;

    let r = egui::Rounding::same(5.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BASE;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    w.noninteractive.rounding = r;
    w.noninteractive.fg_stroke.color = TEXT;

    w.inactive.bg_fill = WIDGET;
    w.inactive.weak_bg_fill = WIDGET;
    w.inactive.bg_stroke = egui::Stroke::NONE;
    w.inactive.rounding = r;

    w.hovered.bg_fill = WIDGET_HOVER;
    w.hovered.weak_bg_fill = WIDGET_HOVER;
    w.hovered.bg_stroke = egui::Stroke::new(1.0_f32, ACCENT.linear_multiply(0.7));
    w.hovered.rounding = r;

    w.active.bg_fill = WIDGET_ACTIVE;
    w.active.weak_bg_fill = WIDGET_ACTIVE;
    w.active.rounding = r;

    w.open.bg_fill = WIDGET_ACTIVE;
    w.open.weak_bg_fill = WIDGET_ACTIVE;
    w.open.rounding = r;

    style.visuals = v;
    ctx.set_style(style);
}
