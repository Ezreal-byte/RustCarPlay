// SPDX-License-Identifier: GPL-3.0-only
// Wire format derived from DiPlay airplay/AirPlayHid.kt.
pub const TOUCH_UID: u32 = 0x2a2a2a2a;
pub const KNOB_UID: u32 = 0x2a2a2a2b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Clockwise90,
    Half,
    Clockwise270,
}

#[derive(Clone, Copy, Debug)]
pub struct Contact {
    pub x: f32,
    pub y: f32,
    pub down: bool,
}

/// Coordinates normalized within the actual displayed video rectangle, not the whole window.
pub fn touch_report(contacts: &[Contact], width: u16, height: u16, rotation: Rotation) -> [u8; 12] {
    let mut report = [0u8; 12];
    report[6] = 1;
    for (slot, contact) in contacts.iter().take(2).enumerate() {
        if !contact.x.is_finite() || !contact.y.is_finite() {
            continue;
        }
        let (x, y) = (contact.x.clamp(0., 1.), contact.y.clamp(0., 1.));
        let (x, y) = match rotation {
            Rotation::None => (x, y),
            Rotation::Clockwise90 => (y, 1. - x),
            Rotation::Half => (1. - x, 1. - y),
            Rotation::Clockwise270 => (1. - y, x),
        };
        let offset = slot * 6;
        report[offset + 1] = u8::from(contact.down);
        report[offset + 2..offset + 4]
            .copy_from_slice(&((x * width as f32).round() as u16).to_le_bytes());
        report[offset + 4..offset + 6]
            .copy_from_slice(&((y * height as f32).round() as u16).to_le_bytes());
    }
    report
}

pub fn knob_report(select: bool, home: bool, back: bool, x: i32, y: i32, wheel: i32) -> [u8; 4] {
    [
        u8::from(select) | (u8::from(home) << 1) | (u8::from(back) << 2),
        x.clamp(-127, 127) as u8,
        y.clamp(-127, 127) as u8,
        wheel.clamp(-127, 127) as u8,
    ]
}

pub fn touch_descriptor(width: u16, height: u16) -> Vec<u8> {
    let mut b = vec![0x05, 0x0d, 0x09, 0x04, 0xa1, 0x01];
    for _ in 0..2 {
        b.extend_from_slice(&[
            0x05,
            0x0d,
            0x09,
            0x22,
            0xa1,
            0x02,
            0x09,
            0x38,
            0x75,
            0x08,
            0x95,
            0x01,
            0x81,
            0x02,
            0x15,
            0x00,
            0x25,
            0x01,
            0x09,
            0x33,
            0x75,
            0x01,
            0x95,
            0x01,
            0x81,
            0x02,
            0x95,
            0x07,
            0x81,
            0x03,
            0x05,
            0x01,
            0x26,
            width as u8,
            (width >> 8) as u8,
            0x09,
            0x30,
            0x75,
            0x10,
            0x95,
            0x01,
            0x81,
            0x02,
            0x26,
            height as u8,
            (height >> 8) as u8,
            0x09,
            0x31,
            0x81,
            0x02,
            0xc0,
        ]);
    }
    b.push(0xc0);
    b
}

pub const KNOB_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x08, 0xa1, 0x01, 0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01,
    0x95, 0x01, 0x81, 0x02, 0x05, 0x0c, 0x0a, 0x23, 0x02, 0x0a, 0x24, 0x02, 0x95, 0x02, 0x81, 0x02,
    0x95, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81,
    0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02, 0xc0, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75,
    0x08, 0x95, 0x01, 0x81, 0x06, 0xc0,
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_and_release_are_exact() {
        let r = touch_report(
            &[Contact {
                x: 0.,
                y: 1.,
                down: true,
            }],
            1280,
            720,
            Rotation::Clockwise90,
        );
        assert_eq!(r, [0, 1, 0, 5, 208, 2, 1, 0, 0, 0, 0, 0]);
        assert_eq!(
            touch_report(&[], 1280, 720, Rotation::None),
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            knob_report(false, true, true, 999, -999, -1),
            [6, 127, 129, 255]
        );
    }
}
