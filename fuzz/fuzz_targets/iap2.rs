// SPDX-License-Identifier: GPL-3.0-only
// Exercises the DiPlay-derived framing/link implementation; never opens hardware.
#![no_main]
use carplay_protocol::iap2::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(131_072)];
    let mut decoder = PacketDecoder::default();
    for chunk in data.chunks(127) {
        if decoder.push(chunk).is_err() {
            break;
        }
        loop {
            let before = decoder.buffered_bytes();
            match decoder.next_packet() {
                Ok(Some(packet)) => {
                    assert_eq!(
                        LinkPacket::decode(&packet.encode().unwrap()).unwrap(),
                        packet
                    );
                }
                Ok(None) => break,
                Err(_) if decoder.buffered_bytes() < before => continue,
                Err(_) => break,
            }
        }
        assert!(decoder.buffered_bytes() <= MAX_FRAME);
    }
    if let Ok(packet) = LinkPacket::decode(data) {
        assert_eq!(
            LinkPacket::decode(&packet.encode().unwrap()).unwrap(),
            packet
        );
    }
    let _ = Synchronization::decode(data);

    let mut link = LinkEngine::default();
    link.start(true, 0);
    link.take_output();
    // Reach the established state without relying on a random valid SYN checksum.
    let syn = LinkPacket {
        control: SYN | ACK,
        sequence: 1,
        acknowledgement: 99,
        session_id: 0,
        payload: Some(Config::default().synchronization().encode().unwrap()),
    };
    link.feed(&syn.encode().unwrap(), 1);
    link.take_output();
    while link.poll_event().is_some() {}
    for (index, chunk) in data.chunks(97).enumerate() {
        let now = 2 + index as u64;
        link.feed(chunk, now);
        link.advance_time(now);
        link.take_output();
        while link.poll_event().is_some() {}
    }
    if data.len() <= MAX_PAYLOAD {
        let _ = link.send_control(data, 2000);
    }
    link.advance_time(60_000);
    link.take_output();
    link.feed_eof();
    assert_eq!(link.state(), State::Dead);
    assert_eq!(link.next_deadline_ms(), None);
});
