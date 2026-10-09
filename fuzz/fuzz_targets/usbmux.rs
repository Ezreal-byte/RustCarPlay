// SPDX-License-Identifier: GPL-3.0-only
// Exercises the DiPlay-derived USB device framing and optional iOS reply trailer boundary.
#![no_main]
use carplay_protocol::usbmux::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(131_072)];
    if let Ok(frame) = Frame::decode(data) {
        assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
    }
    let mut decoder = Decoder::default();
    for chunk in data.chunks(31) {
        if decoder.push(chunk).is_err() {
            break;
        }
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => {
                    let _ = frame.encode().unwrap();
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        assert!(decoder.buffered_bytes() <= MAX_FRAME + 4);
    }
});
