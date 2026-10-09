// SPDX-License-Identifier: GPL-3.0-only
// Exercises the DiPlay-derived NTB16 datagram table codec.
#![no_main]
use carplay_protocol::ncm;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(131_072)];
    if let Ok(frames) = ncm::decode(data) {
        for frame in frames {
            assert!(frame.len() <= data.len());
        }
    }
    if let Ok(encoded) = ncm::encode(data, 42) {
        let frames = ncm::decode(&encoded).unwrap();
        assert_eq!(frames, vec![data]);
    }
});
