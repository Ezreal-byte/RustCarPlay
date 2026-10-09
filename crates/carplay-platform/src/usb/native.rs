// SPDX-License-Identifier: GPL-3.0-only
// USB bring-up follows DiPlay 9e244d9 IphoneUsbHost.kt / NcmUsbBridge.kt.
//! Explicit USB host operations. Enumeration never changes a device. `open`
//! can switch the selected iPhone into CarPlay mode, but never installs drivers
//! or detaches a kernel network driver. A claimed bulk pipe is not a completed
//! CarPlay connection: Lockdown, iAP2 and a host NCM network bridge are separate.

#[cfg(target_os = "windows")]
use super::validate_winusb_configuration;
use super::{
    APPLE_VENDOR_ID, CarPlayConfiguration, UsbConfiguration, match_carplay_configuration,
    parse_configuration,
};
pub use super::{Readiness, UsbPhone};
use nusb::transfer::{Bulk, ControlIn, ControlType, In, Out, Recipient};
use nusb::{Device, DeviceInfo, Interface, MaybeFuture};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
const READ_TIMEOUT: Duration = Duration::from_millis(250);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum UsbError {
    #[error("no USB iPhone found; connect it with a data cable and unlock it")]
    NotFound,
    #[error("more than one USB iPhone is connected; select a device")]
    SelectionRequired,
    #[error("USB operation cancelled")]
    Cancelled,
    #[error("timed out waiting for the selected iPhone to re-enumerate")]
    ReenumerationTimeout,
    #[error("USB operation requires a nonzero timeout")]
    InvalidTimeout,
    #[error("the selected iPhone is no longer present")]
    DeviceChanged,
    #[error("USB configuration descriptor cache is incomplete")]
    IncompleteDescriptors,
    #[error("the iPhone does not expose a USBMUX + CDC NCM configuration")]
    NoCarPlayConfiguration,
    #[error("Windows USB driver setup required: {0}")]
    WindowsDriverRequired(String),
    #[error("USB {operation} failed ({category}, system code {code:?})")]
    Native {
        operation: &'static str,
        category: String,
        code: Option<i64>,
    },
    #[error("USB protocol error: {0}")]
    Protocol(&'static str),
    #[error("USB system backend unavailable: {0}")]
    SystemBackend(&'static str),
    #[error("iPhone must be unlocked and Trust This Computer must be accepted")]
    TrustRequired,
    #[error("iPhone refused USB pairing")]
    TrustDenied,
    #[error("Lockdown {operation} failed with code {code}")]
    Lockdown { operation: &'static str, code: i32 },
    #[error("USB I/O: {0}")]
    Io(#[from] io::Error),
}

fn native(operation: &'static str, error: nusb::Error) -> UsbError {
    UsbError::Native {
        operation,
        category: format!("{:?}", error.kind()),
        code: error.os_error().map(|n| n as i64),
    }
}

fn port_id(info: &DeviceInfo) -> String {
    // nusb's bus_id can contain a Windows instance path. A deterministic hash
    // keeps it useful for local selection without exposing a hardware identity.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in info.bus_id().as_bytes().iter().chain(info.port_chain()) {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    format!("usb-{hash:016x}")
}

fn is_phone(info: &DeviceInfo) -> bool {
    info.vendor_id() == APPLE_VENDOR_ID
        && (info
            .product_string()
            .is_some_and(|s| s.to_ascii_lowercase().contains("iphone"))
            || info
                .interfaces()
                .any(|i| (i.class(), i.subclass(), i.protocol()) == (0xff, 0xfe, 2)))
}

fn infos() -> Result<Vec<DeviceInfo>, UsbError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|e| native("enumeration", e))?
        .filter(is_phone)
        .collect())
}

pub(super) fn select(id: Option<&str>) -> Result<DeviceInfo, UsbError> {
    let mut phones = infos()?;
    if let Some(id) = id {
        return phones
            .into_iter()
            .find(|i| port_id(i) == id)
            .ok_or(UsbError::DeviceChanged);
    }
    match phones.len() {
        0 => Err(UsbError::NotFound),
        1 => Ok(phones.remove(0)),
        _ => Err(UsbError::SelectionRequired),
    }
}

fn configurations(device: &Device) -> Result<Vec<UsbConfiguration>, UsbError> {
    let descriptors = device.configurations().collect::<Vec<_>>();
    if descriptors.len() != usize::from(device.device_descriptor().num_configurations()) {
        return Err(UsbError::IncompleteDescriptors);
    }
    descriptors
        .into_iter()
        .enumerate()
        .map(|(index, descriptor)| {
            parse_configuration(index as u8, descriptor.as_bytes())
                .map_err(|_| UsbError::Protocol("malformed USB configuration descriptor"))
        })
        .collect()
}

fn inspect(info: &DeviceInfo) -> UsbPhone {
    #[cfg(target_os = "windows")]
    let windows_device_token = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(
            info.instance_id()
                .to_string_lossy()
                .to_uppercase()
                .as_bytes(),
        );
        Some(format!(
            "winusb-{}",
            digest[..8]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>()
        ))
    };
    #[cfg(not(target_os = "windows"))]
    let windows_device_token = None;
    let mut phone = UsbPhone {
        id: port_id(info),
        windows_device_token,
        name: info.product_string().unwrap_or("USB iPhone").to_owned(),
        product_id: info.product_id(),
        active_configuration: None,
        configuration: None,
        readiness: Readiness::ModeSwitchRequired,
    };
    let result = (|| {
        let device = info.open().wait().map_err(|e| native("open", e))?;
        phone.active_configuration = device
            .active_configuration()
            .ok()
            .map(|d| d.configuration_value());
        phone.configuration = match_carplay_configuration(&configurations(&device)?);
        if let Some(selected) = &phone.configuration {
            #[cfg(target_os = "windows")]
            {
                validate_winusb_configuration(selected, phone.active_configuration)
                    .map_err(|e| UsbError::WindowsDriverRequired(e.to_string()))?;
            }
            let _ = selected;
            phone.readiness = Readiness::InterfacesAvailable;
        } else {
            #[cfg(target_os = "windows")]
            ensure_windows_driver(info)?;
        }
        Ok::<_, UsbError>(())
    })();
    if let Err(error) = result {
        phone.readiness = match error {
            UsbError::WindowsDriverRequired(reason) => Readiness::DriverSetupRequired(reason),
            error => Readiness::Unavailable(error.to_string()),
        };
    }
    phone
}

/// Read-only discovery for the mode selector; no interface is claimed.
pub fn discover() -> Result<Vec<UsbPhone>, UsbError> {
    Ok(infos()?.iter().map(inspect).collect())
}

/// Read-only preflight for a selected device, or the sole attached iPhone.
pub fn probe(id: Option<&str>) -> Result<UsbPhone, UsbError> {
    Ok(inspect(&select(id)?))
}

#[cfg(target_os = "windows")]
fn ensure_windows_driver(info: &DeviceInfo) -> Result<(), UsbError> {
    if info
        .driver()
        .is_some_and(|d| d.eq_ignore_ascii_case("winusb") || d.eq_ignore_ascii_case("usbccgp"))
    {
        return Ok(());
    }
    Err(UsbError::WindowsDriverRequired(
        "the device uses Apple's driver or has no compatible WinUSB interface; automatic driver replacement is disabled".into(),
    ))
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), UsbError> {
    if cancel.load(Ordering::Acquire) {
        Err(UsbError::Cancelled)
    } else {
        Ok(())
    }
}

fn switch_request() -> ControlIn {
    ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Device,
        request: 0x52,
        value: 0,
        index: 4,
        length: 1,
    }
}

fn switch_mode(info: &DeviceInfo, device: &Device) -> Result<(), UsbError> {
    #[cfg(target_os = "linux")]
    let response = device.control_in(switch_request(), CONTROL_TIMEOUT).wait();
    #[cfg(target_os = "windows")]
    let response = {
        ensure_windows_driver(info)?;
        let mux = device
            .active_configuration()
            .ok()
            .and_then(|c| parse_configuration(0, c.as_bytes()).ok())
            .and_then(|c| {
                c.interfaces
                    .into_iter()
                    .find(|i| (i.class, i.subclass, i.protocol) == (0xff, 0xfe, 2))
            })
            .ok_or(UsbError::Protocol(
                "active configuration has no USBMUX interface",
            ))?;
        let interface = match device.claim_interface(mux.number).wait() {
            Ok(interface) => interface,
            // Apple's upper filter can expose a WinUSB service but reject
            // WinUsb_Initialize. The optional parent filter sends only the
            // vendor request, preserving Apple USBMUX ownership.
            Err(_) => return super::windows_filter::switch_mode(info),
        };
        interface
            .control_in(switch_request(), CONTROL_TIMEOUT)
            .wait()
    };
    let _ = info;
    match response {
        Ok(bytes) if bytes.as_slice() == [0] => Ok(()),
        // A successful mode change can tear down endpoint zero before its
        // completion arrives. The caller still requires observed re-enumeration.
        Err(nusb::transfer::TransferError::Disconnected) => Ok(()),
        Ok(_) => Err(UsbError::Protocol(
            "iPhone rejected CarPlay USB mode or returned an invalid response",
        )),
        Err(error) => Err(UsbError::Native {
            operation: "CarPlay mode switch",
            category: format!("{error:?}"),
            code: None,
        }),
    }
}

/// Two bounded native bulk streams. Interface handles stay alive until the
/// streams are dropped, then nusb releases them. No system driver is detached.
pub struct UsbTransport {
    pub mux: BulkPipe,
    pub ncm: BulkPipe,
    pub configuration: CarPlayConfiguration,
    _control: Interface,
    _device: Device,
}

pub struct BulkPipe {
    input: nusb::io::EndpointRead<Bulk>,
    output: nusb::io::EndpointWrite<Bulk>,
}

impl BulkPipe {
    fn new(interface: &Interface, endpoints: &super::BulkPair) -> Result<Self, UsbError> {
        Ok(Self {
            input: interface
                .endpoint::<Bulk, In>(endpoints.input)
                .map_err(|e| native("bulk input", e))?
                .reader(16 * 1024)
                .with_num_transfers(2)
                .with_read_timeout(READ_TIMEOUT),
            output: interface
                .endpoint::<Bulk, Out>(endpoints.output)
                .map_err(|e| native("bulk output", e))?
                .writer(16 * 1024)
                .with_write_timeout(WRITE_TIMEOUT),
        })
    }

    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.input.set_read_timeout(timeout);
    }

    /// Split USB receive/transmit so a multiplexor can receive while writing.
    pub fn split(self) -> (nusb::io::EndpointRead<Bulk>, nusb::io::EndpointWrite<Bulk>) {
        (self.input, self.output)
    }
}

impl Read for BulkPipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}

impl Write for BulkPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush_end()
    }
}

/// Open the selected phone, request CarPlay mode if necessary, wait for the
/// *same physical port and serial* to return, then claim USBMUX and NCM. Runs on
/// a worker thread. Windows alternate configurations fail before any mutation.
pub fn open(
    id: Option<&str>,
    cancel: &AtomicBool,
    reenumeration_timeout: Duration,
) -> Result<UsbTransport, UsbError> {
    let (_, device, selected) = prepare_device(id, cancel, reenumeration_timeout)?;
    let claim = |number| {
        check_cancel(cancel)?;
        device
            .claim_interface(number)
            .wait()
            .map_err(|e| native("claim interface", e))
    };
    let mux = claim(selected.usbmux_interface)?;
    if selected.usbmux_alternate_setting != 0 {
        mux.set_alt_setting(selected.usbmux_alternate_setting)
            .wait()
            .map_err(|e| native("USBMUX alternate setting", e))?;
    }
    let control = claim(selected.ncm_control_interface)?;
    let data = if selected.ncm_control_interface == selected.ncm_data_interface {
        control.clone()
    } else {
        claim(selected.ncm_data_interface)?
    };
    data.set_alt_setting(selected.ncm_data_alternate_setting)
        .wait()
        .map_err(|e| native("NCM alternate setting", e))?;
    Ok(UsbTransport {
        mux: BulkPipe::new(&mux, &selected.usbmux_endpoints)?,
        ncm: BulkPipe::new(&data, &selected.ncm_endpoints)?,
        configuration: selected,
        _control: control,
        _device: device,
    })
}

/// Request Apple's CarPlay USB mode and wait for the same physical device to
/// expose its new descriptors. This does not select a configuration, install a
/// driver, or claim a media interface. Used by the Windows configuration helper.
pub fn request_carplay_mode(
    id: Option<&str>,
    cancel: &AtomicBool,
    reenumeration_timeout: Duration,
) -> Result<UsbPhone, UsbError> {
    let (info, _, _) = prepare_mode(id, cancel, reenumeration_timeout)?;
    Ok(inspect(&info))
}

fn prepare_mode(
    id: Option<&str>,
    cancel: &AtomicBool,
    reenumeration_timeout: Duration,
) -> Result<(DeviceInfo, Device, CarPlayConfiguration), UsbError> {
    if reenumeration_timeout.is_zero() {
        return Err(UsbError::InvalidTimeout);
    }
    check_cancel(cancel)?;
    let mut info = select(id)?;
    let original_id = port_id(&info);
    let original_serial = info.serial_number().map(str::to_owned);
    let mut device = info.open().wait().map_err(|e| native("open", e))?;
    let mut selection = match_carplay_configuration(&configurations(&device)?);
    if selection.is_none() {
        let original_serial = original_serial.as_deref().filter(|s| normalized_usb_serial(s.as_bytes()).is_some()).ok_or(
            UsbError::SystemBackend("USB serial unavailable; cannot safely track the selected phone through mode switching"),
        )?;
        let deadline = Instant::now()
            .checked_add(reenumeration_timeout)
            .ok_or(UsbError::InvalidTimeout)?;
        switch_mode(&info, &device)?;
        drop(device);
        loop {
            check_cancel(cancel)?;
            if Instant::now() >= deadline {
                return Err(UsbError::ReenumerationTimeout);
            }
            let found = infos()?.into_iter().find(|i| port_id(i) == original_id);
            if let Some(new_info) = found {
                // Windows may preserve the same devinst, and the detach may
                // be shorter than a polling interval. Fresh CarPlay descriptors
                // on the same port and serial are the re-enumeration evidence.
                if !same_reenumerated_phone(original_serial, new_info.serial_number())? {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                if let Ok(new_device) = new_info.open().wait()
                    && let Ok(configs) = configurations(&new_device)
                {
                    selection = match_carplay_configuration(&configs);
                    if selection.is_some() {
                        info = new_info;
                        device = new_device;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let selected = selection.ok_or(UsbError::NoCarPlayConfiguration)?;
    check_cancel(cancel)?;
    Ok((info, device, selected))
}

/// Normalize a USB identity while preserving strict character validation.
pub(super) fn normalized_usb_serial(bytes: &[u8]) -> Option<Vec<u8>> {
    // The real Windows hub descriptor can return 24 serial characters padded
    // to 40 with NULs, while libusb returns only the characters. Remove only
    // that tail; embedded NULs, whitespace and non-hex identity text are invalid.
    let end = bytes.iter().rposition(|b| *b != 0)? + 1;
    let bytes = &bytes[..end];
    if bytes.iter().any(|b| *b != b'-' && !b.is_ascii_hexdigit()) {
        return None;
    }
    let normalized: Vec<_> = bytes
        .iter()
        .copied()
        .filter(|b| *b != b'-')
        .map(|b| b.to_ascii_lowercase())
        .collect();
    (16..=64).contains(&normalized.len()).then_some(normalized)
}

/// The port is matched separately. Serial continuity distinguishes the same
/// phone returning with fresh descriptors from another phone plugged into that
/// port. Windows may preserve the same devinst across a successful switch.
fn same_reenumerated_phone(expected: &str, actual: Option<&str>) -> Result<bool, UsbError> {
    let expected = normalized_usb_serial(expected.as_bytes()).ok_or(UsbError::DeviceChanged)?;
    let Some(actual) = actual.filter(|s| !s.is_empty()) else {
        // PnP can expose a device before its string descriptors are populated.
        return Ok(false);
    };
    if normalized_usb_serial(actual.as_bytes()).is_some_and(|actual| actual == expected) {
        Ok(true)
    } else {
        Err(UsbError::DeviceChanged)
    }
}

pub(super) fn prepare_device(
    id: Option<&str>,
    cancel: &AtomicBool,
    reenumeration_timeout: Duration,
) -> Result<(DeviceInfo, Device, CarPlayConfiguration), UsbError> {
    let (info, device, selected) = prepare_mode(id, cancel, reenumeration_timeout)?;
    #[cfg(target_os = "windows")]
    {
        validate_winusb_configuration(
            &selected,
            device
                .active_configuration()
                .ok()
                .map(|c| c.configuration_value()),
        )
        .map_err(|e| UsbError::WindowsDriverRequired(e.to_string()))?;
    }
    #[cfg(target_os = "linux")]
    if device
        .active_configuration()
        .ok()
        .map(|c| c.configuration_value())
        != Some(selected.configuration_value)
    {
        device
            .set_configuration(selected.configuration_value)
            .wait()
            .map_err(|e| native("select CarPlay configuration", e))?;
    }
    Ok((info, device, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reenumeration_waits_for_identity_and_rejects_replacement_on_same_port() {
        let serial = "00008120-0006ABCDEF123456";
        assert!(!same_reenumerated_phone(serial, None).unwrap());
        assert!(!same_reenumerated_phone(serial, Some("")).unwrap());
        assert!(same_reenumerated_phone(serial, Some("000081200006abcdef123456")).unwrap());
        assert!(matches!(
            same_reenumerated_phone(serial, Some("00008120-0006ABCDEF123457")),
            Err(UsbError::DeviceChanged)
        ));
    }

    #[test]
    fn reenumeration_accepts_only_terminal_nul_padding_variations() {
        let serial = "000081200006ABCDEF123456";
        let padded = format!("{serial}{}", "\0".repeat(16));
        let differently_padded = format!("{serial}\0");
        assert!(same_reenumerated_phone(serial, Some(&padded)).unwrap());
        assert!(same_reenumerated_phone(&padded, Some(serial)).unwrap());
        assert!(same_reenumerated_phone(&padded, Some(&differently_padded)).unwrap());
        for invalid in [
            "00008120\x000006ABCDEF123456",
            "000081200006ABCDEF123456 ",
            "000081200006ABCDEF123456\0x",
            "not-a-usb-device-identity",
            "\0\0",
        ] {
            assert!(matches!(
                same_reenumerated_phone(serial, Some(invalid)),
                Err(UsbError::DeviceChanged)
            ));
            assert!(matches!(
                same_reenumerated_phone(invalid, Some(invalid)),
                Err(UsbError::DeviceChanged)
            ));
        }
        assert!(matches!(
            same_reenumerated_phone(&padded, Some("000081200006ABCDEF123457\0")),
            Err(UsbError::DeviceChanged)
        ));
    }

    #[test]
    fn mode_switch_matches_diplay_vendor_request() {
        let request = switch_request();
        assert_eq!(request.control_type, ControlType::Vendor);
        assert_eq!(request.recipient, Recipient::Device);
        assert_eq!(
            (
                request.request,
                request.value,
                request.index,
                request.length
            ),
            (0x52, 0, 4, 1)
        );
    }

    #[test]
    fn cancelled_or_invalid_open_does_not_enumerate_or_touch_hardware() {
        assert!(matches!(
            open(None, &AtomicBool::new(false), Duration::ZERO),
            Err(UsbError::InvalidTimeout)
        ));
        assert!(matches!(
            open(None, &AtomicBool::new(true), Duration::from_secs(1)),
            Err(UsbError::Cancelled)
        ));
    }
}
