//! Classic Bluetooth RFCOMM sockets for already paired peers.
//!
//! Listening opens a socket only: it does not publish an SDP service, pair a
//! device, or make the adapter discoverable. The embedding application must
//! arrange service registration separately before advertising readiness.

use crate::PlatformError;
use socket2::Socket;
use std::{
    fmt,
    io::{self, Read, Write},
    str::FromStr,
    sync::atomic::AtomicBool,
    time::Duration,
};

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod connecting;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as native;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as native;

/// Address octets in human/network order. Deliberately not serializable, and
/// Debug redacts the address so error logs do not expose a peer identifier.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BluetoothAddress([u8; 6]);

impl BluetoothAddress {
    pub const ANY: Self = Self([0; 6]);
    pub const fn octets(self) -> [u8; 6] {
        self.0
    }
    /// Explicitly reveal an address for a local device picker. Diagnostic and
    /// Debug exports remain redacted; do not include this string in telemetry.
    pub fn to_colon_string(self) -> String {
        let b = self.0;
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn as_integer(self) -> u64 {
        self.0
            .into_iter()
            .fold(0, |value, byte| (value << 8) | u64::from(byte))
    }
}

impl fmt::Debug for BluetoothAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BluetoothAddress([redacted])")
    }
}

impl FromStr for BluetoothAddress {
    type Err = PlatformError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if bytes.len() != 17 {
            return Err(PlatformError::InvalidBluetoothAddress);
        }
        let mut result = [0; 6];
        for (i, out) in result.iter_mut().enumerate() {
            let part = &bytes[i * 3..i * 3 + 2];
            if !part.iter().all(u8::is_ascii_hexdigit) || (i < 5 && bytes[i * 3 + 2] != b':') {
                return Err(PlatformError::InvalidBluetoothAddress);
            }
            *out = u8::from_str_radix(&value[i * 3..i * 3 + 2], 16)
                .map_err(|_| PlatformError::InvalidBluetoothAddress)?;
        }
        Ok(Self(result))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceUuid(uuid::Uuid);
impl ServiceUuid {
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}
impl FromStr for ServiceUuid {
    type Err = PlatformError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 36
            || [8, 13, 18, 23]
                .into_iter()
                .any(|i| value.as_bytes()[i] != b'-')
        {
            return Err(PlatformError::InvalidServiceUuid);
        }
        uuid::Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| PlatformError::InvalidServiceUuid)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConnectOptions {
    pub peer: BluetoothAddress,
    pub service: ServiceUuid,
    /// Bypass SDP when the channel is already known. Never guessed.
    pub channel: Option<u8>,
    pub timeout: Duration,
}

/// Local picker data, intentionally not serializable. Names and addresses must
/// not be copied into the anonymized host diagnostics or application logs.
#[derive(Clone)]
pub struct PairedDevice {
    pub name: String,
    pub address: BluetoothAddress,
    pub authenticated: bool,
    pub remembered: bool,
    pub connected: bool,
}
impl fmt::Debug for PairedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairedDevice")
            .field("name", &"[redacted]")
            .field("address", &self.address)
            .field("authenticated", &self.authenticated)
            .field("remembered", &self.remembered)
            .field("connected", &self.connected)
            .finish()
    }
}

#[derive(Clone)]
pub struct LocalAdapter {
    pub name: String,
    pub address: BluetoothAddress,
}
impl fmt::Debug for LocalAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalAdapter")
            .field("name", &"[redacted]")
            .field("address", &self.address)
            .finish()
    }
}

/// Read Windows' remembered/authenticated Classic Bluetooth cache. Does not
/// issue an inquiry, initiate pairing, or change adapter configuration.
pub fn paired_devices() -> Result<Vec<PairedDevice>, PlatformError> {
    #[cfg(target_os = "windows")]
    {
        windows::paired_devices()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(PlatformError::Unsupported(
            "paired-device enumeration is currently implemented on Windows only",
        ))
    }
}

/// Read adapter identity for the local picker. This is separate from the
/// anonymized diagnostics API, which intentionally exposes only a radio count.
pub fn local_adapters() -> Result<Vec<LocalAdapter>, PlatformError> {
    #[cfg(target_os = "windows")]
    {
        windows::local_adapters()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(PlatformError::Unsupported(
            "adapter identity enumeration is currently implemented on Windows only",
        ))
    }
}

pub struct RfcommStream {
    socket: Socket,
}

impl RfcommStream {
    /// Establishes an RFCOMM socket. Linux UUID discovery uses libbluetooth's
    /// synchronous SDP API; `timeout` bounds socket connection, not SDP lookup.
    pub fn connect(options: ConnectOptions) -> Result<Self, PlatformError> {
        Self::connect_with_cancel(options, &AtomicBool::new(false))
    }

    /// Cancellation closes the pending socket before returning. Linux's BlueZ
    /// SDP lookup remains synchronous; cancellation is checked again afterwards.
    pub fn connect_with_cancel(
        options: ConnectOptions,
        cancelled: &AtomicBool,
    ) -> Result<Self, PlatformError> {
        validate_channel(options.channel)?;
        if options.timeout.is_zero() {
            return Err(PlatformError::InvalidTimeout);
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            native::connect(options, cancelled).map(|socket| Self { socket })
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            let _ = cancelled;
            Err(PlatformError::Unsupported("native RFCOMM backend"))
        }
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_write_timeout(timeout)
    }
    pub fn shutdown(&self) -> io::Result<()> {
        self.socket.shutdown(std::net::Shutdown::Both)
    }
}

impl Drop for RfcommStream {
    fn drop(&mut self) {
        // Bluetooth has no TCP-style half-close: explicitly disconnect before
        // releasing the handle, including callers that never entered iAP2 run().
        let _ = self.shutdown();
    }
}

impl Read for RfcommStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.socket).read(buf)
    }
}
impl Write for RfcommStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.socket).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct RfcommListener {
    socket: Socket,
    channel: u8,
}
impl RfcommListener {
    /// Bind a local radio and explicit channel. This does not register SDP.
    pub fn bind(local: BluetoothAddress, channel: u8) -> Result<Self, PlatformError> {
        validate_channel(Some(channel))?;
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            native::listen(local, channel).map(|socket| Self { socket, channel })
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            let _ = local;
            Err(PlatformError::Unsupported("native RFCOMM backend"))
        }
    }
    pub fn channel(&self) -> u8 {
        self.channel
    }
    pub fn set_nonblocking(&self, value: bool) -> io::Result<()> {
        self.socket.set_nonblocking(value)
    }
    pub fn accept(&self) -> io::Result<RfcommStream> {
        self.socket
            .accept()
            .map(|(socket, _)| RfcommStream { socket })
    }
}

pub(crate) fn validate_channel(channel: Option<u8>) -> Result<(), PlatformError> {
    if channel.is_some_and(|channel| !(1..=30).contains(&channel)) {
        Err(PlatformError::InvalidRfcommChannel)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BluetoothDiagnostic {
    pub rfcomm_socket_supported: bool,
    pub local_radio_count: Option<usize>,
    pub uuid_lookup_backend: &'static str,
    pub sdp_advertising_implemented: bool,
    pub issue: Option<crate::diagnostics::DiagnosticIssue>,
}

pub fn diagnose() -> BluetoothDiagnostic {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        native::diagnose()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        BluetoothDiagnostic {
            rfcomm_socket_supported: false,
            local_radio_count: None,
            uuid_lookup_backend: "unavailable",
            sdp_advertising_implemented: false,
            issue: Some(crate::diagnostics::DiagnosticIssue::unsupported(
                "rfcomm_backend",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_endian_and_redaction() {
        let address: BluetoothAddress = "01:23:45:67:89:aB".parse().unwrap();
        assert_eq!(address.octets(), [1, 35, 69, 103, 137, 171]);
        assert_eq!(address.as_integer(), 0x0123_4567_89ab);
        assert!(!format!("{address:?}").contains("89"));
        assert_eq!(address.to_colon_string(), "01:23:45:67:89:AB");
        let device = PairedDevice {
            name: "Private phone name".into(),
            address,
            authenticated: true,
            remembered: true,
            connected: false,
        };
        let adapter = LocalAdapter {
            name: "Private computer name".into(),
            address,
        };
        for debug in [format!("{device:?}"), format!("{adapter:?}")] {
            assert!(!debug.contains("Private"));
            assert!(!debug.contains("01:23"));
        }
    }
    #[test]
    fn rejects_bad_addresses_without_unicode_panics() {
        for value in [
            "",
            "01-23-45-67-89-AB",
            "01:23:45:67:89:ZZ",
            "01:23:45:67:89:AB ",
            "ééééééééx",
        ] {
            assert!(value.parse::<BluetoothAddress>().is_err(), "{value}");
        }
    }
    #[test]
    fn validates_uuid_channel_and_timeout_before_io() {
        assert!(
            "00001101-0000-1000-8000-00805f9b34fb"
                .parse::<ServiceUuid>()
                .is_ok()
        );
        assert!(
            "0000110100001000800000805f9b34fb"
                .parse::<ServiceUuid>()
                .is_err()
        );
        assert!(validate_channel(Some(0)).is_err());
        assert!(validate_channel(Some(31)).is_err());
        assert!(validate_channel(Some(30)).is_ok());
    }
}
