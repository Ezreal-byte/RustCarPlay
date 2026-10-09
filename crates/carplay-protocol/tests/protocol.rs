// SPDX-License-Identifier: GPL-3.0-only
// Reference cases derived from DiPlay 9e244d9:
// shared/src/test/java/com/shilapi/xcertplay/transport/{Iap2LinkEngineFileTransferTest,Ntb16CodecTest}.kt
// shared/src/test/java/com/shilapi/xcertplay/iap2/Iap2ProtocolTest.kt
// Additional boundary tests exercise Rust parser/queue failure behavior.
use carplay_protocol::{Error, iap2::*, ncm, tlv::*, usbmux};

fn peer_sync() -> Synchronization {
    Synchronization {
        max_outgoing: 8,
        max_length: 4096,
        retransmission_timeout_ms: 2000,
        acknowledgement_timeout_ms: 500,
        max_retransmissions: 4,
        max_acknowledgements: 3,
        sessions: vec![
            SessionDescriptor {
                id: 10,
                kind: 0,
                version: 2,
            },
            SessionDescriptor {
                id: 12,
                kind: 1,
                version: 2,
            },
        ],
    }
}
fn packet(control: u8, sequence: u8, ack: u8, session: u8, payload: Option<Vec<u8>>) -> Vec<u8> {
    LinkPacket {
        control,
        sequence,
        acknowledgement: ack,
        session_id: session,
        payload,
    }
    .encode()
    .unwrap()
}
fn ready(config: Config, sync: Synchronization) -> LinkEngine {
    let mut engine = LinkEngine::new(config).unwrap();
    engine.start(true, 0);
    engine.take_output();
    engine.feed(
        &packet(SYN | ACK, 1, 99, 0, Some(sync.encode().unwrap())),
        1,
    );
    assert_eq!(engine.state(), State::Normal);
    engine.take_output();
    while engine.poll_event().is_some() {}
    engine
}
fn wire_packets(bytes: &[u8]) -> Vec<LinkPacket> {
    let mut decoder = PacketDecoder::default();
    decoder.push(bytes).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = decoder.next_packet().unwrap() {
        packets.push(packet);
    }
    packets
}

#[test]
fn fixed_link_vector_checks_both_checksums_and_endianness() {
    // File-transfer start body 81 01 from upstream, sequence 100, cumulative ACK 2.
    let expected = [
        0xff, 0x5a, 0x00, 0x0c, 0x40, 0x64, 0x02, 0x0c, 0xe9, 0x81, 0x01, 0x7e,
    ];
    assert_eq!(packet(ACK, 100, 2, 12, Some(vec![0x81, 1])), expected);
    assert_eq!(
        LinkPacket::decode(&expected).unwrap().payload,
        Some(vec![0x81, 1])
    );
    for index in [8, 9, 11] {
        let mut corrupt = expected;
        corrupt[index] ^= 1;
        assert_eq!(LinkPacket::decode(&corrupt), Err(Error::Checksum));
    }
}
#[test]
fn incremental_link_frames_survive_every_split() {
    let frame = packet(ACK, 100, 2, 12, Some(vec![0x81, 1]));
    for split in 0..=frame.len() {
        let mut d = PacketDecoder::default();
        d.push(&frame[..split]).unwrap();
        let first = d.next_packet().unwrap();
        d.push(&frame[split..]).unwrap();
        let all = first.or_else(|| d.next_packet().unwrap());
        assert_eq!(all.unwrap().payload, Some(vec![0x81, 1]));
        assert_eq!(d.buffered_bytes(), 0);
    }
}
#[test]
fn parser_discards_corrupt_packet_and_recovers_next() {
    let good = packet(ACK, 1, 1, 10, Some(vec![1, 2]));
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    let mut d = PacketDecoder::default();
    d.push(&[vec![0x42, 0xff, 0x33], bad, good.clone()].concat())
        .unwrap();
    assert_eq!(d.next_packet(), Err(Error::Checksum));
    assert_eq!(
        d.next_packet().unwrap().unwrap(),
        LinkPacket::decode(&good).unwrap()
    );
}
#[test]
fn link_decoder_bound_and_empty_payload_distinction() {
    let no_payload = packet(ACK, 1, 1, 0, None);
    let empty = packet(ACK, 1, 1, 0, Some(Vec::new()));
    assert_eq!(no_payload.len(), 9);
    assert_eq!(empty.len(), 10);
    let mut d = PacketDecoder::default();
    assert_eq!(d.push(&vec![0; 65_536]), Err(Error::Limit));
    assert_eq!(d.buffered_bytes(), 0);
    assert!(
        LinkPacket {
            control: ACK,
            sequence: 1,
            acknowledgement: 0,
            session_id: 10,
            payload: Some(vec![0; 65_526])
        }
        .encode()
        .is_err()
    );
}
#[test]
fn synchronization_reference_fields_and_truncated_descriptor() {
    let s = peer_sync();
    let expected = [
        1, 8, 0x10, 0, 0x07, 0xd0, 0x01, 0xf4, 4, 3, 10, 0, 2, 12, 1, 2,
    ];
    assert_eq!(s.encode().unwrap(), expected);
    assert_eq!(Synchronization::decode(&expected).unwrap(), s);
    assert!(Synchronization::decode(&expected[..15]).is_err());
}
#[test]
fn wireless_marker_requires_all_six_bytes_and_resends() {
    let mut e = LinkEngine::default();
    e.start(false, 0);
    assert_eq!(e.take_output(), MARKER);
    e.advance_time(999);
    assert!(e.take_output().is_empty());
    e.advance_time(1000);
    assert_eq!(e.take_output(), MARKER);
    e.feed(&MARKER[..5], 1001);
    assert_eq!(e.state(), State::Detecting);
    e.feed(&MARKER[5..], 1002);
    assert_eq!(e.state(), State::Negotiating);
    let syn = e.take_output();
    assert_eq!(wire_packets(&syn)[0].control, SYN);
    e.advance_time(1502);
    assert_eq!(e.take_output(), syn);
}
#[test]
fn invalid_marker_and_reset_end_without_pending_output() {
    let mut e = LinkEngine::default();
    e.start(false, 0);
    e.feed(&[1], 1);
    assert_eq!(e.state(), State::Dead);
    assert!(e.take_output().is_empty());
    assert!(matches!(e.poll_event(), Some(Event::Dead(Some(_)))));
    let mut e = ready(Config::default(), peer_sync());
    e.feed(&packet(RESET, 2, 99, 0, None), 2);
    assert_eq!(e.state(), State::Dead);
    assert_eq!(e.next_deadline_ms(), None);
}
#[test]
fn upstream_file_transfer_routes_and_replies() {
    let mut e = ready(Config::default(), peer_sync());
    let body = vec![0x81, 4, 0, 0, 0, 0, 0, 0, 0, 5, 0, 2];
    e.feed(&packet(ACK, 2, 99, 12, Some(body.clone())), 2);
    assert_eq!(
        e.poll_event(),
        Some(Event::Session {
            session_id: 12,
            bytes: body
        })
    );
    e.take_output();
    e.send_session(12, &[0x81, 1], 3).unwrap();
    let out = wire_packets(&e.take_output());
    assert_eq!(out[0].session_id, 12);
    assert_eq!(out[0].payload, Some(vec![0x81, 1]));
}
#[test]
fn control_receive_reorders_and_deduplicates() {
    let mut e = ready(Config::default(), peer_sync());
    e.feed(&packet(ACK, 3, 99, 10, Some(vec![3])), 2);
    assert_eq!(e.poll_event(), None);
    e.feed(&packet(ACK, 2, 99, 10, Some(vec![2])), 3);
    assert_eq!(e.poll_event(), Some(Event::Control(vec![2])));
    assert_eq!(e.poll_event(), Some(Event::Control(vec![3])));
    e.feed(&packet(ACK, 2, 99, 10, Some(vec![2])), 4);
    assert_eq!(e.poll_event(), None);
}
#[test]
fn sequence_wrap_delivers_every_control_packet_once() {
    let mut e = ready(Config::default(), peer_sync());
    for value in 2..=260u16 {
        e.feed(
            &packet(ACK, value as u8, 99, 10, Some(vec![value as u8])),
            value as u64,
        );
        assert_eq!(e.poll_event(), Some(Event::Control(vec![value as u8])));
        e.take_output();
    }
}
#[test]
fn send_window_does_not_exceed_peer_limit_and_cumulative_ack_drains() {
    let mut s = peer_sync();
    s.max_outgoing = 2;
    let mut e = ready(Config::default(), s);
    for b in 1..=4 {
        e.send_control(&[b], 2).unwrap();
    }
    assert_eq!(wire_packets(&e.take_output()).len(), 2);
    assert!(!e.writable());
    e.feed(&packet(ACK, 1, 101, 0, None), 3);
    let p = wire_packets(&e.take_output());
    assert_eq!(
        p.iter()
            .filter(|p| p.payload.is_some())
            .map(|p| p.sequence)
            .collect::<Vec<_>>(),
        vec![102, 103]
    );
}
#[test]
fn unknown_ack_cannot_discard_outstanding_data() {
    let mut s = peer_sync();
    s.max_outgoing = 1;
    let mut e = ready(Config::default(), s);
    e.send_control(&[1], 2).unwrap();
    e.send_control(&[2], 2).unwrap();
    let original = e.take_output();
    e.feed(&packet(ACK, 1, 110, 0, None), 3);
    assert!(e.take_output().is_empty());
    e.advance_time(2002);
    assert_eq!(e.take_output(), original);
}
#[test]
fn timeout_retries_then_reports_dead() {
    let mut e = ready(Config::default(), peer_sync());
    e.send_control(&[1, 2, 3], 2).unwrap();
    let original = e.take_output();
    for now in [2002, 4002, 6002] {
        e.advance_time(now);
        assert_eq!(e.take_output(), original);
        assert_eq!(e.state(), State::Normal);
    }
    e.advance_time(8002);
    assert_eq!(e.state(), State::Dead);
    assert_eq!(e.next_deadline_ms(), None);
}
#[test]
fn extended_ack_retransmits_only_requested_sequence() {
    let mut e = ready(Config::default(), peer_sync());
    e.send_control(&[1], 2).unwrap();
    e.send_control(&[2], 2).unwrap();
    e.take_output();
    e.feed(&packet(EAK, 1, 99, 0, Some(vec![101])), 3);
    let p = wire_packets(&e.take_output());
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].sequence, 101);
    assert_eq!(p[0].payload, Some(vec![2]));
}
#[test]
fn delayed_ack_has_peer_acknowledgement_number() {
    let mut e = ready(Config::default(), peer_sync());
    e.feed(&packet(ACK, 2, 99, 10, Some(vec![2])), 2);
    e.take_output();
    e.advance_time(501);
    assert!(e.take_output().is_empty());
    e.advance_time(502);
    let p = wire_packets(&e.take_output());
    assert_eq!(p[0].acknowledgement, 2);
    assert_eq!(p[0].payload, None);
}
#[test]
fn invalid_peer_limits_fail_and_peer_payload_bound_is_enforced() {
    for (size, window, timeout) in [(10, 8, 2000), (4096, 0, 2000), (4096, 8, 0)] {
        let mut s = peer_sync();
        s.max_length = size;
        s.max_outgoing = window;
        s.retransmission_timeout_ms = timeout;
        let mut e = LinkEngine::default();
        e.start(true, 0);
        e.take_output();
        e.feed(&packet(SYN | ACK, 1, 99, 0, Some(s.encode().unwrap())), 1);
        assert_eq!(e.state(), State::Dead);
    }
    let mut e = ready(Config::default(), peer_sync());
    assert_eq!(e.send_control(&vec![0; 4087], 2), Err(Error::Limit));
    assert!(e.send_session(77, &[1], 2).is_err());
}
#[test]
fn queued_payload_is_rechecked_after_peer_negotiation() {
    let mut e = LinkEngine::default();
    e.send_control(&vec![0; 4090], 0).unwrap();
    e.start(true, 0);
    e.take_output();
    e.feed(
        &packet(SYN | ACK, 1, 99, 0, Some(peer_sync().encode().unwrap())),
        1,
    );
    assert_eq!(e.state(), State::Dead);
}
#[test]
fn output_event_queue_and_out_of_order_limits_are_terminal() {
    let mut e = LinkEngine::new(Config {
        maximum_pending_output_bytes: 1,
        ..Config::default()
    })
    .unwrap();
    e.start(true, 0);
    assert_eq!(e.state(), State::Dead);
    let mut e = LinkEngine::new(Config {
        maximum_queued_packets: 1,
        ..Config::default()
    })
    .unwrap();
    e.send_control(&[1], 0).unwrap();
    assert_eq!(e.send_control(&[2], 0), Err(Error::Limit));
    let mut e = ready(
        Config {
            maximum_pending_events: 1,
            ..Config::default()
        },
        peer_sync(),
    );
    e.feed(&packet(ACK, 2, 99, 10, Some(vec![1])), 2);
    e.feed(&packet(ACK, 3, 99, 10, Some(vec![2])), 3);
    assert_eq!(e.state(), State::Dead);
    let mut e = ready(
        Config {
            maximum_out_of_order_packets: 1,
            ..Config::default()
        },
        peer_sync(),
    );
    e.feed(&packet(ACK, 3, 99, 10, Some(vec![3])), 2);
    e.feed(&packet(ACK, 4, 99, 10, Some(vec![4])), 3);
    assert_eq!(e.state(), State::Dead);
}
#[test]
fn two_accessories_can_exchange_negotiation_and_data_over_byte_fragments() {
    let mut a = LinkEngine::default();
    let mut b = LinkEngine::default();
    a.start(true, 0);
    b.start(false, 0);
    for now in 1..=4 {
        for byte in a.take_output() {
            b.feed(&[byte], now);
        }
        for byte in b.take_output() {
            a.feed(&[byte], now);
        }
    }
    assert_eq!(a.state(), State::Normal);
    assert_eq!(b.state(), State::Normal);
    while b.poll_event().is_some() {}
    a.send_control(&[1, 2, 3], 5).unwrap();
    for byte in a.take_output() {
        b.feed(&[byte], 5);
    }
    assert_eq!(b.poll_event(), Some(Event::Control(vec![1, 2, 3])));
}

#[test]
fn upstream_csm_fragmented_and_concatenated_frames() {
    let first = ControlMessage {
        message_id: 0x4300,
        body: vec![0, 4, 0, 0],
    };
    let second = ControlMessage {
        message_id: 0x4301,
        body: vec![0, 5, 0, 0, 1],
    };
    let bytes = [first.encode().unwrap(), second.encode().unwrap()].concat();
    let mut d = ControlDecoder::default();
    assert!(d.offer(&bytes[..3]).is_empty());
    assert_eq!(d.offer(&bytes[3..]), vec![first, second]);
}
#[test]
fn csm_garbage_invalid_length_and_maximum_frame() {
    let frame = ControlMessage {
        message_id: 0xaa01,
        body: vec![0; 65_529],
    };
    let encoded = frame.encode().unwrap();
    let mut d = ControlDecoder::default();
    assert!(d.offer(&[9, 0x40, 0x40, 0, 1, 4, 5]).is_empty());
    assert_eq!(d.offer(&encoded), vec![frame]);
    assert!(d.buffered_bytes() <= 1);
    assert!(
        ControlMessage {
            message_id: 1,
            body: vec![0; 65_530]
        }
        .encode()
        .is_err()
    );
}
#[test]
fn upstream_ordered_parameters_keep_repeated_unknown_ids() {
    let p = vec![
        Parameter::u8(0, 1),
        Parameter::u8(0, 2),
        Parameter::new(0x7fff, vec![3, 4]),
    ];
    assert_eq!(
        parse_parameters(&encode_parameters(&p).unwrap()).unwrap(),
        p
    );
    for malformed in [&[0, 1, 2][..], &[0, 3, 0, 0], &[0, 6, 0, 0, 1]] {
        assert!(parse_parameters(malformed).is_err());
    }
}
#[test]
fn strings_are_utf8_nul_terminated_and_validate_wire_input() {
    let p = Parameter::string(1, "车机").unwrap();
    assert_eq!(p.as_str().unwrap(), "车机");
    assert_eq!(p.value.last(), Some(&0));
    assert!(Parameter::string(1, "a\0b").is_err());
    assert!(Parameter::new(1, vec![0xff, 0]).as_str().is_err());
    assert!(Parameter::new(1, vec![b'a']).as_str().is_err());
}
#[test]
fn authentication_and_wifi_have_fixed_csm_vectors() {
    assert_eq!(
        accessory_certificate(&[0x11, 0x22])
            .unwrap()
            .encode()
            .unwrap(),
        [0x40, 0x40, 0, 12, 0xaa, 1, 0, 6, 0, 0, 0x11, 0x22]
    );
    assert_eq!(
        authentication_response(&[0x33]).unwrap().encode().unwrap(),
        [0x40, 0x40, 0, 11, 0xaa, 3, 0, 5, 0, 0, 0x33]
    );
    let wifi = wifi_configuration("LIVI", "secret", 36, 3, None).unwrap();
    let p = wifi.parameters().unwrap();
    assert_eq!(p.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert_eq!(p[0].as_str().unwrap(), "LIVI");
    assert_eq!(p[2].as_u8().unwrap(), 3);
    assert_eq!(p[3].as_u8().unwrap(), 36);
    assert_eq!(
        wifi_configuration("LIVI", "secret", 36, 3, Some([1, 2, 3, 4, 5, 6]))
            .unwrap()
            .parameters()
            .unwrap()[0]
            .value,
        [1, 2, 3, 4, 5, 6]
    );
}
#[test]
fn challenge_reader_rejects_wrong_id_duplicate_and_missing() {
    let challenge = ControlMessage::new(
        REQUEST_CHALLENGE_RESPONSE,
        &[Parameter::new(0, vec![1; 32])],
    )
    .unwrap();
    assert_eq!(authentication_challenge(&challenge).unwrap(), vec![1; 32]);
    assert!(authentication_challenge(&ControlMessage::empty(CERTIFICATE)).is_err());
    assert!(
        authentication_challenge(
            &ControlMessage::new(
                REQUEST_CHALLENGE_RESPONSE,
                &[Parameter::u8(0, 1), Parameter::u8(0, 2)]
            )
            .unwrap()
        )
        .is_err()
    );
}
#[test]
fn upstream_start_session_nested_fields_and_u32_port() {
    let message = StartSession {
        airplay_port: 7000,
        public_key: "pub".into(),
        source_version: "1.0".into(),
        wired_ipv6_addresses: vec!["fe80::2".into()],
        wired_reserved: Some(3),
        wireless: Some(WirelessSession {
            ssid: "LIVI".into(),
            passphrase: "secret".into(),
            channel: 36,
            ip_addresses: vec!["192.168.1.1".into(), "192.168.1.2".into()],
            security_type: 3,
        }),
        device_identifier: Some("dev-1".into()),
        sdk_version: Some("27.0".into()),
        cluster_asset: Some(("cluster".into(), 4)),
        mutual_auth: Some(true),
    }
    .build()
    .unwrap();
    let params = message.parameters().unwrap();
    assert_eq!(
        params.iter().find(|p| p.id == 2).unwrap().as_u32().unwrap(),
        7000
    );
    let wireless = params[1].parameters().unwrap();
    assert_eq!(wireless.iter().filter(|p| p.id == 3).count(), 2);
    assert_eq!(params.last().unwrap().as_u8().unwrap(), 1);
    let mut d = ControlDecoder::default();
    let mut frames = Vec::new();
    for chunk in message.link_chunks(7).unwrap() {
        frames.extend(d.offer(&chunk));
    }
    assert_eq!(frames, vec![message]);
}
#[test]
fn identification_wireless_component_is_binary_mac_and_declares_only_selected_messages() {
    let info = Identification {
        name: "RustCarplay".into(),
        model: "Desktop".into(),
        manufacturer: "Open Source".into(),
        serial: "test".into(),
        firmware_version: "0.1".into(),
        hardware_version: "1".into(),
        language: "en".into(),
        external_accessory_protocol: "org.rustcarplay".into(),
        transport: IdentificationTransport::Wireless {
            bluetooth_mac: [1, 2, 3, 4, 5, 6],
            ssid: "LAN".into(),
        },
        sent_messages: vec![0xaa01, 0xaa03, 0x5703, 0x4301],
        received_messages: vec![0xaa00, 0xaa02, 0xaa04, 0xaa05],
        extra_components: vec![],
    };
    let p = info.build().unwrap().parameters().unwrap();
    let bt = p.iter().find(|p| p.id == 17).unwrap().parameters().unwrap();
    assert_eq!(
        bt.iter().find(|p| p.id == 3).unwrap().value,
        [1, 2, 3, 4, 5, 6]
    );
    assert!(!p.iter().any(|p| p.id == 16 || p.id == 22 || p.id == 30));
    assert_eq!(
        p.iter().find(|p| p.id == 6).unwrap().value,
        [0xaa, 1, 0xaa, 3, 0x57, 3, 0x43, 1]
    );
}
#[test]
fn upstream_ntb16_frame_and_largest_datagram() {
    let frame = [0x33, 0x33, 0, 0, 0, 1, 0x86, 0xdd];
    let block = ncm::encode(&frame, 7).unwrap();
    assert_eq!(
        &block[..28],
        &[
            0x4e, 0x43, 0x4d, 0x48, 12, 0, 7, 0, 36, 0, 12, 0, 0x4e, 0x43, 0x4d, 0x30, 16, 0, 0, 0,
            28, 0, 8, 0, 0, 0, 0, 0
        ]
    );
    assert_eq!(ncm::decode(&block).unwrap(), vec![frame.as_slice()]);
    let max = ncm::encode(&vec![0; 65_507], 0x1234).unwrap();
    assert_eq!(max.len(), 65_535);
    assert_eq!(&max[8..10], &[0xff, 0xff]);
    assert!(ncm::encode(&vec![0; 65_508], 1).is_err());
}
#[test]
fn ntb16_padding_and_malformed_chains() {
    let block = ncm::encode(&vec![0xab; 484], 0).unwrap();
    assert_eq!(block.len(), 513);
    assert_eq!(&block[8..10], &[0, 2]);
    assert_eq!(ncm::decode(&block).unwrap()[0].len(), 484);
    let mut corrupt = block.clone();
    corrupt[18] = 12;
    assert!(ncm::decode(&corrupt).is_err());
    let mut corrupt = block.clone();
    corrupt[22..24].copy_from_slice(&65535u16.to_le_bytes());
    assert!(ncm::decode(&corrupt).is_err());
    let mut corrupt = block.clone();
    corrupt[15] = b'1';
    assert!(ncm::decode(&corrupt).is_err());
}
fn mux_tcp() -> usbmux::Frame {
    let mut payload = vec![0; 20];
    payload[0..2].copy_from_slice(&62078u16.to_be_bytes());
    payload[2..4].copy_from_slice(&1024u16.to_be_bytes());
    payload[12] = 0x50;
    usbmux::Frame {
        protocol: usbmux::TCP,
        word8: usbmux::REPLY_MAGIC,
        sequence: 1,
        acknowledgement: 2,
        payload,
    }
}
#[test]
fn usbmux_fixed_header_and_fragmentation() {
    let f = mux_tcp();
    let bytes = f.encode().unwrap();
    assert_eq!(
        &bytes[..16],
        &[0, 0, 0, 6, 0, 0, 0, 36, 0xfa, 0xce, 0xfa, 0xce, 0, 1, 0, 2]
    );
    for split in 0..bytes.len() {
        let mut d = usbmux::Decoder::default();
        d.push(&bytes[..split]).unwrap();
        assert!(d.next_frame().unwrap().is_none());
        d.push(&bytes[split..]).unwrap();
        assert_eq!(d.next_frame().unwrap(), Some(f.clone()));
    }
}
#[test]
fn usbmux_known_optional_padding_is_recovered_but_not_arbitrary_scan() {
    let version = usbmux::Frame {
        protocol: 0,
        word8: 2,
        sequence: 0,
        acknowledgement: 0,
        payload: vec![0; 4],
    };
    let next = mux_tcp();
    let mut d = usbmux::Decoder::default();
    d.push(&version.encode().unwrap()).unwrap();
    assert_eq!(d.next_frame().unwrap(), Some(version));
    let bytes = [vec![0; 4], next.encode().unwrap()].concat();
    for byte in &bytes[..bytes.len() - 1] {
        d.push(&[*byte]).unwrap();
        assert!(d.next_frame().unwrap().is_none());
    }
    d.push(&bytes[bytes.len() - 1..]).unwrap();
    assert_eq!(d.next_frame().unwrap(), Some(next.clone()));
    let mut d = usbmux::Decoder::default();
    d.push(&bytes).unwrap();
    assert!(d.next_frame().is_err());
}
#[test]
fn usbmux_no_padding_does_not_discard_next_valid_header() {
    let f = mux_tcp();
    let bytes = [f.encode().unwrap(), f.encode().unwrap()].concat();
    let mut d = usbmux::Decoder::default();
    d.push(&bytes).unwrap();
    assert_eq!(d.next_frame().unwrap(), Some(f.clone()));
    assert_eq!(d.next_frame().unwrap(), Some(f));
    assert_eq!(d.push(&vec![0; usbmux::MAX_FRAME + 5]), Err(Error::Limit));
}
