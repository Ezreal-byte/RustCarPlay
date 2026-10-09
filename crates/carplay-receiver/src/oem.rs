// SPDX-License-Identifier: GPL-3.0-only
//! Neutral return-to-car tile. Replace the PNG and configure oem_label for a
//! vehicle integration; UiRequested remains the platform-independent entry event.
use crate::info::{array, dict, number, text};
use carplay_core::config::ReceiverConfig;
use plist::{Dictionary, Value};

pub(crate) fn apply(info: &mut Dictionary, config: &ReceiverConfig) {
    info.insert("oemIconVisible".into(), Value::Boolean(true));
    info.insert("oemIconLabel".into(), text(&config.oem_label));
    info.insert(
        "oemIcons".into(),
        array([dict([
            (
                "imageData",
                Value::Data(include_bytes!("../assets/oem-car.png").to_vec()),
            ),
            ("widthPixels", number(256)),
            ("heightPixels", number(256)),
            ("prerendered", Value::Boolean(true)),
        ])]),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oem_tile_is_advertised_with_configurable_label_and_matching_png_size() {
        let config = ReceiverConfig {
            oem_label: "My vehicle".into(),
            ..Default::default()
        };
        let info = crate::info::build(&config, "02:00:00:00:00:01", "02:00:00:00:00:02");
        let info = info.as_dictionary().unwrap();
        assert_eq!(info["oemIconLabel"].as_string(), Some("My vehicle"));
        assert_eq!(info["oemIconVisible"].as_boolean(), Some(true));
        let icon = info["oemIcons"].as_array().unwrap()[0]
            .as_dictionary()
            .unwrap();
        let png = icon["imageData"].as_data().unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 256);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 256);
        assert_eq!(icon["widthPixels"].as_unsigned_integer(), Some(256));
    }
}
