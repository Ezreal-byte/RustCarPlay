// SPDX-License-Identifier: GPL-3.0-only
//! Optional, explicitly installed libusb-win32 parent filter for Apple's mode
//! request. Bulk USBMUX remains owned by Apple; NCM remains owned by Windows.
//! ABI: upstream libusb-win32 1.4.0.2 lusb0_usb.h (cdecl, Windows x64).
use super::native::UsbError;
use libloading::Library;
use nusb::DeviceInfo;
use std::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::Mutex;

static API_LOCK: Mutex<()> = Mutex::new(());
type Handle = *mut c_void;

// The upstream header's pshpack1.h applies to every descriptor AND list
// structure until poppack.h after usb_version, including Device and Bus.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Descriptor {
    length: u8,
    kind: u8,
    usb: u16,
    class: u8,
    subclass: u8,
    protocol: u8,
    packet_size: u8,
    vendor: u16,
    product: u16,
    device: u16,
    manufacturer: u8,
    product_string: u8,
    serial: u8,
    configurations: u8,
}
#[repr(C, packed)]
struct Device {
    next: *mut Device,
    previous: *mut Device,
    filename: [c_char; 512],
    bus: *mut Bus,
    descriptor: Descriptor,
    configurations: *mut c_void,
    private: *mut c_void,
    number: u8,
    child_count: u8,
    children: *mut *mut Device,
}
#[repr(C, packed)]
struct Bus {
    next: *mut Bus,
    previous: *mut Bus,
    dirname: [c_char; 512],
    devices: *mut Device,
    location: u32,
    root: *mut Device,
}
type Init = unsafe extern "C" fn();
type SetDebug = unsafe extern "C" fn(c_int);
type Find = unsafe extern "C" fn() -> c_int;
type Busses = unsafe extern "C" fn() -> *mut Bus;
type Open = unsafe extern "C" fn(*mut Device) -> Handle;
type Close = unsafe extern "C" fn(Handle) -> c_int;
type StringDescriptor = unsafe extern "C" fn(Handle, c_int, *mut c_char, usize) -> c_int;
type Control =
    unsafe extern "C" fn(Handle, c_int, c_int, c_int, c_int, *mut c_char, c_int, c_int) -> c_int;

struct Api {
    init: Init,
    debug: SetDebug,
    find_busses: Find,
    find_devices: Find,
    busses: Busses,
    open: Open,
    close: Close,
    string: StringDescriptor,
    control: Control,
    _library: Library,
}

fn library_path() -> Result<PathBuf, UsbError> {
    let dir = if let Some(path) = std::env::var_os("RUSTCARPLAY_USB_FILTER_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(UsbError::SystemBackend(
                "RUSTCARPLAY_USB_FILTER_DIR must be an absolute directory",
            ));
        }
        path
    } else {
        std::env::current_exe()?
            .parent()
            .ok_or(UsbError::SystemBackend(
                "cannot locate executable directory",
            ))?
            .join("runtime/usb-filter")
    };
    Ok(dir.join("libusb0.dll"))
}

impl Api {
    fn load() -> Result<Self, UsbError> {
        let path = library_path()?;
        // SAFETY: absolute, explicitly configured library; transitive imports
        // are restricted to that directory and System32. Its ABI is retained.
        unsafe {
            let library: Library = libloading::os::windows::Library::load_with_flags(
                &path,
                libloading::os::windows::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                    | libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32,
            ).map_err(|_| UsbError::WindowsDriverRequired(
                "Apple's interface cannot receive standard WinUSB requests; prepare the per-device USB filter with scripts/windows-usb-filter.ps1".into(),
            ))?.into();
            macro_rules! symbol {
                ($name:literal, $ty:ty) => {
                    *library
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|_| UsbError::SystemBackend("incompatible libusb-win32 runtime"))?
                };
            }
            Ok(Self {
                init: symbol!("usb_init", Init),
                debug: symbol!("usb_set_debug", SetDebug),
                find_busses: symbol!("usb_find_busses", Find),
                find_devices: symbol!("usb_find_devices", Find),
                busses: symbol!("usb_get_busses", Busses),
                open: symbol!("usb_open", Open),
                close: symbol!("usb_close", Close),
                string: symbol!("usb_get_string_simple", StringDescriptor),
                control: symbol!("usb_control_msg", Control),
                _library: library,
            })
        }
    }
}

struct OpenDevice<'a> {
    handle: Handle,
    api: &'a Api,
}
impl Drop for OpenDevice<'_> {
    fn drop(&mut self) {
        // SAFETY: this handle was opened by this API, is uniquely owned, and
        // closes while the library and global serialization guard remain alive.
        unsafe {
            (self.api.close)(self.handle);
        }
    }
}

fn same_serial(expected: &str, actual: &[u8]) -> bool {
    let Some(expected) = super::native::normalized_usb_serial(expected.as_bytes()) else {
        return false;
    };
    super::native::normalized_usb_serial(actual).is_some_and(|actual| actual == expected)
}

/// Only send mode 4 to the exact selected device after a serial-descriptor
/// match; never mutate an arbitrary Apple device or install drivers here.
pub(super) fn switch_mode(info: &DeviceInfo) -> Result<(), UsbError> {
    let expected_serial = info
        .serial_number()
        .filter(|s| super::native::normalized_usb_serial(s.as_bytes()).is_some())
        .ok_or(UsbError::DeviceChanged)?;
    let _guard = API_LOCK
        .lock()
        .map_err(|_| UsbError::SystemBackend("USB filter lock poisoned"))?;
    let api = Api::load()?;
    // SAFETY: ABI functions loaded above, called while holding the process-wide
    // lock. All list pointers are owned by this library and used before rescan.
    unsafe {
        (api.debug)(0);
        (api.init)();
        if (api.find_busses)() < 0 || (api.find_devices)() < 0 {
            return Err(UsbError::WindowsDriverRequired(
                "USB filter enumeration failed".into(),
            ));
        }
        let mut bus = (api.busses)();
        let mut seen_busses = std::collections::HashSet::new();
        while !bus.is_null() && seen_busses.len() < 256 && seen_busses.insert(bus as usize) {
            let mut device = (*bus).devices;
            let mut seen_devices = std::collections::HashSet::new();
            while !device.is_null()
                && seen_devices.len() < 256
                && seen_devices.insert(device as usize)
            {
                let descriptor = std::ptr::addr_of!((*device).descriptor).read_unaligned();
                if descriptor.vendor == super::APPLE_VENDOR_ID
                    && descriptor.product == info.product_id()
                    && descriptor.serial != 0
                {
                    let handle = (api.open)(device);
                    if !handle.is_null() {
                        let opened = OpenDevice { handle, api: &api };
                        let mut serial = [0_u8; 512];
                        let size = (api.string)(
                            opened.handle,
                            descriptor.serial.into(),
                            serial.as_mut_ptr().cast(),
                            serial.len(),
                        );
                        if size > 0
                            && (size as usize) <= serial.len()
                            && same_serial(expected_serial, &serial[..size as usize])
                        {
                            let mut status = [0xff_u8];
                            let received = (api.control)(
                                opened.handle,
                                0xc0,
                                0x52,
                                0,
                                4,
                                status.as_mut_ptr().cast(),
                                1,
                                2000,
                            );
                            return validate_response(received, status[0]);
                        }
                    }
                }
                device = (*device).next;
            }
            bus = (*bus).next;
        }
    }
    Err(UsbError::WindowsDriverRequired("selected iPhone is not reachable through the per-device filter; verify its installation and reconnect the cable".into()))
}

fn validate_response(received: i32, status: u8) -> Result<(), UsbError> {
    match (received, status) {
        (1, 0) | (-19, _) => Ok(()), // ENODEV: caller must still observe re-enumeration.
        (1, _) => Err(UsbError::Protocol(
            "iPhone rejected CarPlay USB mode; reconnect the data cable",
        )),
        _ => Err(UsbError::Native {
            operation: "USB filter CarPlay mode switch",
            category: "control transfer failed".into(),
            code: Some(i64::from(received)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_serial_required_before_mode_mutation() {
        assert!(same_serial("00008130-1234ABCD", b"000081301234abcd"));
        assert!(!same_serial("", b""));
        assert!(!same_serial("000081301234abcd", b"000081301234abce"));
    }
    #[test]
    fn padded_hub_serial_matches_unpadded_libusb_without_accepting_embedded_nuls() {
        let serial = "000081200006ABCDEF123456";
        let padded = format!("{serial}{}", "\0".repeat(16));
        assert!(same_serial(&padded, serial.as_bytes()));
        assert!(same_serial(serial, padded.as_bytes()));
        assert!(!same_serial(&padded, b"000081200006ABCDEF123457"));
        assert!(!same_serial(serial, b"00008120\x000006ABCDEF123456"));
        assert!(!same_serial(serial, b"000081200006ABCDEF123456\0x"));
        assert!(!same_serial("\0\0", b"\0"));
        assert!(!same_serial(
            "000081200006ABCDEF123456 ",
            b"000081200006ABCDEF123456 "
        ));
    }
    #[test]
    fn nonzero_mode_status_is_not_success() {
        assert!(validate_response(1, 0).is_ok());
        assert!(validate_response(1, 1).is_err());
        assert!(validate_response(0, 0).is_err());
        assert!(validate_response(-5, 0).is_err());
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn win64_libusb01_layout_matches_public_header() {
        assert_eq!(std::mem::size_of::<Descriptor>(), 18);
        assert_eq!(std::mem::offset_of!(Device, descriptor), 536);
        assert_eq!(std::mem::align_of::<Descriptor>(), 1);
        assert_eq!(std::mem::align_of::<Device>(), 1);
        assert_eq!(std::mem::offset_of!(Device, configurations), 554);
        assert_eq!(std::mem::offset_of!(Device, children), 572);
        assert_eq!(std::mem::size_of::<Device>(), 580);
        assert_eq!(std::mem::offset_of!(Bus, devices), 528);
        assert_eq!(std::mem::offset_of!(Bus, root), 540);
        assert_eq!(std::mem::size_of::<Bus>(), 548);
    }
}
