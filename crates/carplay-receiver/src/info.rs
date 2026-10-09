// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay airplay/AirPlayInfoPlist.kt and AirPlayHid.kt.
use carplay_core::{config::ReceiverConfig, input};
use plist::{Dictionary, Value};

pub const MAIN_UUID: &str = "b7e6c5a0-1111-4000-8000-000000000001";
pub const ALT_UUID: &str = "b7e6c5a0-2222-4000-8000-000000000002";
pub fn dict(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}
pub fn number(n: u64) -> Value {
    Value::Integer(n.into())
}
pub fn text(s: impl Into<String>) -> Value {
    Value::String(s.into())
}
pub fn array(items: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(items.into_iter().collect())
}
pub fn encode(value: &Value) -> anyhow::Result<Vec<u8>> {
    let mut b = Vec::new();
    value.to_writer_binary(&mut b)?;
    Ok(b)
}
pub fn get_number(d: &Dictionary, key: &str) -> Option<u64> {
    d.get(key)?.as_unsigned_integer()
}

pub fn features(c: &ReceiverConfig) -> u64 {
    if c.audio_output {
        0x615653aee2
    } else {
        0x615653aee2 & !0x10004540a00
    }
}
pub fn build(c: &ReceiverConfig, device_id: &str, bt_address: &str) -> Value {
    let resource = |id| {
        dict([
            ("resourceID", number(id)),
            ("transferType", number(1)),
            ("transferPriority", number(100)),
            ("takeConstraint", number(100)),
            ("borrowConstraint", number(100)),
            ("unborrowConstraint", number(100)),
        ])
    };
    let mut d = Dictionary::new();
    for (key, value) in [
        ("sourceVersion", text(carplay_core::SOURCE_VERSION)),
        ("features", number(features(c))),
        ("statusFlags", number(4)),
        ("model", text("RustCarPlay")),
        ("manufacturer", text("RustCarPlay")),
        ("deviceID", text(device_id)),
        ("bluetoothIDs", array([text(bt_address)])),
        ("name", text(&c.name)),
        ("rightHandDrive", Value::Boolean(c.right_hand_drive)),
        ("keepAliveLowPower", Value::Boolean(false)),
        ("keepAliveSendStatsAsBody", Value::Boolean(false)),
        (
            "modes",
            dict([
                ("resources", array([resource(1), resource(2)])),
                (
                    "appStates",
                    array([
                        dict([("appStateID", number(2)), ("state", Value::Boolean(false))]),
                        dict([
                            ("appStateID", number(1)),
                            ("speechMode", Value::Integer((-1i64).into())),
                        ]),
                        dict([("appStateID", number(3)), ("state", Value::Boolean(false))]),
                    ]),
                ),
            ]),
        ),
        (
            "extendedFeatures",
            array([text("vocoderInfo"), text("enhancedRequestCarUI")]),
        ),
    ] {
        d.insert(key.into(), value);
    }
    let mut displays = vec![display(c.width, c.height, c.fps, 110, MAIN_UUID)];
    if c.second_screen {
        let mut second = display(800, 480, 30, 111, ALT_UUID);
        let d = second.as_dictionary_mut().expect("display is a dictionary");
        d.insert("initialURL".into(), text("maps:/car/instrumentcluster/map"));
        d.insert("primaryInputDevice".into(), number(0));
        d.insert("features".into(), number(0));
        displays.push(second);
    }
    d.insert("displays".into(), array(displays));
    let hid = |uid: u32, name: &str, desc: Vec<u8>| {
        dict([
            ("hidProductID", number(1)),
            ("hidVendorID", number(2)),
            ("hidCountryCode", number(0)),
            ("uuid", text(format!("{uid:x}"))),
            ("name", text(name)),
            ("displayUUID", text(MAIN_UUID)),
            ("hidDescriptor", Value::Data(desc)),
        ])
    };
    d.insert(
        "hidDevices".into(),
        array([
            hid(
                input::TOUCH_UID,
                "RustCarPlay Touch",
                input::touch_descriptor(c.width, c.height),
            ),
            hid(
                input::KNOB_UID,
                "RustCarPlay Knob",
                input::KNOB_DESCRIPTOR.to_vec(),
            ),
        ]),
    );
    if c.audio_output {
        let mut formats = Vec::new();
        for (stream, kind, out, mic) in [
            (100, "compatibility", 0xc3fc, 0x4154),
            (101, "compatibility", 0xc3fc, 0),
            (100, "default", 0x7000c3fc, 0x70004154),
            (100, "alert", 0x7000c3fc, 0),
            (100, "media", 0xc3fc, 0),
            (100, "telephony", 0x70004154, 0x70004154),
            (100, "speechRecognition", 0x70004154, 0x70004154),
            (101, "default", 0x7000c3fc, 0),
            (102, "media", 0x800000, 0),
        ] {
            let mut f = Dictionary::new();
            f.insert("type".into(), number(stream));
            f.insert("audioType".into(), text(kind));
            f.insert("audioOutputFormats".into(), number(out));
            if c.microphone && mic != 0 {
                f.insert("audioInputFormats".into(), number(mic));
            }
            formats.push(Value::Dictionary(f));
        }
        d.insert("audioFormats".into(), array(formats));
        d.insert(
            "audioLatencies".into(),
            array(
                [100, 101]
                    .into_iter()
                    .map(|t| {
                        dict([
                            ("type", number(t)),
                            ("inputLatencyMicros", number(0)),
                            ("outputLatencyMicros", number(0)),
                        ])
                    })
                    .chain(
                        [
                            (100, "default"),
                            (100, "media"),
                            (100, "telephony"),
                            (100, "speechRecognition"),
                            (100, "alert"),
                            (101, "default"),
                            (102, "default"),
                        ]
                        .into_iter()
                        .map(|(t, a)| {
                            dict([
                                ("type", number(t)),
                                ("audioType", text(a)),
                                ("inputLatencyMicros", number(0)),
                                ("outputLatencyMicros", number(0)),
                            ])
                        }),
                    ),
            ),
        );
    }
    if c.hevc {
        d.insert("hevcInfo".into(), Value::Dictionary(Dictionary::new()));
    }
    Value::Dictionary(d)
}

/// Keep input codec declarations consistent with the supplied capture backend.
/// supports() is a capability probe and must never activate an input device.
pub(crate) fn filter_capture(
    info: &mut Value,
    factory: Option<&dyn carplay_core::media::CaptureFactory>,
) {
    let Some(formats) = info
        .as_dictionary_mut()
        .and_then(|d| d.get_mut("audioFormats"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for entry in formats {
        let Some(entry) = entry.as_dictionary_mut() else {
            continue;
        };
        let Some(mask) = entry
            .get("audioInputFormats")
            .and_then(Value::as_unsigned_integer)
        else {
            continue;
        };
        let supported = (0..64)
            .map(|shift| 1u64 << shift)
            .filter(|bit| mask & bit != 0)
            .filter(|bit| {
                crate::microphone::capture_config(*bit, 0)
                    .is_some_and(|(config, _)| factory.is_some_and(|f| f.supports(&config)))
            })
            .fold(0, |mask, bit| mask | bit);
        if supported == 0 {
            entry.remove("audioInputFormats");
        } else {
            entry.insert("audioInputFormats".into(), number(supported));
        }
    }
}

fn display(w: u16, h: u16, fps: u16, kind: u64, id: &str) -> Value {
    let safe = dict([
        ("widthPixels", number(w.into())),
        ("heightPixels", number(h.into())),
        ("originXPixels", number(0)),
        ("originYPixels", number(0)),
        ("drawUIOutsideSafeArea", Value::Boolean(true)),
    ]);
    dict([
        ("uuid", text(id)),
        ("type", number(kind)),
        ("maxFPS", number(fps.into())),
        ("widthPixels", number(w.into())),
        ("heightPixels", number(h.into())),
        ("widthPhysical", number(200)),
        ("heightPhysical", number((200u64 * h as u64) / (w as u64))),
        ("features", number(10)),
        ("primaryInputDevice", number(1)),
        ("initialViewArea", number(0)),
        (
            "viewAreas",
            array([dict([
                ("widthPixels", number(w.into())),
                ("heightPixels", number(h.into())),
                ("originXPixels", number(0)),
                ("originYPixels", number(0)),
                ("safeArea", safe),
            ])]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declaration_has_consistent_real_capabilities() {
        let c = ReceiverConfig {
            audio_output: false,
            ..Default::default()
        };
        let value = build(&c, "02:00:00:00:00:01", "02:00:00:00:00:02");
        let d = value.as_dictionary().unwrap();
        assert!(!d.contains_key("audioFormats"));
        assert!(!d.contains_key("videoPlaybackInfo"));
        assert_eq!(
            Value::from_reader(std::io::Cursor::new(encode(&value).unwrap())).unwrap(),
            value
        );
    }
}
