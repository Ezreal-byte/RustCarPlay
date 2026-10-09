//! Native host capabilities. Diagnostics never pair Bluetooth devices, install
//! drivers, claim USB interfaces, or change a USB configuration.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod bluetooth;
pub mod diagnostics;
pub mod hotspot;
pub mod network;
pub mod usb;
pub mod wifi;

pub use diagnostics::{Diagnostics, collect_diagnostics};

/// Blocking byte stream usable by protocol crates without platform types.
pub trait ReadWrite: std::io::Read + std::io::Write + Send {}
impl<T: std::io::Read + std::io::Write + Send> ReadWrite for T {}

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("invalid Bluetooth address; expected six hexadecimal octets separated by colons")]
    InvalidBluetoothAddress,
    #[error("invalid canonical 128-bit service UUID")]
    InvalidServiceUuid,
    #[error("RFCOMM channel must be between 1 and 30")]
    InvalidRfcommChannel,
    #[error("timeout must be nonzero")]
    InvalidTimeout,
    #[error("platform operation is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("BlueZ SDP service lookup failed: {0}")]
    ServiceDiscovery(&'static str),
    #[error("native operation failed: {0}")]
    Io(#[from] std::io::Error),
}
