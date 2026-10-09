// SPDX-License-Identifier: GPL-3.0-only
// Exercises the DiPlay-derived CSM, TLV and display metadata parsers.
#![no_main]
use carplay_protocol::{metadata, tlv::*};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(131_072)];
    if let Ok(parameters) = parse_parameters(data) {
        assert_eq!(encode_parameters(&parameters).unwrap(), data);
        for p in parameters {
            let _ = p.as_u8();
            let _ = p.as_u16();
            let _ = p.as_u32();
            let _ = p.as_str();
        }
    }
    if let Ok(message) = ControlMessage::decode(data) {
        assert_eq!(
            ControlMessage::decode(&message.encode().unwrap()).unwrap(),
            message
        );
        let _ = metadata::decode(&message);
    }
    let mut decoder = ControlDecoder::default();
    for chunk in data.chunks(63) {
        for message in decoder.offer(chunk) {
            let _ = message.parameters();
            let _ = metadata::decode(&message);
        }
    }
    let body = data[..data.len().min(MAX_FRAME - 6)].to_vec();
    for id in [
        metadata::NOW_PLAYING,
        metadata::ROUTE_GUIDANCE,
        metadata::ROUTE_MANEUVER,
        metadata::CALL_STATE,
    ] {
        let _ = metadata::decode(&ControlMessage {
            message_id: id,
            body: body.clone(),
        });
    }
});
