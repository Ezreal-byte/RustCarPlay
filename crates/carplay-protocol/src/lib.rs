// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9, shared/src/main/java/com/shilapi/xcertplay/{iap2,transport}.
//! Wire protocols and a deterministic iAP2 link engine. No hardware or credentials are accessed.
pub mod file_transfer;
pub mod iap2;
pub mod metadata;
pub mod ncm;
pub mod tlv;
pub mod usbmux;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("incomplete frame")]
    Incomplete,
    #[error("invalid wire data: {0}")]
    Invalid(&'static str),
    #[error("checksum mismatch")]
    Checksum,
    #[error("length or buffering limit exceeded")]
    Limit,
    #[error("link has ended")]
    Closed,
}
pub type Result<T> = std::result::Result<T, Error>;
