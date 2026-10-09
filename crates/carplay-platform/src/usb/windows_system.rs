// SPDX-License-Identifier: GPL-3.0-only
//! Windows wired CarPlay over Apple Mobile Device Service / libimobiledevice
//! and the native UsbNcm driver. Pairing remains in the system usbmux store.
//! The dynamic FFI signatures follow libimobiledevice's public headers.
use super::native::{UsbError, prepare_device};
#[path = "windows_network.rs"]
mod windows_network;
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV6, TcpStream};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinSock::{
    SD_BOTH, SO_SNDTIMEO, SOL_SOCKET, WSAENOTCONN, WSAGetLastError, setsockopt, shutdown,
};

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
        validate_usbmux_address(std::env::var_os("USBMUXD_SOCKET_ADDRESS").as_deref())?;
        let path = runtime_library_path()?;
        // Only the explicitly selected runtime directory and System32 may
        // resolve dependencies. Neither PATH nor the working directory is used.
        // SAFETY: the absolute library path names the supported C ABI, and all
        // function pointers remain bounded by this owned library's lifetime.
        unsafe {
            let library: Library = libloading::os::windows::Library::load_with_flags(
                &path,
                libloading::os::windows::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                    | libloading::os::windows::LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
            .map_err(|_| UsbError::SystemBackend(
                "could not load the Windows libimobiledevice runtime or its dependencies; run scripts/setup-usb-runtime.ps1",
            ))?.into();
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

    fn usb_udids(&self) -> Result<Vec<CString>, UsbError> {
        let mut list = ptr::null_mut();
        let mut count = 0;
        // SAFETY: initialized writable output slots. The returned list is
        // library-owned until free_list; each returned UDID is NUL-terminated.
        let status = unsafe { (self.list_devices)(&mut list, &mut count) };
        if status != 0 {
            return Err(UsbError::SystemBackend(
                "Apple Mobile Device Service is not available on localhost:27015",
            ));
        }
        let mut devices = Vec::new();
        if !list.is_null() && (0..=256).contains(&count) {
            for index in 0..count as usize {
                // SAFETY: index is bounded by the count returned with this list.
                let entry = unsafe { *list.add(index) };
                if !entry.is_null() {
                    // SAFETY: each non-null entry is a library-owned C string.
                    let value = unsafe { CStr::from_ptr(entry) };
                    devices.push(value.to_owned());
                }
            }
        }
        if !list.is_null() {
            // SAFETY: list was obtained from this same library and freed once.
            unsafe { (self.free_list)(list) };
        }
        if !(0..=256).contains(&count) {
            return Err(UsbError::Protocol("invalid USBMUX device count"));
        }
        Ok(devices)
    }

    fn matching_udid(&self, serial: &str) -> Result<Option<CString>, UsbError> {
        Ok(self
            .usb_udids()?
            .into_iter()
            .find(|s| s.to_str().is_ok_and(|s| same_udid(serial, s))))
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
    (16..=64).contains(&serial.len())
        && serial.iter().all(u8::is_ascii_hexdigit)
        && serial == canonical(udid)
}

fn valid_usb_serial(value: &str) -> bool {
    let length = value.bytes().filter(|b| *b != b'-').count();
    (16..=64).contains(&length) && value.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
}

fn physical_pnp_serial(instance: &str, product: u16) -> Option<&str> {
    let mut parts = instance.split('\\');
    if !parts.next()?.eq_ignore_ascii_case("USB")
        || !parts
            .next()?
            .eq_ignore_ascii_case(&format!("VID_05AC&PID_{product:04X}"))
    {
        return None;
    }
    let serial = parts.next()?;
    // A generated Windows location ID contains '&' and is not a serial. Child
    // MI IDs and other devices are also excluded by the exact parent prefix.
    (parts.next().is_none() && valid_usb_serial(serial)).then_some(serial)
}

fn selected_usb_identity<'a>(
    instance: Option<&'a str>,
    product: u16,
    descriptor_serial: Option<&'a str>,
) -> Result<(&'a str, &'static str), UsbError> {
    let pnp = instance.and_then(|s| physical_pnp_serial(s, product));
    let descriptor = descriptor_serial.filter(|s| valid_usb_serial(s));
    match (pnp, descriptor) {
        (Some(pnp), Some(descriptor)) if !same_udid(pnp, descriptor) => {
            Err(UsbError::DeviceChanged)
        }
        // Windows' hub string descriptor can contain fixed-width NUL padding
        // after the USB mode changes (observed: 24 hex chars plus 16 NULs).
        // The exact PnP parent instance serial remains stable across
        // re-enumeration; match it against USB-only AMDS identities, never names.
        (Some(pnp), _) => Ok((pnp, "physical_pnp_instance")),
        (None, Some(descriptor)) => Ok((descriptor, "usb_serial_descriptor")),
        (None, None) => Err(UsbError::SystemBackend(
            "no valid physical USB serial is available for matching Apple Mobile Device Service",
        )),
    }
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
        if status != 0 || stream.fd == -1 {
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
        // Writes have a separate two-second bound. Winsock expects a DWORD
        // millisecond timeout, unlike Linux's timeval.
        let send_millis = 2000_u32;
        // SAFETY: libimobiledevice exposes its Windows socket using the public
        // int fd ABI. Preserve that bit pattern when widening to SOCKET. The
        // library retains ownership; only options and shutdown are performed.
        let status = unsafe {
            setsockopt(
                self.fd as u32 as usize,
                SOL_SOCKET,
                SO_SNDTIMEO,
                ptr::from_ref(&send_millis).cast(),
                std::mem::size_of_val(&send_millis) as i32,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            // SAFETY: reads this thread's most recent Winsock error.
            Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
        }
    }

    pub fn close(&self) -> io::Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) && self.fd != -1 {
            // SAFETY: shutdown wakes native I/O without releasing socket
            // ownership. Drop later disconnects through libimobiledevice.
            let status = unsafe { shutdown(self.fd as u32 as usize, SD_BOTH) };
            if status != 0 {
                // SAFETY: reads this thread's most recent Winsock error.
                let code = unsafe { WSAGetLastError() };
                if code != WSAENOTCONN {
                    return Err(io::Error::from_raw_os_error(code));
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
        // libimobiledevice 1.3.0's TLS receive_timeout loops until the requested
        // length is filled, and may discard a partial count when it times out.
        // Request a single byte for TLS so a short iAP2 frame is never consumed
        // then lost. iAP2 control traffic is small; media uses NCM directly.
        // Plain USBMUX uses normal short-read semantics and keeps bulk reads.
        let length = if self.tls {
            1
        } else {
            bytes.len().min(u32::MAX as usize) as u32
        };
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

/// Check only the local library ABI; this does not pair or touch any iPhone.
pub fn runtime_available() -> Result<(), UsbError> {
    Api::load().map(|_| ())
}

/// Read-only evidence for diagnosing system USBMUX registration. Hardware
/// identities never leave this function: only counts, lengths and booleans.
#[derive(Debug, serde::Serialize)]
pub struct UsbMuxStatus {
    pub system_usb_phone_count: usize,
    pub selected_usb_serial_present: bool,
    pub selected_usb_serial_length: usize,
    pub selected_usb_serial_is_hex: bool,
    pub selected_usb_registered: bool,
    pub system_udid_lengths: Vec<usize>,
    pub descriptor_serial_equals_product_name: bool,
    pub pnp_serial_length: usize,
    pub pnp_serial_is_hex: bool,
    pub pnp_serial_registered: bool,
    pub resolved_identity_source: Option<&'static str>,
    pub descriptor_nul_count: usize,
    pub descriptor_whitespace_count: usize,
    pub descriptor_non_hex_count: usize,
    pub descriptor_contains_pnp_serial: bool,
}

pub fn usbmux_status(id: Option<&str>) -> Result<UsbMuxStatus, UsbError> {
    let api = Api::load()?;
    let selected = super::native::select(id)?;
    let serial = selected.serial_number();
    let pnp_serial = selected
        .instance_id()
        .to_str()
        .and_then(|s| s.rsplit('\\').next());
    let devices = api.usb_udids()?;
    let resolved = selected_usb_identity(
        selected.instance_id().to_str(),
        selected.product_id(),
        serial,
    )
    .ok();
    Ok(UsbMuxStatus {
        system_usb_phone_count: devices.len(),
        selected_usb_serial_present: serial.is_some(),
        selected_usb_serial_length: serial.map_or(0, str::len),
        selected_usb_serial_is_hex: serial.is_some_and(|s| {
            !s.is_empty() && s.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
        }),
        selected_usb_registered: resolved.is_some_and(|(serial, _)| {
            devices
                .iter()
                .any(|s| s.to_str().is_ok_and(|s| same_udid(serial, s)))
        }),
        system_udid_lengths: devices.iter().map(|s| s.as_bytes().len()).collect(),
        descriptor_serial_equals_product_name: serial.is_some()
            && serial == selected.product_string(),
        pnp_serial_length: pnp_serial.map_or(0, str::len),
        pnp_serial_is_hex: pnp_serial.is_some_and(|s| {
            !s.is_empty() && s.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
        }),
        pnp_serial_registered: pnp_serial.is_some_and(|serial| {
            devices
                .iter()
                .any(|s| s.to_str().is_ok_and(|s| same_udid(serial, s)))
        }),
        resolved_identity_source: resolved.map(|(_, source)| source),
        descriptor_nul_count: serial.map_or(0, |s| s.bytes().filter(|b| *b == 0).count()),
        descriptor_whitespace_count: serial
            .map_or(0, |s| s.bytes().filter(u8::is_ascii_whitespace).count()),
        descriptor_non_hex_count: serial.map_or(0, |s| {
            s.bytes()
                .filter(|b| *b != b'-' && !b.is_ascii_hexdigit())
                .count()
        }),
        descriptor_contains_pnp_serial: serial
            .zip(pnp_serial)
            .is_some_and(|(s, p)| s.to_ascii_lowercase().contains(&p.to_ascii_lowercase())),
    })
}

fn validate_usbmux_address(address: Option<&std::ffi::OsStr>) -> Result<(), UsbError> {
    if address.is_none_or(|s| matches!(s.to_str(), Some("127.0.0.1:27015" | "localhost:27015"))) {
        Ok(())
    } else {
        Err(UsbError::SystemBackend(
            "USBMUXD_SOCKET_ADDRESS must be unset or point to localhost:27015; remote USBMUX servers are not accepted for physical USB mode",
        ))
    }
}

fn runtime_directory(explicit: Option<PathBuf>, executable: &Path) -> Result<PathBuf, UsbError> {
    if let Some(path) = explicit {
        if path.is_absolute() {
            return Ok(path);
        }
        return Err(UsbError::SystemBackend(
            "RUSTCARPLAY_LIBIMOBILEDEVICE_DIR must be an absolute runtime directory",
        ));
    }
    executable
        .parent()
        .filter(|p| p.is_absolute())
        .map(|p| p.join("runtime").join("libimobiledevice"))
        .ok_or(UsbError::SystemBackend(
            "cannot locate the executable runtime directory",
        ))
}

fn runtime_library_path() -> Result<PathBuf, UsbError> {
    let executable = std::env::current_exe()?;
    let directory = runtime_directory(
        std::env::var_os("RUSTCARPLAY_LIBIMOBILEDEVICE_DIR").map(PathBuf::from),
        &executable,
    )?;
    // Canonicalizing also rules out drive-relative paths and makes the loader
    // independent of later current-directory changes in any UI thread.
    let directory = directory.canonicalize().map_err(|_| UsbError::SystemBackend(
        "Windows libimobiledevice runtime is missing; run scripts/setup-usb-runtime.ps1 and its launcher",
    ))?;
    [
        "libimobiledevice-1.0-6.dll",
        "libimobiledevice-1.0.dll",
        "libimobiledevice.dll",
    ]
    .into_iter()
    .map(|name| directory.join(name))
    .find(|path| path.is_file())
    .ok_or(UsbError::SystemBackend(
        "Windows libimobiledevice DLL is missing from the configured runtime directory",
    ))
}

/// Bring up the selected physical Windows USB phone. Apple's Mobile Device
/// Service owns USBMUX and UsbNcm owns media networking; neither is replaced.
/// This method may trigger the iPhone's normal computer trust prompt.
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
    // This bounded, loopback-only probe yields an actionable error before USB
    // mode changes if the system usbmux service is absent.
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, 27015)),
        Duration::from_millis(500),
    ).map_err(|_| UsbError::SystemBackend(
        "Apple Mobile Device Service is not listening on localhost:27015; install/open Apple Devices or iTunes with Apple Mobile Device Support",
    ))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(UsbError::InvalidTimeout)?;
    let (info, device, configuration) = prepare_device(id, cancel, timeout)?;
    let (serial, _) = selected_usb_identity(
        info.instance_id().to_str(),
        info.product_id(),
        info.serial_number(),
    )?;
    let serial = serial.to_owned();
    let instance_id = info.instance_id().to_owned();
    // Apple Mobile Device Service owns USBMUX and UsbNcm owns network data.
    // No nusb interface is claimed while those system drivers are using it.
    drop(device);
    let udid = loop {
        check_cancel(cancel)?;
        if let Some(udid) = api.matching_udid(&serial)? {
            break udid;
        }
        if Instant::now() >= deadline {
            return Err(UsbError::SystemBackend(
                "selected physical iPhone was not registered by Apple Mobile Device Service",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Trigger the phone's trust prompt before waiting for its media network.
    // A missing IP address must not prevent the required pairing UI appearing.
    let stream = CarKitStream::open(api, &udid, cancel)?;
    let (bind, mac) = loop {
        check_cancel(cancel)?;
        if let Some(endpoint) =
            windows_network::find_ncm_endpoint(&instance_id, configuration.ncm_control_interface)?
        {
            break (endpoint.bind, endpoint.mac);
        }
        if Instant::now() >= deadline {
            return Err(UsbError::SystemBackend(
                "selected phone needs its CDC NCM interface bound to Windows UsbNcm and enabled with IPv6 link-local; check USB driver setup",
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
    fn nul_padded_hub_string_cannot_hide_the_exact_physical_pnp_phone() {
        let instance = r"USB\VID_05AC&PID_12A8\000081200006ABCDEF123456";
        let padded = format!("{}{}", "000081200006ABCDEF123456", "\0".repeat(16));
        let (identity, source) =
            selected_usb_identity(Some(instance), 0x12a8, Some(&padded)).unwrap();
        assert!(same_udid(identity, "00008120-0006abcdef123456"));
        assert_eq!(source, "physical_pnp_instance");
        assert!(matches!(
            selected_usb_identity(Some(instance), 0x12a8, Some("000081200006ABCDEF123457")),
            Err(UsbError::DeviceChanged)
        ));
    }

    #[test]
    fn pnp_identity_rejects_location_ids_child_interfaces_and_other_devices() {
        for instance in [
            r"USB\VID_05AC&PID_12A8\7&1234567&0&1",
            r"USB\VID_05AC&PID_12A8&MI_01\000081200006ABCDEF123456",
            r"USB\VID_05AC&PID_12A9\000081200006ABCDEF123456",
            r"USB\VID_05AD&PID_12A8\000081200006ABCDEF123456",
            r"USB\VID_05AC&PID_12A8\000081200006ABCDEF123456\extra",
        ] {
            assert!(physical_pnp_serial(instance, 0x12a8).is_none());
        }
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
        assert!(!same_udid("not-a-device-serial", "not-a-device-serial"));
    }

    #[test]
    fn dll_search_never_uses_path_or_relative_override() {
        let exe = Path::new(r"C:\RustCarPlay\carplay-desktop.exe");
        assert_eq!(
            runtime_directory(None, exe).unwrap(),
            Path::new(r"C:\RustCarPlay\runtime\libimobiledevice")
        );
        assert!(runtime_directory(Some(PathBuf::from("runtime")), exe).is_err());
        assert!(runtime_directory(Some(PathBuf::from(r"C:runtime")), exe).is_err());
        assert_eq!(
            runtime_directory(Some(PathBuf::from(r"D:\trusted")), exe).unwrap(),
            Path::new(r"D:\trusted")
        );
    }

    #[test]
    fn physical_usb_cannot_be_redirected_to_remote_usbmux() {
        assert!(validate_usbmux_address(None).is_ok());
        assert!(validate_usbmux_address(Some(std::ffi::OsStr::new("127.0.0.1:27015"))).is_ok());
        assert!(validate_usbmux_address(Some(std::ffi::OsStr::new("192.168.1.2:27015"))).is_err());
        assert!(validate_usbmux_address(Some(std::ffi::OsStr::new("localhost:12345"))).is_err());
    }

    #[test]
    fn cancelled_or_zero_timeout_precedes_library_or_hardware_access() {
        assert!(matches!(
            prepare_system(None, &AtomicBool::new(true), Duration::from_secs(1)),
            Err(UsbError::Cancelled)
        ));
        assert!(matches!(
            prepare_system(None, &AtomicBool::new(false), Duration::ZERO),
            Err(UsbError::InvalidTimeout)
        ));
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
