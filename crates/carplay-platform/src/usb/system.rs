// SPDX-License-Identifier: GPL-3.0-only
//! Linux wired CarPlay over the system usbmuxd / libimobiledevice stack and
//! cdc_ncm network driver. Credentials remain in the system pairing store.
//! The dynamic FFI signatures follow libimobiledevice's public headers.
use super::native::{UsbError, prepare_device};
use crate::network;
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
type NewDevice = unsafe extern "C" fn(*mut Handle, *const c_char, c_int) -> c_int;
type Free = unsafe extern "C" fn(Handle) -> c_int;
type ListDevices = unsafe extern "C" fn(*mut *mut *mut c_char, *mut c_int) -> c_int;
type FreeList = unsafe extern "C" fn(*mut *mut c_char) -> c_int;
type NewLockdown = unsafe extern "C" fn(Handle, *mut Handle, *const c_char) -> c_int;
type StartService = unsafe extern "C" fn(Handle, *const c_char, *mut *mut Service) -> c_int;
type FreeService = unsafe extern "C" fn(*mut Service) -> c_int;
type Connect = unsafe extern "C" fn(Handle, u16, *mut Handle) -> c_int;
type GetFd = unsafe extern "C" fn(Handle, *mut c_int) -> c_int;
type SendBytes = unsafe extern "C" fn(Handle, *const c_char, u32, *mut u32) -> c_int;
type Receive = unsafe extern "C" fn(Handle, *mut c_char, u32, *mut u32, u32) -> c_int;

#[repr(C)]
struct Service {
    port: u16,
    ssl_enabled: u8,
    identifier: *mut c_char,
}

struct Api {
    new_device: NewDevice,
    free_device: Free,
    list_devices: ListDevices,
    free_list: FreeList,
    new_lockdown: NewLockdown,
    free_lockdown: Free,
    start_service: StartService,
    free_service: FreeService,
    connect: Connect,
    disconnect: Free,
    enable_ssl: Free,
    get_fd: GetFd,
    send: SendBytes,
    receive: Receive,
    _library: Library,
}

impl Api {
    fn load() -> Result<Self, UsbError> {
        // SAFETY: libimobiledevice.so.6 is the distro-provided ABI. Symbols
        // below match its public C headers; the library outlives all handles.
        unsafe {
            let library = Library::new("libimobiledevice-1.0.so.6").map_err(|_| {
                UsbError::SystemBackend(
                    "install libimobiledevice6 and usbmuxd, then start the usbmuxd service",
                )
            })?;
            macro_rules! symbol {
                ($name:literal, $ty:ty) => {
                    *library
                        .get::<$ty>(concat!($name, "\0").as_bytes())
                        .map_err(|_| UsbError::SystemBackend("incompatible libimobiledevice ABI"))?
                };
            }
            Ok(Self {
                new_device: symbol!("idevice_new_with_options", NewDevice),
                free_device: symbol!("idevice_free", Free),
                list_devices: symbol!("idevice_get_device_list", ListDevices),
                free_list: symbol!("idevice_device_list_free", FreeList),
                new_lockdown: symbol!("lockdownd_client_new_with_handshake", NewLockdown),
                free_lockdown: symbol!("lockdownd_client_free", Free),
                start_service: symbol!("lockdownd_start_service", StartService),
                free_service: symbol!("lockdownd_service_descriptor_free", FreeService),
                connect: symbol!("idevice_connect", Connect),
                disconnect: symbol!("idevice_disconnect", Free),
                enable_ssl: symbol!("idevice_connection_enable_ssl", Free),
                get_fd: symbol!("idevice_connection_get_fd", GetFd),
                send: symbol!("idevice_connection_send", SendBytes),
                receive: symbol!("idevice_connection_receive_timeout", Receive),
                _library: library,
            })
        }
    }

    fn matching_udid(&self, serial: &str) -> Result<Option<CString>, UsbError> {
        let mut list = ptr::null_mut();
        let mut count = 0;
        // SAFETY: initialized writable output slots. The returned list is
        // library-owned until free_list; each returned UDID is NUL-terminated.
        let status = unsafe { (self.list_devices)(&mut list, &mut count) };
        if status != 0 {
            return Err(UsbError::SystemBackend("usbmuxd is not available"));
        }
        let mut matched = None;
        if !list.is_null() && (0..=256).contains(&count) {
            for index in 0..count as usize {
                // SAFETY: index is bounded by the count returned with this list.
                let entry = unsafe { *list.add(index) };
                if !entry.is_null() {
                    // SAFETY: each non-null entry is a library-owned C string.
                    let value = unsafe { CStr::from_ptr(entry) };
                    if value.to_str().is_ok_and(|s| same_udid(serial, s)) {
                        matched = Some(value.to_owned());
                        break;
                    }
                }
            }
        }
        if !list.is_null() {
            // SAFETY: list was obtained from this same library and freed once.
            unsafe { (self.free_list)(list) };
        }
        Ok(matched)
    }
}

fn same_udid(serial: &str, udid: &str) -> bool {
    let canonical = |s: &str| {
        s.bytes()
            .filter(|b| *b != b'-')
            .map(|b| b.to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    let serial = canonical(serial);
    !serial.is_empty() && serial == canonical(udid)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), UsbError> {
    if cancel.load(Ordering::Acquire) {
        Err(UsbError::Cancelled)
    } else {
        Ok(())
    }
}

fn lockdown_error(operation: &'static str, code: i32) -> UsbError {
    match code {
        -17 | -19 => UsbError::TrustRequired,
        -18 => UsbError::TrustDenied,
        _ => UsbError::Lockdown { operation, code },
    }
}

/// An actual CarKit service, with its creating Lockdown session kept alive.
/// This type is deliberately not Clone or Sync. One protocol worker owns it.
pub struct CarKitStream {
    api: Api,
    device: Handle,
    lockdown: Handle,
    connection: Handle,
    fd: c_int,
    tls: bool,
    read_timeout_ms: AtomicU32,
    closed: AtomicBool,
}

// SAFETY: owned native handles can be moved between threads, but are never
// shared concurrently. All libimobiledevice I/O is through &mut self.
unsafe impl Send for CarKitStream {}

impl CarKitStream {
    fn open(api: Api, udid: &CStr, cancel: &AtomicBool) -> Result<Self, UsbError> {
        let mut stream = Self {
            api,
            device: ptr::null_mut(),
            lockdown: ptr::null_mut(),
            connection: ptr::null_mut(),
            fd: -1,
            tls: false,
            read_timeout_ms: AtomicU32::new(100),
            closed: AtomicBool::new(false),
        };
        check_cancel(cancel)?;
        // SAFETY: live API and output slot; UDID is NUL-terminated. USBMUX-only
        // lookup (flag 2) cannot silently switch to a Wi-Fi-paired iPhone.
        let status = unsafe { (stream.api.new_device)(&mut stream.device, udid.as_ptr(), 2) };
        if status != 0 || stream.device.is_null() {
            return Err(UsbError::SystemBackend(
                "selected iPhone is not available through usbmuxd",
            ));
        }
        // SAFETY: live device and output pointer; returned handle is owned here.
        let status = unsafe {
            (stream.api.new_lockdown)(stream.device, &mut stream.lockdown, c"RustCarPlay".as_ptr())
        };
        if status != 0 || stream.lockdown.is_null() {
            return Err(lockdown_error("pair/start session", status));
        }
        check_cancel(cancel)?;
        let mut service: *mut Service = ptr::null_mut();
        // SAFETY: live session, constant service name, writable result pointer.
        let status = unsafe {
            (stream.api.start_service)(
                stream.lockdown,
                c"com.apple.carkit.service".as_ptr(),
                &mut service,
            )
        };
        if status != 0 || service.is_null() {
            if !service.is_null() {
                // SAFETY: the optional result is owned here even on error.
                unsafe { (stream.api.free_service)(service) };
            }
            return Err(lockdown_error("start CarKit service", status));
        }
        // SAFETY: successful start_service returned this valid descriptor.
        let (port, ssl) = unsafe { ((*service).port, (*service).ssl_enabled != 0) };
        // SAFETY: descriptor is no longer needed and freed exactly once.
        unsafe { (stream.api.free_service)(service) };
        if port == 0 {
            return Err(UsbError::Protocol(
                "Lockdown returned an invalid CarKit port",
            ));
        }
        check_cancel(cancel)?;
        // SAFETY: live device and writable connection output.
        let status = unsafe { (stream.api.connect)(stream.device, port, &mut stream.connection) };
        if status != 0 || stream.connection.is_null() {
            return Err(lockdown_error("connect CarKit service", status));
        }
        // SAFETY: live connection and writable descriptor slot. The library
        // retains fd ownership; we only set socket options or shutdown it.
        let status = unsafe { (stream.api.get_fd)(stream.connection, &mut stream.fd) };
        if status != 0 || stream.fd < 0 {
            return Err(UsbError::Protocol("CarKit connection has no valid socket"));
        }
        stream.set_io_timeout(Duration::from_millis(100))?;
        if ssl {
            // SAFETY: this is the newly opened service, never previously TLS.
            let status = unsafe { (stream.api.enable_ssl)(stream.connection) };
            if status != 0 {
                return Err(lockdown_error("CarKit TLS handshake", status));
            }
            stream.tls = true;
        }
        Ok(stream)
    }

    pub fn set_io_timeout(&self, timeout: Duration) -> io::Result<()> {
        if timeout.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nonzero timeout required",
            ));
        }
        let millis = timeout.as_millis().clamp(1, u32::MAX as u128) as u32;
        self.read_timeout_ms.store(millis, Ordering::Release);
        // Writes are bounded independently of the protocol's short read poll.
        let timeout = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        // SAFETY: fd is the connection's live socket; timeval layout and size
        // match the Linux SO_SNDTIMEO ABI, and the pointer lives through the call.
        let result = unsafe {
            libc::setsockopt(
                self.fd,
                libc::SOL_SOCKET,
                libc::SO_SNDTIMEO,
                ptr::from_ref(&timeout).cast(),
                std::mem::size_of_val(&timeout) as libc::socklen_t,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn close(&self) -> io::Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) && self.fd >= 0 {
            // SAFETY: shutdown does not release fd ownership and wakes any
            // pending native socket I/O; Drop performs the actual disconnect.
            let status = unsafe { libc::shutdown(self.fd, libc::SHUT_RDWR) };
            if status != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOTCONN) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

impl Read for CarKitStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.closed.load(Ordering::Acquire) {
            return Ok(0);
        }
        let mut received = 0;
        let length = receive_length(bytes.len(), self.tls);
        // SAFETY: live uniquely owned connection and writable output buffer;
        // requested length cannot exceed its capacity.
        let status = unsafe {
            (self.api.receive)(
                self.connection,
                bytes.as_mut_ptr().cast(),
                length,
                &mut received,
                self.read_timeout_ms.load(Ordering::Acquire),
            )
        };
        if received > length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid native receive count",
            ));
        }
        if received > 0 || status == 0 {
            return Ok(received as usize);
        }
        if status == -7 {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("CarKit receive failed ({status})"),
        ))
    }
}

fn receive_length(capacity: usize, tls: bool) -> u32 {
    // libimobiledevice 1.3.x's TLS receive_timeout tries to fill the requested
    // length and can discard a short TLS record when its next wait times out.
    // One byte preserves Read's partial-read contract without touching private
    // SSL state. iAP2 is low-bandwidth control; media uses the NCM sockets.
    capacity.min(if tls { 1 } else { u32::MAX as usize }) as u32
}

impl Write for CarKitStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut sent = 0;
        let length = bytes.len().min(16 * 1024) as u32;
        // SAFETY: uniquely owned live connection, initialized readable buffer.
        let status =
            unsafe { (self.api.send)(self.connection, bytes.as_ptr().cast(), length, &mut sent) };
        if sent > length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid native send count",
            ));
        }
        if sent > 0 || status == 0 {
            return Ok(sent as usize);
        }
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("CarKit send failed ({status})"),
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for CarKitStream {
    fn drop(&mut self) {
        let _ = self.close();
        // SAFETY: these handles were initialized through this still-loaded
        // library and each is released exactly once, in dependency order.
        unsafe {
            if !self.connection.is_null() {
                (self.api.disconnect)(self.connection);
            }
            if !self.lockdown.is_null() {
                (self.api.free_lockdown)(self.lockdown);
            }
            if !self.device.is_null() {
                (self.api.free_device)(self.device);
            }
        }
    }
}

pub struct PreparedSystem {
    pub stream: CarKitStream,
    /// Scoped IPv6 link-local socket; callers may change only its port.
    pub bind: SocketAddrV6,
    pub mac: [u8; 6],
    pub usb_interface: u8,
}

fn parse_mac(value: &str) -> Option<[u8; 6]> {
    let octets = value
        .trim()
        .split(':')
        .map(|s| u8::from_str_radix(s, 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let mac: [u8; 6] = octets.try_into().ok()?;
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

/// Locate only an NCM netdevice descended from the selected physical USB phone.
/// Wireless, tethering on another iPhone, and arbitrary new adapters never match.
fn ncm_interface(device_path: &Path, control: u8) -> io::Result<Option<(u32, [u8; 6])>> {
    let device_path = std::fs::canonicalize(device_path)?;
    for entry in std::fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        let path = entry.path();
        let Ok(parent) = std::fs::canonicalize(path.join("device")) else {
            continue;
        };
        if !parent.starts_with(&device_path) {
            continue;
        }
        let Ok(number) = std::fs::read_to_string(parent.join("bInterfaceNumber")) else {
            continue;
        };
        if u8::from_str_radix(number.trim(), 16).ok() != Some(control) {
            continue;
        }
        let Ok(driver) = std::fs::read_link(parent.join("driver")) else {
            continue;
        };
        if driver.file_name().and_then(|s| s.to_str()) != Some("cdc_ncm") {
            continue;
        }
        let Ok(index) = std::fs::read_to_string(path.join("ifindex")) else {
            continue;
        };
        let index = index
            .trim()
            .parse::<u32>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid interface index"))?;
        let Ok(address) = std::fs::read_to_string(path.join("address")) else {
            continue;
        };
        let mac = parse_mac(&address)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid NCM MAC address"))?;
        return Ok(Some((index, mac)));
    }
    Ok(None)
}

/// Bring up a real Linux wired service. Requires the system usbmuxd service,
/// libimobiledevice6, USB permissions, and an enabled cdc_ncm interface with an
/// IPv6 link-local address. It does not install software, replace drivers,
/// change routes, or create a Wi-Fi connection.
pub fn prepare_system(
    id: Option<&str>,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<PreparedSystem, UsbError> {
    if timeout.is_zero() {
        return Err(UsbError::InvalidTimeout);
    }
    check_cancel(cancel)?;
    let api = Api::load()?;
    let deadline = Instant::now() + timeout;
    let (info, device, configuration) = prepare_device(id, cancel, timeout)?;
    let serial = info
        .serial_number()
        .ok_or(UsbError::SystemBackend(
            "USB serial unavailable; cannot safely match the selected phone to usbmuxd",
        ))?
        .to_owned();
    let sysfs_path: PathBuf = info.sysfs_path().into();
    // usbmuxd owns the USBMUX bulk interface and cdc_ncm owns network data.
    // No nusb interface is claimed while those system drivers are using it.
    drop(device);
    let udid = loop {
        check_cancel(cancel)?;
        if let Some(udid) = api.matching_udid(&serial)? {
            break udid;
        }
        if Instant::now() >= deadline {
            return Err(UsbError::SystemBackend(
                "selected phone was not registered by usbmuxd",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Trigger the phone's trust prompt before waiting for its media network.
    // A missing IP address must not prevent the required pairing UI appearing.
    let stream = CarKitStream::open(api, &udid, cancel)?;
    let (bind, mac) = loop {
        check_cancel(cancel)?;
        let network = match ncm_interface(&sysfs_path, configuration.ncm_control_interface) {
            Ok(value) => value,
            // A sysfs device can disappear briefly while the host finishes
            // probing its new configuration. Keep cancellation/deadline active.
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some((index, mac)) = network
            && let Some(address) = network::diagnose().addresses.into_iter().find(|a| {
                a.interface_index == index
                    && a.is_up
                    && matches!(a.address, IpAddr::V6(v) if v.is_unicast_link_local())
            })
            && let IpAddr::V6(address) = address.address
        {
            break (SocketAddrV6::new(address, 7000, 0, index), mac);
        }
        if Instant::now() >= deadline {
            return Err(UsbError::SystemBackend(
                "selected phone needs usbmuxd and an enabled cdc_ncm interface with IPv6 link-local; check USB permissions and network configuration",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    Ok(PreparedSystem {
        stream,
        bind,
        mac,
        usb_interface: configuration.usbmux_interface,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_tls_reads_never_request_more_than_one_available_byte() {
        assert_eq!(receive_length(4096, true), 1);
        assert_eq!(receive_length(0, true), 0);
        assert_eq!(receive_length(4096, false), 4096);
    }

    #[test]
    fn usb_serial_matches_only_exact_udid_not_a_different_phone() {
        assert!(same_udid(
            "000081200006ABCDEF123456",
            "00008120-0006abcdef123456"
        ));
        assert!(!same_udid(
            "000081200006ABCDEF123456",
            "00008120-0006abcdef123457"
        ));
        assert!(!same_udid("", ""));
    }

    #[test]
    fn ncm_mac_rejects_bad_or_multicast_addresses() {
        assert_eq!(
            parse_mac("02:11:22:33:44:55\n"),
            Some([2, 0x11, 0x22, 0x33, 0x44, 0x55])
        );
        assert_eq!(parse_mac("01:11:22:33:44:55"), None);
        assert_eq!(parse_mac("00:00:00:00:00:00"), None);
        assert_eq!(parse_mac("02:11:22:33:44:zz"), None);
    }

    #[test]
    fn trust_pending_and_denied_are_actionable_separate_errors() {
        assert!(matches!(
            lockdown_error("pair", -19),
            UsbError::TrustRequired
        ));
        assert!(matches!(lockdown_error("pair", -18), UsbError::TrustDenied));
        assert!(matches!(
            lockdown_error("pair", -27),
            UsbError::Lockdown { code: -27, .. }
        ));
    }
}
