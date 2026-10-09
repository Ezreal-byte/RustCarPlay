// SPDX-License-Identifier: GPL-3.0-only
//! Translate ordered pointer events, preserving the final contact position on release.
use carplay_core::input::Contact;
use eframe::egui::{Event, PointerButton, Pos2, Rect};

#[derive(Default)]
pub struct TouchInput {
    position: Option<Pos2>,
}

impl TouchInput {
    pub fn release(&mut self) -> Option<Contact> {
        self.position.take().map(|p| Contact {
            x: p.x,
            y: p.y,
            down: false,
        })
    }

    pub fn process(
        &mut self,
        rect: Rect,
        events: &[Event],
        primary_down: bool,
        focused: bool,
    ) -> Vec<Contact> {
        if !focused || !rect.is_positive() {
            return self.release().into_iter().collect();
        }
        let normalized = |p: Pos2| {
            let p = (p - rect.min) / rect.size();
            Pos2::new(p.x.clamp(0., 1.), p.y.clamp(0., 1.))
        };
        let contact = |p: Pos2| Contact {
            x: p.x,
            y: p.y,
            down: true,
        };
        let mut output = Vec::new();
        let mut moved = None;
        for event in events {
            match *event {
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    ..
                } => {
                    if self.position.is_none() && rect.contains(pos) {
                        let p = normalized(pos);
                        self.position = Some(p);
                        output.push(contact(p));
                    }
                }
                Event::PointerMoved(pos) if self.position.is_some() => {
                    let p = normalized(pos);
                    if self.position != Some(p) {
                        self.position = Some(p);
                        // A slow frame may contain hundreds of motion samples. Keep its latest
                        // position without discarding any press/release transition.
                        moved = Some(contact(p));
                    }
                }
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    ..
                } => {
                    if self.position.is_some() && rect.contains(pos) {
                        self.position = Some(normalized(pos));
                    }
                    if let Some(last_move) = moved.take() {
                        output.push(last_move);
                    }
                    if let Some(release) = self.release() {
                        output.push(release);
                    }
                }
                Event::PointerGone | Event::WindowFocused(false) => {
                    if let Some(last_move) = moved.take() {
                        output.push(last_move);
                    }
                    if let Some(release) = self.release() {
                        output.push(release);
                    }
                }
                _ => {}
            }
        }
        if let Some(last_move) = moved {
            output.push(last_move);
        }
        // Also release if the OS lost a button-up event, always at the saved position.
        if !primary_down && let Some(release) = self.release() {
            output.push(release);
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carplay_core::input::{Rotation, touch_report};
    use eframe::egui::{Modifiers, pos2, vec2};
    fn rect() -> Rect {
        Rect::from_min_size(pos2(100., 50.), vec2(640., 360.))
    }
    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }
    }
    fn report(c: Contact) -> [u8; 12] {
        touch_report(&[c], 1280, 720, Rotation::None)
    }

    #[test]
    fn a_complete_fast_tap_in_one_frame_keeps_press_and_release_at_same_pixel() {
        let mut input = TouchInput::default();
        let output = input.process(
            rect(),
            &[
                button(pos2(420., 230.), true),
                button(pos2(420., 230.), false),
            ],
            false,
            true,
        );
        assert_eq!(output.len(), 2);
        assert_eq!(report(output[0]), [0, 1, 128, 2, 104, 1, 1, 0, 0, 0, 0, 0]);
        assert_eq!(report(output[1]), [0, 0, 128, 2, 104, 1, 1, 0, 0, 0, 0, 0]);
        assert!(input.release().is_none());
    }

    #[test]
    fn focus_loss_and_panel_exit_release_the_last_position_once() {
        let mut input = TouchInput::default();
        input.process(rect(), &[button(pos2(260., 140.), true)], true, true);
        let output = input.process(rect(), &[], false, false);
        assert_eq!(output.len(), 1);
        assert_eq!(report(output[0]), [0, 0, 64, 1, 180, 0, 1, 0, 0, 0, 0, 0]);
        assert!(input.release().is_none());
        input.process(rect(), &[button(pos2(420., 230.), true)], true, true);
        assert_eq!(
            report(input.release().unwrap()),
            [0, 0, 128, 2, 104, 1, 1, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn dragging_coalesces_motion_but_preserves_edges_and_does_not_start_outside_video() {
        let mut input = TouchInput::default();
        assert!(
            input
                .process(
                    rect(),
                    &[
                        button(pos2(20., 20.), true),
                        Event::PointerMoved(pos2(200., 100.))
                    ],
                    true,
                    true
                )
                .is_empty()
        );
        let mut events = vec![button(pos2(100., 50.), true)];
        for x in 101..740 {
            events.push(Event::PointerMoved(pos2(x as f32, 410.)));
        }
        events.push(button(pos2(900., 800.), false));
        let output = input.process(rect(), &events, false, true);
        assert_eq!(output.len(), 3);
        assert!(output[0].down && output[1].down && !output[2].down);
        assert_eq!(&report(output[1])[2..6], &report(output[2])[2..6]);
        assert!(input.release().is_none());
    }
}
