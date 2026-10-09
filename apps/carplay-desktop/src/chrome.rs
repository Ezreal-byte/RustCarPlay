// SPDX-License-Identifier: GPL-3.0-only
use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, ResizeDirection, Sense,
    Stroke, StrokeKind, Vec2, ViewportCommand,
};

const BACKGROUND: Color32 = Color32::from_rgb(17, 22, 25);
const TEXT: Color32 = Color32::from_rgb(233, 240, 236);
const MUTED: Color32 = Color32::from_rgb(132, 149, 140);
const EDGE: f32 = 4.0;
const HEIGHT: f32 = 48.0;
const BUTTON_WIDTH: f32 = 48.0;

/// Draw the app's native window controls. The caller hides this in full screen.
pub fn title_bar(ctx: &egui::Context, logo: &egui::TextureHandle, subtitle: &str) {
    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
    egui::TopBottomPanel::top("window_title_bar")
        .exact_height(HEIGHT)
        .frame(egui::Frame::NONE.fill(BACKGROUND))
        .show(ctx, |ui| {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::hover());
            let controls_left = rect.right() - BUTTON_WIDTH * 3.0;
            let drag_rect = Rect::from_min_max(
                rect.min + Vec2::splat(EDGE),
                Pos2::new(controls_left, rect.bottom()),
            );
            let drag = ui.interact(drag_rect, ui.id().with("drag"), Sense::click_and_drag());
            if drag.double_clicked_by(PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            } else if drag.drag_started_by(PointerButton::Primary) {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }

            let painter = ui.painter().with_clip_rect(drag_rect);
            painter.image(
                logo.id(),
                Rect::from_center_size(
                    Pos2::new(rect.left() + 30.0, rect.center().y),
                    Vec2::splat(24.0),
                ),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            let name = painter.text(
                Pos2::new(rect.left() + 53.0, rect.center().y),
                Align2::LEFT_CENTER,
                "RustCarPlay",
                FontId::proportional(16.0),
                TEXT,
            );
            if !subtitle.is_empty() {
                painter.line_segment(
                    [
                        Pos2::new(name.right() + 18.0, rect.center().y - 7.0),
                        Pos2::new(name.right() + 18.0, rect.center().y + 7.0),
                    ],
                    Stroke::new(1.0, Color32::from_rgb(50, 63, 57)),
                );
                painter.text(
                    Pos2::new(name.right() + 34.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    subtitle,
                    FontId::proportional(13.0),
                    MUTED,
                );
            }

            for (index, control) in [Control::Minimize, Control::Maximize, Control::Close]
                .into_iter()
                .enumerate()
            {
                let button = Rect::from_min_size(
                    Pos2::new(
                        controls_left + index as f32 * BUTTON_WIDTH,
                        rect.top() + EDGE,
                    ),
                    Vec2::new(
                        BUTTON_WIDTH - if index == 2 { EDGE } else { 0.0 },
                        HEIGHT - EDGE,
                    ),
                );
                let response = ui
                    .interact(button, ui.id().with(index), Sense::click())
                    .on_hover_text(match control {
                        Control::Minimize => "最小化",
                        Control::Maximize if maximized => "还原窗口",
                        Control::Maximize => "最大化",
                        Control::Close => "关闭",
                    });
                let color = if response.hovered() && matches!(control, Control::Close) {
                    ui.painter()
                        .rect_filled(button, 4, Color32::from_rgb(191, 52, 64));
                    Color32::WHITE
                } else if response.hovered() || response.is_pointer_button_down_on() {
                    ui.painter()
                        .rect_filled(button, 4, Color32::from_rgb(38, 49, 44));
                    TEXT
                } else {
                    MUTED
                };
                control.paint(ui.painter(), button.center(), color, maximized);
                if response.clicked_by(PointerButton::Primary) {
                    ctx.send_viewport_cmd(match control {
                        Control::Minimize => ViewportCommand::Minimized(true),
                        Control::Maximize => ViewportCommand::Maximized(!maximized),
                        Control::Close => ViewportCommand::Close,
                    });
                }
            }
        });
}

enum Control {
    Minimize,
    Maximize,
    Close,
}

impl Control {
    fn paint(&self, painter: &egui::Painter, center: Pos2, color: Color32, maximized: bool) {
        let stroke = Stroke::new(1.2, color);
        match self {
            Self::Minimize => {
                painter.line_segment(
                    [center - Vec2::new(5.0, 0.0), center + Vec2::new(5.0, 0.0)],
                    stroke,
                );
            }
            Self::Maximize if maximized => {
                let back = Rect::from_center_size(center + Vec2::new(1.5, -1.5), Vec2::splat(8.0));
                let front = back.translate(Vec2::new(-3.0, 3.0));
                painter.line_segment(
                    [Pos2::new(back.left(), front.top()), back.left_top()],
                    stroke,
                );
                painter.line_segment([back.left_top(), back.right_top()], stroke);
                painter.line_segment([back.right_top(), back.right_bottom()], stroke);
                painter.line_segment(
                    [back.right_bottom(), Pos2::new(front.right(), back.bottom())],
                    stroke,
                );
                painter.rect_stroke(front, 0, stroke, StrokeKind::Inside);
            }
            Self::Maximize => {
                painter.rect_stroke(
                    Rect::from_center_size(center, Vec2::splat(10.0)),
                    0,
                    stroke,
                    StrokeKind::Inside,
                );
            }
            Self::Close => {
                painter.line_segment(
                    [center - Vec2::splat(5.0), center + Vec2::splat(5.0)],
                    stroke,
                );
                painter.line_segment(
                    [center + Vec2::new(-5.0, 5.0), center + Vec2::new(5.0, -5.0)],
                    stroke,
                );
            }
        }
    }
}

/// Restore the resize handles lost when native window decorations are disabled.
/// Keep interactive app content inset by at least 4 points from the window edges.
pub fn resize_edges(ctx: &egui::Context) {
    let rect = ctx.viewport_rect();
    let direction = ctx.input(|input| {
        let viewport = input.viewport();
        if viewport.fullscreen.unwrap_or(false) || viewport.maximized.unwrap_or(false) {
            return None;
        }
        resize_direction(rect, input.pointer.hover_pos()?)
    });
    let Some(direction) = direction else { return };
    ctx.set_cursor_icon(match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
    });
    if ctx.input(|input| input.pointer.button_pressed(PointerButton::Primary)) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

fn resize_direction(rect: Rect, pointer: Pos2) -> Option<ResizeDirection> {
    if !rect.contains(pointer) {
        return None;
    }
    let left = pointer.x - rect.left();
    let right = rect.right() - pointer.x;
    let top = pointer.y - rect.top();
    let bottom = rect.bottom() - pointer.y;
    if left > EDGE && right > EDGE && top > EDGE && bottom > EDGE {
        return None;
    }
    // Slightly longer corners stay within the narrow border, while making
    // diagonal resizing easier than targeting a 4-by-4-point square.
    let corner = EDGE * 3.0;
    match (
        left <= corner,
        right <= corner,
        top <= corner,
        bottom <= corner,
    ) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (_, _, true, _) if top <= EDGE => Some(ResizeDirection::North),
        (_, _, _, true) if bottom <= EDGE => Some(ResizeDirection::South),
        (true, _, _, _) if left <= EDGE => Some(ResizeDirection::West),
        (_, true, _, _) if right <= EDGE => Some(ResizeDirection::East),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TitleBarHarness {
        ctx: egui::Context,
        logo: egui::TextureHandle,
        time: f64,
        maximized: bool,
    }

    impl TitleBarHarness {
        fn new(maximized: bool) -> Self {
            let ctx = egui::Context::default();
            let logo = ctx.load_texture(
                "test-logo",
                egui::ColorImage::filled([1, 1], Color32::GREEN),
                egui::TextureOptions::LINEAR,
            );
            let mut harness = Self {
                ctx,
                logo,
                time: 0.0,
                maximized,
            };
            // Register the widgets before delivering input, as a real eframe
            // window does on its initial repaint.
            harness.frame(vec![]);
            harness
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<ViewportCommand> {
            self.time += 0.02;
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .maximized = Some(self.maximized);
            let mut output = self.ctx.run(input, |ctx| {
                title_bar(ctx, &self.logo, "连接与设置");
                resize_edges(ctx);
            });
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .unwrap()
                .commands
        }

        fn button(&mut self, pos: Pos2, pressed: bool) -> Vec<ViewportCommand> {
            self.frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        }
    }

    #[test]
    fn native_buttons_click_without_starting_window_drag() {
        // Exercise pointer input across frames rather than calling a button's
        // command mapping directly. This catches overlapping drag regions.
        for (x, maximized, expected) in [
            (780.0, false, ViewportCommand::Minimized(true)),
            (828.0, false, ViewportCommand::Maximized(true)),
            (828.0, true, ViewportCommand::Maximized(false)),
            (874.0, false, ViewportCommand::Close),
        ] {
            let mut harness = TitleBarHarness::new(maximized);
            let position = Pos2::new(x, 24.0);
            assert!(harness.button(position, true).is_empty());
            let commands = harness.button(position, false);
            assert_eq!(commands, vec![expected]);
            assert!(!commands.contains(&ViewportCommand::StartDrag));
        }
    }

    #[test]
    fn title_double_click_maximizes_and_drag_moves_the_window() {
        let position = Pos2::new(400.0, 24.0);
        let mut double_click = TitleBarHarness::new(false);
        assert!(double_click.button(position, true).is_empty());
        assert!(double_click.button(position, false).is_empty());
        assert!(double_click.button(position, true).is_empty());
        assert_eq!(
            double_click.button(position, false),
            vec![ViewportCommand::Maximized(true)]
        );

        let mut drag = TitleBarHarness::new(false);
        assert!(drag.button(position, true).is_empty());
        assert_eq!(
            drag.frame(vec![egui::Event::PointerMoved(
                position + Vec2::new(20.0, 0.0)
            )]),
            vec![ViewportCommand::StartDrag]
        );
        assert!(
            drag.button(position + Vec2::new(20.0, 0.0), false)
                .is_empty()
        );
    }

    #[test]
    fn border_resizing_does_not_capture_content_or_outside_pointer() {
        let rect = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(900.0, 700.0));
        for pointer in [
            rect.center(),
            rect.min + Vec2::splat(6.0),
            rect.min - Vec2::splat(1.0),
        ] {
            assert_eq!(resize_direction(rect, pointer), None);
        }
        assert_eq!(
            resize_direction(rect, Pos2::new(21.0, 39.0)),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(911.0, 31.0)),
            Some(ResizeDirection::NorthEast)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(29.0, 729.0)),
            Some(ResizeDirection::SouthWest)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(919.0, 721.0)),
            Some(ResizeDirection::SouthEast)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(450.0, 31.0)),
            Some(ResizeDirection::North)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(450.0, 729.0)),
            Some(ResizeDirection::South)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(21.0, 350.0)),
            Some(ResizeDirection::West)
        );
        assert_eq!(
            resize_direction(rect, Pos2::new(919.0, 350.0)),
            Some(ResizeDirection::East)
        );
    }
}
