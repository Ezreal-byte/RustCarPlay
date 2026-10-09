// SPDX-License-Identifier: GPL-3.0-only
use eframe::egui::{self, Color32, RichText};

pub const BACKGROUND: Color32 = Color32::from_rgb(15, 20, 23);
pub const SURFACE: Color32 = Color32::from_rgb(24, 31, 35);
pub const BORDER: Color32 = Color32::from_rgb(44, 56, 60);
pub const TEXT: Color32 = Color32::from_rgb(234, 241, 237);
pub const MUTED: Color32 = Color32::from_rgb(151, 167, 160);
pub const GREEN: Color32 = Color32::from_rgb(67, 221, 132);

pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = SURFACE;
    visuals.extreme_bg_color = Color32::from_rgb(14, 21, 24);
    visuals.faint_bg_color = SURFACE;
    visuals.selection.bg_fill = Color32::from_rgb(30, 89, 59);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, GREEN);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(34, 44, 48);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 44, 48);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(46, 63, 58);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 63, 58);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, GREEN);
    visuals.widgets.active.bg_fill = Color32::from_rgb(35, 84, 57);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.corner_radius = egui::CornerRadius::same(7);
    }
    ctx.set_visuals(visuals);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10., 8.);
        s.spacing.button_padding = egui::vec2(14., 8.);
        s.spacing.interact_size.y = 34.;
        s.spacing.combo_width = 160.;
        s.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.));
        s.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(15.));
        s.text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.));
        s.text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(22.));
    });
}

pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(16)
        .inner_margin(24)
}

pub fn muted(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(MUTED));
}

pub fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(RichText::new(label).size(13.).color(MUTED));
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(f32::INFINITY)
            .hint_text(hint)
            .margin(egui::vec2(10., 9.)),
    );
}

pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(
        RichText::new(label)
            .color(Color32::from_rgb(10, 37, 22))
            .strong(),
    )
    .fill(GREEN)
    .min_size(egui::vec2(140., 44.))
}
