//! Apple USB discovery and descriptor-driven CarPlay selection.
//! `diagnose` is read-only; explicit device operations live in `native`.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub mod native;
#[cfg(target_os = "linux")]
pub mod system;
#[cfg(target_os = "windows")]
#[path = "usb/windows_system.rs"]
pub mod system;
#[cfg(target_os = "windows")]
mod windows_filter;
#[cfg(target_os = "windows")]
pub mod windows_setup;
use crate::diagnostics::DiagnosticIssue;
use serde::Serialize;

pub const APPLE_VENDOR_ID: u16 = 0x05ac;

/// All strings in this result exclude USB serial numbers and device instance
/// paths. IDs describe physical ports and survive the expected re-enumeration.
#[derive(Debug, Clone, Serialize)]
pub struct UsbPhone {
    pub id: String,
    /// Hash shared with the privileged Windows configuration helper. This is
    /// an instance selector, never a raw PnP path or serial number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_device_token: Option<String>,
    pub name: String,
    pub product_id: u16,
    pub active_configuration: Option<u8>,
    pub configuration: Option<CarPlayConfiguration>,
    pub readiness: Readiness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum Readiness {
    /// Descriptors pass preflight. Interface claim and the remaining protocol
    /// handshake still need to succeed; this never means CarPlay is connected.
    InterfacesAvailable,
    ModeSwitchRequired,
    DriverSetupRequired(String),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbEndpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
}
impl UsbEndpoint {
    fn bulk(&self) -> bool {
        self.attributes & 3 == 2
    }
    fn input(&self) -> bool {
        self.address & 0x80 != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbInterface {
    pub number: u8,
    pub alternate_setting: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<UsbEndpoint>,
    /// CDC Union descriptor subordinate interface number, when present.
    pub union_data_interface: Option<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbConfiguration {
    /// Descriptor index, distinct from bConfigurationValue.
    pub index: u8,
    pub value: u8,
    pub interfaces: Vec<UsbInterface>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DescriptorError {
    #[error("invalid or truncated configuration descriptor at byte {0}")]
    Malformed(usize),
    #[error("interface endpoint count does not match its descriptor")]
    EndpointCount,
    #[error("configuration interface count does not match its descriptor")]
    InterfaceCount,
}

/// Parse a complete USB configuration, including alternate settings and CDC
/// Union descriptors. Unknown class-specific descriptors are safely skipped.
pub fn parse_configuration(index: u8, bytes: &[u8]) -> Result<UsbConfiguration, DescriptorError> {
    if bytes.len() < 9 || bytes[0] < 9 || bytes[1] != 2 {
        return Err(DescriptorError::Malformed(0));
    }
    let total = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    if total != bytes.len() || bytes[0] as usize > total || bytes[5] == 0 {
        return Err(DescriptorError::Malformed(0));
    }
    let mut config = UsbConfiguration {
        index,
        value: bytes[5],
        interfaces: Vec::new(),
    };
    let mut expected_endpoints = None;
    let mut offset = bytes[0] as usize;
    while offset < total {
        if offset + 2 > total {
            return Err(DescriptorError::Malformed(offset));
        }
        let length = bytes[offset] as usize;
        if length < 2 || offset + length > total {
            return Err(DescriptorError::Malformed(offset));
        }
        let item = &bytes[offset..offset + length];
        match item[1] {
            4 => {
                if item.len() < 9 {
                    return Err(DescriptorError::Malformed(offset));
                }
                check_endpoint_count(config.interfaces.last(), expected_endpoints)?;
                if config
                    .interfaces
                    .iter()
                    .any(|i| i.number == item[2] && i.alternate_setting == item[3])
                {
                    return Err(DescriptorError::Malformed(offset));
                }
                config.interfaces.push(UsbInterface {
                    number: item[2],
                    alternate_setting: item[3],
                    class: item[5],
                    subclass: item[6],
                    protocol: item[7],
                    endpoints: Vec::new(),
                    union_data_interface: None,
                });
                expected_endpoints = Some(item[4] as usize);
            }
            5 => {
                if item.len() < 7 || item[2] & 0x0f == 0 || item[2] & 0x70 != 0 {
                    return Err(DescriptorError::Malformed(offset));
                }
                let interface = config
                    .interfaces
                    .last_mut()
                    .ok_or(DescriptorError::Malformed(offset))?;
                if interface.endpoints.iter().any(|e| e.address == item[2]) {
                    return Err(DescriptorError::Malformed(offset));
                }
                interface.endpoints.push(UsbEndpoint {
                    address: item[2],
                    attributes: item[3],
                    max_packet_size: u16::from_le_bytes([item[4], item[5]]),
                });
            }
            0x24 if item.len() >= 3 && item[2] == 6 => {
                if item.len() < 5 {
                    return Err(DescriptorError::Malformed(offset));
                }
                let interface = config
                    .interfaces
                    .last_mut()
                    .ok_or(DescriptorError::Malformed(offset))?;
                if interface.number != item[3] {
                    return Err(DescriptorError::Malformed(offset));
                }
                interface.union_data_interface = Some(item[4]);
            }
            2 => return Err(DescriptorError::Malformed(offset)),
            _ => {}
        }
        offset += length;
    }
    check_endpoint_count(config.interfaces.last(), expected_endpoints)?;
    let numbers: std::collections::BTreeSet<_> =
        config.interfaces.iter().map(|i| i.number).collect();
    if numbers.len() != bytes[4] as usize {
        return Err(DescriptorError::InterfaceCount);
    }
    Ok(config)
}

fn check_endpoint_count(
    interface: Option<&UsbInterface>,
    expected: Option<usize>,
) -> Result<(), DescriptorError> {
    if let (Some(interface), Some(expected)) = (interface, expected)
        && interface.endpoints.len() != expected
    {
        return Err(DescriptorError::EndpointCount);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BulkPair {
    pub input: u8,
    pub output: u8,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CarPlayConfiguration {
    pub configuration_index: u8,
    pub configuration_value: u8,
    pub usbmux_interface: u8,
    pub usbmux_alternate_setting: u8,
    pub usbmux_endpoints: BulkPair,
    pub ncm_control_interface: u8,
    pub ncm_data_interface: u8,
    pub ncm_data_alternate_setting: u8,
    pub ncm_endpoints: BulkPair,
    pub has_apple_ethernet: bool,
}

fn bulk_pair(interface: &UsbInterface, preferred: Option<(u8, u8)>) -> Option<BulkPair> {
    let endpoint = |input: bool, preferred: Option<u8>| {
        let endpoints: Vec<_> = interface
            .endpoints
            .iter()
            .filter(|e| e.bulk() && e.input() == input)
            .collect();
        preferred
            .and_then(|address| {
                endpoints
                    .iter()
                    .find(|e| e.address == address)
                    .map(|e| e.address)
            })
            .or_else(|| (endpoints.len() == 1).then(|| endpoints[0].address))
    };
    Some(BulkPair {
        input: endpoint(true, preferred.map(|p| p.0))?,
        output: endpoint(false, preferred.map(|p| p.1))?,
    })
}

/// Matches USBMUX ff/fe/02 and CDC NCM 02/0d with a usable data
/// alternate setting. Prefer configurations also exposing Apple ff/fd/01.
/// A vendor Ethernet interface alone is not mistaken for CDC NCM.
pub fn match_carplay_configuration(
    configurations: &[UsbConfiguration],
) -> Option<CarPlayConfiguration> {
    let mut candidates = Vec::new();
    for config in configurations {
        let Some((mux, mux_pair)) = config
            .interfaces
            .iter()
            .filter(|i| (i.class, i.subclass, i.protocol) == (0xff, 0xfe, 2))
            .find_map(|i| bulk_pair(i, Some((0x85, 0x04))).map(|pair| (i, pair)))
        else {
            continue;
        };
        for control in config
            .interfaces
            .iter()
            .filter(|i| i.class == 2 && i.subclass == 0x0d)
        {
            let mut data: Vec<_> = config
                .interfaces
                .iter()
                .filter(|i| {
                    i.class == 0x0a
                        && control
                            .union_data_interface
                            .is_none_or(|number| number == i.number)
                })
                .filter_map(|i| bulk_pair(i, None).map(|pair| (i, pair)))
                .collect();
            let data_numbers: std::collections::BTreeSet<_> =
                data.iter().map(|(i, _)| i.number).collect();
            if data_numbers.len() != 1 {
                continue;
            } // No guessing between unrelated CDC data functions.
            data.sort_by_key(|(i, _)| (i.alternate_setting != 1, i.alternate_setting));
            let Some((data, pair)) = data.into_iter().next() else {
                continue;
            };
            candidates.push(CarPlayConfiguration {
                configuration_index: config.index,
                configuration_value: config.value,
                usbmux_interface: mux.number,
                usbmux_alternate_setting: mux.alternate_setting,
                usbmux_endpoints: mux_pair.clone(),
                ncm_control_interface: control.number,
                ncm_data_interface: data.number,
                ncm_data_alternate_setting: data.alternate_setting,
                ncm_endpoints: pair,
                has_apple_ethernet: config
                    .interfaces
                    .iter()
                    .any(|i| (i.class, i.subclass, i.protocol) == (0xff, 0xfd, 1)),
            });
            break;
        }
    }
    candidates.sort_by_key(|c| (!c.has_apple_ethernet, c.configuration_index));
    candidates.into_iter().next()
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq, Serialize)]
pub enum WinUsbConfigurationError {
    #[error(
        "CarPlay USB configuration is not active (index {index}, value {value}); use Prepare Windows USB before connecting"
    )]
    NonFirstConfiguration { index: u8, value: u8 },
    #[error("selected USB configuration is not active; diagnostics will not change it")]
    NotActive,
}

/// WinUSB has no SET_CONFIGURATION API, but it can use a configuration already
/// selected by the composite parent driver (including a non-first one).
pub fn validate_winusb_configuration(
    selected: &CarPlayConfiguration,
    active_value: Option<u8>,
) -> Result<(), WinUsbConfigurationError> {
    if active_value == Some(selected.configuration_value) {
        return Ok(());
    }
    if selected.configuration_index != 0 {
        return Err(WinUsbConfigurationError::NonFirstConfiguration {
            index: selected.configuration_index,
            value: selected.configuration_value,
        });
    }
    Err(WinUsbConfigurationError::NotActive)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    WinUsb,
    WindowsComposite,
    AppleMobileDevice,
    LinuxUsbfs,
    Other,
    Unknown,
}
#[derive(Debug, Clone, Serialize)]
pub struct AppleUsbDevice {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Driver of the whole device; does not imply its child interfaces use WinUSB.
    pub device_driver: DriverKind,
    pub active_configuration_value: Option<u8>,
    pub configurations: Vec<UsbConfiguration>,
    pub carplay: Option<CarPlayConfiguration>,
    pub winusb_configuration_issue: Option<WinUsbConfigurationError>,
    pub issues: Vec<DiagnosticIssue>,
}
#[derive(Debug, Clone, Serialize)]
pub struct UsbDiagnostic {
    pub devices: Vec<AppleUsbDevice>,
    pub issue: Option<DiagnosticIssue>,
}

pub fn diagnose() -> UsbDiagnostic {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        use nusb::MaybeFuture;
        let devices = match nusb::list_devices().wait() {
            Ok(devices) => devices,
            Err(error) => {
                return UsbDiagnostic {
                    devices: Vec::new(),
                    issue: Some(DiagnosticIssue {
                        operation: "usb_enumeration",
                        category: format!("{:?}", error.kind()),
                        os_code: error.os_error().map(|code| code as i32),
                    }),
                };
            }
        };
        let mut output = Vec::new();
        for info in devices.filter(|info| info.vendor_id() == APPLE_VENDOR_ID) {
            #[cfg(target_os = "windows")]
            let driver = match info.driver().map(str::to_ascii_lowercase).as_deref() {
                Some("winusb") => DriverKind::WinUsb,
                Some("usbccgp") => DriverKind::WindowsComposite,
                Some("usbaapl") | Some("usbaapl64") => DriverKind::AppleMobileDevice,
                Some(_) => DriverKind::Other,
                None => DriverKind::Unknown,
            };
            #[cfg(target_os = "linux")]
            let driver = DriverKind::LinuxUsbfs;
            let mut device = AppleUsbDevice {
                vendor_id: info.vendor_id(),
                product_id: info.product_id(),
                device_driver: driver,
                active_configuration_value: None,
                configurations: Vec::new(),
                carplay: None,
                winusb_configuration_issue: None,
                issues: Vec::new(),
            };
            // Opening reads descriptors. No claim_interface/reset/set_configuration calls.
            match info.open().wait() {
                Ok(opened) => {
                    device.active_configuration_value = opened
                        .active_configuration()
                        .ok()
                        .map(|c| c.configuration_value());
                    let descriptors = opened.configurations().collect::<Vec<_>>();
                    // nusb's Windows cache drops descriptors whose reads fail.
                    // Enumerating a partial cache would renumber later entries
                    // and could misreport an actual second config as WinUSB's
                    // first. Refuse to infer descriptor indices in that case.
                    if descriptors.len()
                        != usize::from(opened.device_descriptor().num_configurations())
                    {
                        device.issues.push(DiagnosticIssue {
                            operation: "usb_configuration_index_validation",
                            category: "incomplete_descriptor_cache".into(),
                            os_code: None,
                        });
                        output.push(device);
                        continue;
                    }
                    for (index, descriptor) in descriptors.into_iter().enumerate() {
                        match parse_configuration(index as u8, descriptor.as_bytes()) {
                            Ok(config) => device.configurations.push(config),
                            Err(_) => device.issues.push(DiagnosticIssue {
                                operation: "usb_configuration_parse",
                                category: "malformed_descriptor".into(),
                                os_code: None,
                            }),
                        }
                    }
                    device.carplay = match_carplay_configuration(&device.configurations);
                    #[cfg(target_os = "windows")]
                    if let Some(selected) = &device.carplay {
                        device.winusb_configuration_issue = validate_winusb_configuration(
                            selected,
                            device.active_configuration_value,
                        )
                        .err();
                    }
                }
                Err(error) => device.issues.push(DiagnosticIssue {
                    operation: "usb_open_read_descriptors",
                    category: format!("{:?}", error.kind()),
                    os_code: error.os_error().map(|code| code as i32),
                }),
            }
            output.push(device);
        }
        UsbDiagnostic {
            devices: output,
            issue: None,
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        UsbDiagnostic {
            devices: Vec::new(),
            issue: Some(DiagnosticIssue::unsupported("usb_backend")),
        }
    }
}
