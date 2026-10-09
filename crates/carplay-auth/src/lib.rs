// SPDX-License-Identifier: GPL-3.0-only
//! Accessory-side authentication, pairing and encrypted control records.
//!
//! Ported from DiPlay's `shared/src/main/java/com/shilapi/xcertplay/{airplay,mfi}`.
//! A local identity consistency check never establishes Apple/iPhone trust.
//! Secrets are supplied by the caller; no MFi private identity is distributed.

pub mod crypto;
pub mod mfi;
pub mod pairing;
pub mod record;
pub mod srp;
pub mod tlv;

pub use mfi::{
    AuthProvider, BaaCertificates, CertificateType, IdentitySelfCheck, LocalIdentity,
    mfi_sap_auth_setup,
};
pub use pairing::{
    AirPlayIdentity, ControlKeys, MemoryPairingStore, PairSetup, PairVerify, PairingStore,
};
pub use record::{ControlReader, ControlWriter};

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid authentication input: {0}")]
    InvalidInput(&'static str),
    #[error("authentication failed")]
    Authentication,
    #[error("cryptographic operation failed")]
    Crypto,
    #[error("invalid or mismatched external identity: {0}")]
    Identity(&'static str),
    #[error("unsupported authentication operation: {0}")]
    Unsupported(&'static str),
    #[error("pairing store operation failed: {0}")]
    Store(String),
    #[error("encrypted record counter exhausted")]
    CounterExhausted,
    #[error("authentication I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AuthError>;
