// SPDX-License-Identifier: GPL-3.0-only
//! Port of DiPlay `airplay/AirPlayCrypto.kt` using RustCrypto primitives.
use crate::{AuthError, Result};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| AuthError::Crypto)?;
    Ok(bytes)
}

pub fn x25519_generate() -> Result<(Zeroizing<[u8; 32]>, [u8; 32])> {
    let secret = Zeroizing::new(random_bytes::<32>()?);
    let public = PublicKey::from(&StaticSecret::from(*secret)).to_bytes();
    Ok((secret, public))
}

pub fn x25519_shared(private: &[u8; 32], peer: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>> {
    let shared = StaticSecret::from(*private).diffie_hellman(&PublicKey::from(*peer));
    if !shared.was_contributory() {
        return Err(AuthError::Authentication);
    }
    Ok(Zeroizing::new(shared.to_bytes()))
}

pub fn ed25519_sign(seed: &[u8; 32], data: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed).sign(data).to_bytes()
}

pub fn ed25519_verify(public: &[u8; 32], data: &[u8], signature: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(signature) else {
        return false;
    };
    key.verify_strict(data, &signature).is_ok()
}

pub fn hkdf_sha512(
    ikm: &[u8],
    salt: &[u8],
    info: &[u8],
    length: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    if length > 255 * 64 {
        return Err(AuthError::InvalidInput("HKDF output length"));
    }
    let mut output = Zeroizing::new(vec![0; length]);
    Hkdf::<Sha512>::new(Some(salt), ikm)
        .expand(info, &mut output)
        .map_err(|_| AuthError::Crypto)?;
    Ok(output)
}

pub fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut output = Zeroizing::new([0; 32]);
    Hkdf::<Sha512>::new(Some(salt), ikm)
        .expand(info, output.as_mut())
        .map_err(|_| AuthError::Crypto)?;
    Ok(output)
}

pub fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut digest = Sha512::new();
    for part in parts {
        digest.update(part);
    }
    digest.finalize().into()
}

pub fn chacha_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    ChaCha20Poly1305::new(key.into())
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| AuthError::Crypto)
}

pub fn chacha_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    if ciphertext.len() < 16 {
        return Err(AuthError::Authentication);
    }
    ChaCha20Poly1305::new(key.into())
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| AuthError::Authentication)
}

pub fn nonce64(counter: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    nonce
}

/// The eight-byte ASCII label is zero-padded after four zero nonce bytes.
/// Overlength/non-ASCII labels are rejected instead of silently colliding.
pub fn nonce_label(label: &str) -> Result<[u8; 12]> {
    if !label.is_ascii() || label.len() > 8 {
        return Err(AuthError::InvalidInput("nonce label"));
    }
    let mut nonce = [0; 12];
    nonce[4..4 + label.len()].copy_from_slice(label.as_bytes());
    Ok(nonce)
}
