//! Graphite/blue visual system and shared widgets.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Response, RichText, Sense,
    Stroke, StrokeKind, TextStyle, Ui, Vec2,
};

pub const SURFACE: Color32 = Color32::from_rgb(0x14, 0x16, 0x19);
pub const PANEL: Color32 = Color32::from_rgb(0x1b, 0x1e, 0x22);
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x21, 0x25, 0x2a);
pub const FG: Color32 = Color32::from_rgb(0xe8, 0xea, 0xed);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xa6);
pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x35, 0x3a);
pub const ACCENT: Color32 = Color32::from_rgb(0x4d, 0xa3, 0xff);
/// Translucent selection band under a selected row or G-code line. Matches the
/// tool table's band, so a selection reads the same in every list.
pub const SELECTION_ALPHA: u8 = 34;
/// Solid bar on the rim of a selection.
pub const SELECTION_BAR: f32 = 3.0;
/// Corner radius of the selection band.
pub const SELECTION_RADIUS: f32 = 4.0;
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x0d, 0x2c, 0x4f);
pub const SUCCESS: Color32 = Color32::from_rgb(0x3d, 0xdc, 0x84);
pub const WARNING: Color32 = Color32::from_rgb(0xff, 0xb0, 0x20);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x5c, 0x5c);
pub const C1: Color32 = Color32::from_rgb(0x67, 0xe8, 0xf9);
pub const C2: Color32 = Color32::from_rgb(0xc0, 0x84, 0xfc);
pub const C3: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const C4: Color32 = Color32::from_rgb(0x34, 0xd3, 0x99);
pub const C5: Color32 = Color32::from_rgb(0xfb, 0x92, 0x3c);
pub const C6: Color32 = Color32::from_rgb(0xf4, 0x72, 0xb6);
pub const COMMENT: Color32 = Color32::from_rgb(0x94, 0xa3, 0xb8);
pub const LINE_NUMBER: Color32 = Color32::from_rgb(0x6b, 0x72, 0x80);
/// The MDI pane's block preview over MAIN's toolpath. Yellow is on none of the
/// path layers the card draws — accent blue cut, muted rapid, green live trail —
/// so the block under the caret cannot be read as one of them. The card's only
/// other warm marks are the amber tool and the current-line dot, both flat
/// markers rather than strokes.
pub const MDI_PREVIEW: Color32 = C3;

pub const TOP_BAR_HEIGHT: f32 = 48.0;
/// A single read-only status line: one text row plus a small frame margin.
/// It carries no buttons, so it does not need a target-sized row.
pub const BOTTOM_BAR_HEIGHT: f32 = 36.0;
pub const STATUS_LINE_HEIGHT: f32 = 20.0;
pub const NAV_WIDTH: f32 = 172.0;
pub const MIN_TARGET: f32 = 44.0;
/// Baseline layout is 1280x720. Text is enlarged slightly relative to the
/// 1000x640 SVG so it remains proportional when the shell is scaled.
pub const FONT_SCALE: f32 = 1.25;

/// SVG type ramp, scaled to the 1280x720 baseline. Keeping the ramp in one
/// place preserves the label/value/display ratios from the accepted SVGs.
const SVG_RAMP: [f32; 13] = [
    8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 22.0, 26.0, 30.0,
];

/// Width of `text` set at a ramp size, in points.
///
/// Columns are measured from the font rather than given magic numbers, so a
/// font-scale change moves the table with it.
pub fn measure(ui: &Ui, text: &str, base: f32) -> f32 {
    ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                text.to_owned(),
                FontId::monospace(font_size(base)),
                Color32::WHITE,
            )
            .size()
            .x
    })
}

/// Shared table geometry: cell padding either side of a measured column.
pub const CELL_PAD: f32 = 16.0;

pub fn font_size(base: f32) -> f32 {
    let mut nearest = SVG_RAMP[0];
    for value in SVG_RAMP {
        if (value - base).abs() <= (nearest - base).abs() {
            nearest = value;
        }
    }
    nearest * FONT_SCALE
}

/// Scale the whole shell from the 1280x720 baseline to the actual physical
/// viewport. `multiplier` is the profile/runtime UI scale preference.
pub fn apply_design_zoom(ctx: &egui::Context, multiplier: f32) {
    let rect = ctx.input(|input| input.viewport_rect());
    let ppp = ctx.pixels_per_point();
    let physical = rect.size() * ppp;
    if physical.x < 100.0 || physical.y < 100.0 {
        return;
    }
    let native = ctx.native_pixels_per_point().unwrap_or(1.0).max(0.1);
    let base = (physical.x / 1280.0).min(physical.y / 720.0);
    let target = (base / native * multiplier).clamp(0.5, 4.0);
    if (ctx.zoom_factor() - target).abs() > 0.005 {
        ctx.set_zoom_factor(target);
    }
}

/// Give proportional text the bundled Hack fallback. Ubuntu-Light, the default
/// proportional face, has no arrow glyphs (U+2190–U+2193); Hack does, so prose
/// and table cells can use typographic arrows instead of replacement boxes.
pub fn install_font_fallback(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if !family.iter().any(|name| name == "Hack") {
            family.insert(1, "Hack".to_owned());
        }
    }
    fonts
}

pub fn apply(ctx: &egui::Context) {
    // SPEC §5 requires a visible focus ring at all times. egui's default drops
    // focus when the operator clicks empty chrome, which would leave the shell
    // with no focus stop and no ring until Tab was pressed.
    ctx.options_mut(|options| {
        options.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never;
    });
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = SURFACE;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(0x0f, 0x11, 0x13);
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(FG);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, FG);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = ERROR;
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG);
    visuals.widgets.inactive.bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.weak_bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, FG);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2a, 0x30, 0x37);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2a, 0x30, 0x37);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, FG);
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::from_rgb(0x0b, 0x17, 0x22));
    visuals.widgets.open.bg_fill = PANEL_RAISED;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(6);
    visuals.menu_corner_radius = CornerRadius::same(6);
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(10.0, 7.0);
        style.spacing.interact_size.y = MIN_TARGET;
        style.spacing.slider_width = 180.0;
        style.spacing.combo_width = 160.0;
        style.spacing.indent = 14.0;
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(font_size(9.0)));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(font_size(12.0)));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(font_size(12.0)));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(font_size(16.0)));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(font_size(11.0)));
    });
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(12))
        .show(ui, add)
        .inner
}

pub fn raised_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(PANEL_RAISED)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(12))
        .show(ui, add)
        .inner
}

pub fn section_header(ui: &mut Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(font_size(12.0))
            .strong()
            .color(MUTED),
    );
}

pub fn mono(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).monospace().size(size).color(FG)
}

pub fn label_value(ui: &mut Ui, label: &str, value: impl Into<String>, size: f32) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).size(font_size(9.0)).color(MUTED));
        ui.label(mono(value, size));
    });
}

pub fn chip(ui: &mut Ui, text: impl Into<String>, active: bool) -> Response {
    let text = text.into();
    let button = egui::Button::new(RichText::new(text).size(font_size(10.0)).strong().color(
        if active {
            Color32::from_rgb(0x0b, 0x17, 0x22)
        } else {
            FG
        },
    ))
    .fill(if active { ACCENT } else { PANEL_RAISED })
    .stroke(Stroke::new(1.0, if active { ACCENT } else { BORDER }))
    .corner_radius(CornerRadius::same(14))
    .min_size(Vec2::new(0.0, 32.0));
    ui.add(button)
}

pub fn keycap(ui: &mut Ui, label: &str, active: bool) -> Response {
    let response =
        ui.add_sized(
            [58.0, 32.0],
            egui::Button::new(RichText::new(label).size(font_size(9.0)).strong().color(
                if active {
                    Color32::from_rgb(0x0b, 0x17, 0x22)
                } else {
                    FG
                },
            ))
            .fill(if active {
                ACCENT
            } else {
                Color32::from_rgb(0x24, 0x29, 0x2f)
            })
            .stroke(Stroke::new(1.0, if active { ACCENT } else { BORDER }))
            .corner_radius(CornerRadius::same(4)),
        );
    focus_ring(ui, &response);
    response
}

pub fn action_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).strong().color(if enabled {
            FG
        } else {
            Color32::from_rgb(0x5b, 0x60, 0x66)
        }))
        .min_size(Vec2::new(96.0, MIN_TARGET))
        .fill(if enabled { PANEL_RAISED } else { PANEL })
        .stroke(Stroke::new(
            1.0,
            if enabled {
                BORDER
            } else {
                Color32::from_rgb(0x25, 0x28, 0x2c)
            },
        )),
    );
    focus_ring(ui, &response);
    response
}

pub fn accent_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).strong().color(if enabled {
            Color32::from_rgb(0x0b, 0x17, 0x22)
        } else {
            MUTED
        }))
        .min_size(Vec2::new(120.0, MIN_TARGET))
        .fill(if enabled { ACCENT } else { PANEL })
        .stroke(Stroke::new(1.0, if enabled { ACCENT } else { BORDER })),
    );
    focus_ring(ui, &response);
    response
}

pub fn danger_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).strong().color(if enabled {
            Color32::from_rgb(0x1a, 0x0b, 0x0b)
        } else {
            MUTED
        }))
        .min_size(Vec2::new(120.0, MIN_TARGET))
        .fill(if enabled { ERROR } else { PANEL })
        .stroke(Stroke::new(1.0, if enabled { ERROR } else { BORDER })),
    );
    focus_ring(ui, &response);
    response
}

pub fn focus_ring(ui: &Ui, response: &Response) {
    if response.has_focus() {
        let rect = response.rect.expand(2.0);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(5),
            Stroke::new(2.0, ACCENT),
            StrokeKind::Outside,
        );
    }
}

pub fn status_dot(ui: &mut Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.label(RichText::new(text).size(font_size(11.0)).color(FG));
    });
}

/// Horizontal gauge with a numeric value.
pub fn meter(ui: &mut Ui, label: &str, value: f64, text: impl Into<String>, color: Color32) {
    let text = text.into();
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(font_size(9.0)).color(MUTED));
        let width = (ui.available_width() - 74.0).max(60.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 12.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius::same(6),
            Color32::from_rgb(0x0f, 0x11, 0x13),
        );
        let fill = rect.width() * (value.clamp(0.0, 1.0) as f32);
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, Vec2::new(fill, rect.height())),
            CornerRadius::same(6),
            color,
        );
        painter.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, BORDER),
            StrokeKind::Inside,
        );
        ui.label(
            RichText::new(text)
                .monospace()
                .size(font_size(11.0))
                .color(FG),
        );
    });
}

pub fn vertical_meter(ui: &mut Ui, label: &str, value: f64, height: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(28.0, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        CornerRadius::same(6),
        Color32::from_rgb(0x0f, 0x11, 0x13),
    );
    let fill_height = rect.height() * (value.clamp(0.0, 1.0) as f32);
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::Pos2::new(rect.left(), rect.bottom() - fill_height),
            Vec2::new(rect.width(), fill_height),
        ),
        CornerRadius::same(6),
        ACCENT,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, BORDER),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center_bottom() + Vec2::new(0.0, 4.0),
        Align2::CENTER_TOP,
        label,
        FontId::monospace(font_size(9.0)),
        MUTED,
    );
    response
}

pub fn kv_row(ui: &mut Ui, key: &str, value: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(font_size(11.0)).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(value.into())
                    .monospace()
                    .size(font_size(12.0))
                    .color(FG),
            );
        });
    });
}

pub fn banner(ui: &mut Ui, color: Color32, text: &str) {
    Frame::new()
        .fill(color.linear_multiply(0.22))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.label(RichText::new(text).strong().color(color));
        });
}

/// A persistent banner with one operator action on the right.
///
/// The action is the only way to clear a latched fault; the banner itself has
/// no timer and does not disappear when the toast or message strip expires.
pub fn banner_action(ui: &mut Ui, color: Color32, text: &str, action: &str) -> bool {
    let mut clicked = false;
    Frame::new()
        .fill(color.linear_multiply(0.22))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(text).strong().color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    clicked = action_button(ui, action, true).clicked();
                });
            });
        });
    clicked
}

pub fn separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, BORDER),
    );
}

pub fn format_position(value: f64, decimals: usize) -> String {
    if value.is_finite() {
        format!("{value:+.*}", decimals)
    } else {
        "---".into()
    }
}

pub fn color_for_status(ok: bool) -> Color32 {
    if ok {
        SUCCESS
    } else {
        ERROR
    }
}

pub fn pressure_color(fraction: f64) -> Color32 {
    if fraction >= 0.9 {
        ERROR
    } else if fraction >= 0.7 {
        WARNING
    } else {
        SUCCESS
    }
}

/// A translucent version of `color`, for selection bands.
pub fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

#[cfg(test)]
mod font_tests {
    use super::font_definitions;

    #[test]
    fn proportional_family_falls_back_to_hack() {
        let fonts = font_definitions();
        assert!(fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .is_some_and(|family| family.iter().any(|name| name == "Hack")));
    }
}
