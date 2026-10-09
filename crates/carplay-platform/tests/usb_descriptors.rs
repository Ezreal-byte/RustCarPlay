use carplay_platform::usb::*;

// Binary USB configuration layouts reconstructed from the public interface
// signatures in DiPlay's IphoneCarPlayConfiguration and NcmFunctionDiscovery.
// These are protocol fixtures, NOT a claimed capture from connected hardware.
fn configuration(index: u8, value: u8, apple_ethernet: bool) -> Vec<u8> {
    let _ = index;
    let mut bytes = vec![
        9,
        2,
        0,
        0,
        if apple_ethernet { 4 } else { 3 },
        value,
        0,
        0x80,
        250,
    ];
    bytes.extend_from_slice(&[9, 4, 1, 0, 2, 0xff, 0xfe, 2, 0]);
    bytes.extend_from_slice(&[7, 5, 4, 2, 0, 2, 0, 7, 5, 0x85, 2, 0, 2, 0]);
    bytes.extend_from_slice(&[9, 4, 2, 0, 1, 2, 0x0d, 0, 0]);
    bytes.extend_from_slice(&[5, 0x24, 6, 2, 3]); // CDC Union: control 2, data 3.
    bytes.extend_from_slice(&[7, 5, 0x86, 3, 16, 0, 9]);
    bytes.extend_from_slice(&[9, 4, 3, 0, 0, 0x0a, 0, 1, 0]);
    bytes.extend_from_slice(&[9, 4, 3, 1, 2, 0x0a, 0, 1, 0]);
    bytes.extend_from_slice(&[7, 5, 7, 2, 0, 2, 0, 7, 5, 0x88, 2, 0, 2, 0]);
    if apple_ethernet {
        bytes.extend_from_slice(&[9, 4, 4, 0, 0, 0xff, 0xfd, 1, 0]);
    }
    let length = (bytes.len() as u16).to_le_bytes();
    bytes[2..4].copy_from_slice(&length);
    bytes
}

#[test]
fn selects_by_descriptors_not_configuration_value_or_interface_index() {
    let basic = parse_configuration(0, &configuration(0, 9, false)).unwrap();
    let preferred = parse_configuration(2, &configuration(2, 7, true)).unwrap();
    let found = match_carplay_configuration(&[basic, preferred]).unwrap();
    assert_eq!(
        (found.configuration_index, found.configuration_value),
        (2, 7)
    );
    assert_eq!(
        found.usbmux_endpoints,
        BulkPair {
            input: 0x85,
            output: 4
        }
    );
    assert_eq!(
        (
            found.ncm_control_interface,
            found.ncm_data_interface,
            found.ncm_data_alternate_setting
        ),
        (2, 3, 1)
    );
    assert_eq!(
        found.ncm_endpoints,
        BulkPair {
            input: 0x88,
            output: 7
        }
    );
    assert!(validate_winusb_configuration(&found, Some(7)).is_ok());
    assert_eq!(
        validate_winusb_configuration(&found, Some(1)),
        Err(WinUsbConfigurationError::NonFirstConfiguration { index: 2, value: 7 })
    );
}

#[test]
fn winusb_first_index_is_not_assumed_to_have_value_one() {
    let config = parse_configuration(0, &configuration(0, 9, false)).unwrap();
    let selected = match_carplay_configuration(&[config]).unwrap();
    assert!(validate_winusb_configuration(&selected, Some(9)).is_ok());
    assert_eq!(
        validate_winusb_configuration(&selected, Some(1)),
        Err(WinUsbConfigurationError::NotActive)
    );
}

#[test]
fn all_truncations_and_zero_length_descriptors_fail() {
    let bytes = configuration(0, 1, true);
    for length in 0..bytes.len() {
        assert!(parse_configuration(0, &bytes[..length]).is_err());
    }
    let mut broken = bytes.clone();
    broken[9] = 0;
    assert!(parse_configuration(0, &broken).is_err());
    let mut broken = bytes;
    broken[13] = 3;
    assert_eq!(
        parse_configuration(0, &broken),
        Err(DescriptorError::EndpointCount)
    );
}

#[test]
fn does_not_join_unrelated_ncm_function_or_vendor_ethernet() {
    let mut config = parse_configuration(0, &configuration(0, 1, true)).unwrap();
    config
        .interfaces
        .iter_mut()
        .find(|i| i.number == 2)
        .unwrap()
        .union_data_interface = Some(20);
    assert!(match_carplay_configuration(&[config.clone()]).is_none());
    config.interfaces.retain(|i| i.class != 2);
    assert!(match_carplay_configuration(&[config]).is_none());
}

#[test]
fn rejects_duplicate_endpoints_and_non_contiguous_claims() {
    let mut bytes = configuration(0, 1, false);
    bytes[27] = 4; // Second USBMUX endpoint duplicates the first.
    assert!(parse_configuration(0, &bytes).is_err());
    let mut bytes = configuration(0, 1, false);
    bytes[4] = 4;
    assert_eq!(
        parse_configuration(0, &bytes),
        Err(DescriptorError::InterfaceCount)
    );
}
