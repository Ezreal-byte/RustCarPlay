// SPDX-License-Identifier: GPL-3.0-only
// Wire scenarios from DiPlay 9e244d9 shared/src/test/java/com/shilapi/xcertplay/hud/
// {BydClusterSongTest,BydHudRouteStateTest,CarPlayCallStateTest}.kt.
use carplay_protocol::{
    Error,
    metadata::*,
    tlv::{ControlMessage, Parameter},
};
fn update(id: u16, params: &[Parameter]) -> Update {
    decode(&ControlMessage::new(id, params).unwrap())
        .unwrap()
        .unwrap()
}

#[test]
fn upstream_now_playing_groups_and_partial_clear_are_distinct() {
    let Update::NowPlaying(value) = update(
        NOW_PLAYING,
        &[
            Parameter::group(
                0,
                &[
                    Parameter::string(1, "Numb").unwrap(),
                    Parameter::string(12, "Linkin Park").unwrap(),
                    Parameter::u32(4, 180_000),
                ],
            )
            .unwrap(),
            Parameter::group(1, &[Parameter::u8(0, 1), Parameter::u32(1, 120_706)]).unwrap(),
        ],
    ) else {
        panic!()
    };
    assert_eq!(value.item.as_ref().unwrap().title.as_deref(), Some("Numb"));
    assert_eq!(
        value.item.as_ref().unwrap().artist.as_deref(),
        Some("Linkin Park")
    );
    assert_eq!(value.item.as_ref().unwrap().duration_ms, Some(180_000));
    assert!(
        value
            .playback
            .as_ref()
            .unwrap()
            .status
            .unwrap()
            .is_playing()
    );
    assert_eq!(value.playback.unwrap().elapsed_ms, Some(120_706));
    let Update::NowPlaying(value) = update(
        NOW_PLAYING,
        &[Parameter::group(0, &[Parameter::string(1, "").unwrap()]).unwrap()],
    ) else {
        panic!()
    };
    let item = value.item.unwrap();
    assert_eq!(item.title, Some(String::new()));
    assert_eq!(item.artist, None);
    assert_eq!(value.playback, None);
    for status in [1, 3, 4] {
        assert!(PlaybackStatus(status).is_playing());
    }
    assert!(!PlaybackStatus(2).is_playing());
}

#[test]
fn upstream_route_vector_preserves_indices_roads_and_64_bit_time() {
    let Update::Maneuver(maneuver) = update(
        ROUTE_MANEUVER,
        &[
            Parameter::u16(1, 2),
            Parameter::u8(3, 2),
            Parameter::string(4, "Am Wehr").unwrap(),
            Parameter::u8(8, 0),
        ],
    ) else {
        panic!()
    };
    assert_eq!(maneuver.index, Some(2));
    assert_eq!(maneuver.after_road.as_deref(), Some("Am Wehr"));
    let Update::Route(route) = update(
        ROUTE_GUIDANCE,
        &[
            Parameter::u8(1, 1),
            Parameter::string(3, "Hauptstraße").unwrap(),
            Parameter::new(5, 0x68d52a40u64.to_be_bytes().to_vec()),
            Parameter::new(6, 7200u64.to_be_bytes().to_vec()),
            Parameter::u32(7, 4000),
            Parameter::u32(10, 300),
            Parameter::new(13, vec![0, 2, 0, 7]),
        ],
    ) else {
        panic!()
    };
    assert_eq!(route.state, Some(RouteState::Active));
    assert_eq!(route.current_road.as_deref(), Some("Hauptstraße"));
    assert_eq!(route.arrival_epoch_seconds, Some(0x68d52a40));
    assert_eq!(route.remaining_seconds, Some(7200));
    assert_eq!(route.remaining_meters, Some(4000));
    assert_eq!(route.distance_to_maneuver_meters, Some(300));
    assert_eq!(route.current_maneuver_indices, Some(vec![2, 7]));
    let Update::Route(empty) = update(ROUTE_GUIDANCE, &[Parameter::empty(13)]) else {
        panic!()
    };
    assert_eq!(empty.current_maneuver_indices, Some(vec![]));
    let Update::Route(end) = update(ROUTE_GUIDANCE, &[Parameter::u8(1, 2)]) else {
        panic!()
    };
    assert_eq!(end.state, Some(RouteState::Arrived));
    assert_eq!(end.current_maneuver_indices, None);
}

#[test]
fn upstream_call_states_and_incremental_fields() {
    for (raw, expected) in [
        (0, CallStatus::Disconnected),
        (1, CallStatus::Sending),
        (2, CallStatus::Ringing),
        (3, CallStatus::Connecting),
        (4, CallStatus::Active),
        (5, CallStatus::Held),
        (6, CallStatus::Disconnecting),
        (9, CallStatus::Unknown(9)),
    ] {
        let Update::Call(call) = update(
            CALL_STATE,
            &[
                Parameter::string(0, "+971500000000").unwrap(),
                Parameter::string(1, "Mum").unwrap(),
                Parameter::u8(2, raw),
                Parameter::u8(3, 1),
                Parameter::string(4, "a").unwrap(),
            ],
        ) else {
            panic!()
        };
        assert_eq!(call.status, Some(expected));
        assert_eq!(call.call_uuid.as_deref(), Some("a"));
        assert_eq!(call.display_name.as_deref(), Some("Mum"));
        assert_eq!(call.unknown, vec![Parameter::u8(3, 1)]);
    }
    let Update::Call(partial) = update(
        CALL_STATE,
        &[Parameter::u8(2, 4), Parameter::string(4, "b").unwrap()],
    ) else {
        panic!()
    };
    assert_eq!(partial.display_name, None);
    assert_eq!(partial.remote_identifier, None);
}

#[test]
fn repeated_unknown_fields_are_retained_at_their_original_group_level() {
    let unknown = vec![
        Parameter::new(200, vec![9, 8]),
        Parameter::new(200, vec![7]),
        Parameter::empty(201),
    ];
    let mut params = unknown.clone();
    params.push(Parameter::group(0, &unknown).unwrap());
    let Update::NowPlaying(parsed) = update(NOW_PLAYING, &params) else {
        panic!()
    };
    assert_eq!(parsed.unknown, unknown);
    assert_eq!(parsed.item.unwrap().unknown, unknown);
    assert_eq!(decode(&ControlMessage::empty(0x9999)).unwrap(), None);
}

#[test]
fn malformed_truncated_duplicate_and_wrong_width_fields_fail_atomically() {
    for body in [
        vec![0, 8, 0, 1, 1],
        vec![0, 3, 0, 1],
        vec![0],
        vec![0, 5, 0, 1, 1, 0],
    ] {
        assert!(
            decode(&ControlMessage {
                message_id: ROUTE_GUIDANCE,
                body
            })
            .is_err()
        );
    }
    for (id, params) in [
        (ROUTE_GUIDANCE, vec![Parameter::new(13, vec![0])]),
        (ROUTE_GUIDANCE, vec![Parameter::u32(5, 1)]),
        (ROUTE_MANEUVER, vec![Parameter::u8(1, 1)]),
        (CALL_STATE, vec![Parameter::u16(2, 1)]),
        (CALL_STATE, vec![Parameter::u8(2, 1), Parameter::u8(2, 2)]),
        (NOW_PLAYING, vec![Parameter::new(0, vec![0, 10, 0, 1])]),
        (NOW_PLAYING, vec![Parameter::empty(0), Parameter::empty(0)]),
    ] {
        assert!(decode(&ControlMessage::new(id, &params).unwrap()).is_err());
    }
}

#[test]
fn strings_are_strict_utf8_and_text_parameter_list_and_body_limits_are_enforced() {
    for value in [vec![0xff, 0], b"unterminated".to_vec(), b"a\0b\0".to_vec()] {
        assert!(
            decode(&ControlMessage::new(CALL_STATE, &[Parameter::new(1, value)]).unwrap()).is_err()
        );
    }
    let text = ControlMessage::new(CALL_STATE, &[Parameter::string(1, "🎵").unwrap()]).unwrap();
    assert_eq!(
        decode_with_limits(
            &text,
            Limits {
                max_text_bytes: 3,
                ..Default::default()
            }
        ),
        Err(Error::Limit)
    );
    let nested = ControlMessage::new(
        NOW_PLAYING,
        &[Parameter::group(0, &[Parameter::string(1, "x").unwrap()]).unwrap()],
    )
    .unwrap();
    assert_eq!(
        decode_with_limits(
            &nested,
            Limits {
                max_parameters: 1,
                ..Default::default()
            }
        ),
        Err(Error::Limit)
    );
    let list =
        ControlMessage::new(ROUTE_GUIDANCE, &[Parameter::new(13, vec![0, 1, 0, 2])]).unwrap();
    assert_eq!(
        decode_with_limits(
            &list,
            Limits {
                max_maneuver_indices: 1,
                ..Default::default()
            }
        ),
        Err(Error::Limit)
    );
    assert_eq!(
        decode_with_limits(
            &text,
            Limits {
                max_body_bytes: 3,
                ..Default::default()
            }
        ),
        Err(Error::Limit)
    );
    assert_eq!(
        decode(&ControlMessage {
            message_id: CALL_STATE,
            body: vec![0; 65_530]
        }),
        Err(Error::Limit)
    );
}
