// SPDX-License-Identifier: GPL-3.0-only
// Exercises the DiPlay-derived RTSP parser with streaming and pipelining.
#![no_main]
use carplay_core::rtsp::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(MAX_HEADER + MAX_BODY + 1)];
    let mut decoder = Decoder::default();
    for chunk in data.chunks(257) {
        if decoder.push(chunk).is_err() {
            break;
        }
        loop {
            let before = decoder.buffered_len();
            match decoder.next_request() {
                Ok(Some(request)) => {
                    assert!(request.body.len() <= MAX_BODY);
                    assert!(decoder.buffered_len() < before);
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        assert!(decoder.buffered_len() <= MAX_HEADER + MAX_BODY);
    }
});
