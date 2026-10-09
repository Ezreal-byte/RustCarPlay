// SPDX-License-Identifier: GPL-3.0-only
//! Port of DiPlay `mfi/LocalMfiAuthenticationClient.kt` and
//! `airplay/MfiSapAuthSetup.kt`. No certificate-chain/iPhone trust is implied.
use crate::{AuthError, Result, crypto};
use cms::{cert::CertificateChoices, content_info::ContentInfo, signed_data::SignedData};
use der::{Decode, Encode};
use p256::{
    ecdsa::{
        Signature, SigningKey, VerifyingKey,
        signature::hazmat::{PrehashSigner, PrehashVerifier},
    },
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};
use zeroize::Zeroizing;

const MAX_IDENTITY_BYTES: usize = 16 * 1024;
const MAX_CERTIFICATE_BYTES: usize = 65_525;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertificateType {
    Mfi,
    Baa,
}

pub struct BaaCertificates {
    pub leaf: Vec<u8>,
    pub intermediate: Vec<u8>,
}

/// Blocking provider contract; hardware/network implementations belong on an I/O
/// worker. MFi v3 receives a 32-byte SHA-256 digest, v2 receives a SHA-1 digest;
/// BAA receives the concatenated public keys. Never re-hash v3's input digest.
pub trait AuthProvider: Send + Sync {
    fn protocol_major(&self) -> u8;
    fn certificate_type(&self) -> CertificateType {
        CertificateType::Mfi
    }
    fn certificate(&self) -> Result<Vec<u8>>;
    fn sign_challenge(&self, challenge: &[u8]) -> Result<Vec<u8>>;
    fn baa_certificates(&self) -> Result<BaaCertificates> {
        Err(AuthError::Unsupported("BAA certificates"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentitySelfCheck {
    pub algorithm: &'static str,
    pub certificate_sha256: [u8; 32],
    pub key_matches_certificate: bool,
    /// Always false: local consistency cannot prove phone acceptance.
    pub iphone_trust_verified: bool,
}

/// External P-256 PKCS#8 private key and one DER X.509/CMS PKCS#7 certificate.
/// The private key is intentionally neither printable nor serializable here.
pub struct LocalIdentity {
    key: SigningKey,
    certificate: Vec<u8>,
    self_check: IdentitySelfCheck,
}

impl LocalIdentity {
    pub fn load(directory: impl AsRef<Path>) -> Result<Self> {
        let key = Zeroizing::new(read_bounded(&directory.as_ref().join("identity.pk8"))?);
        let certificate = read_bounded(&directory.as_ref().join("certificate.p7b"))?;
        Self::from_der(&key, &certificate)
    }

    pub fn from_der(private_pkcs8: &[u8], certificate_der_or_pkcs7: &[u8]) -> Result<Self> {
        for input in [private_pkcs8, certificate_der_or_pkcs7] {
            if input.is_empty() || input.len() > MAX_IDENTITY_BYTES {
                return Err(AuthError::Identity("empty or oversized identity file"));
            }
        }
        let key = SigningKey::from_pkcs8_der(private_pkcs8)
            .map_err(|_| AuthError::Identity("expected P-256 PKCS#8 private key"))?;
        let certificate = single_certificate(certificate_der_or_pkcs7)?;
        let spki = certificate
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .map_err(|_| AuthError::Identity("malformed certificate public key"))?;
        let public = VerifyingKey::from_public_key_der(&spki)
            .map_err(|_| AuthError::Identity("expected P-256 certificate public key"))?;
        let challenge = crypto::random_bytes::<32>()?;
        let signature: Signature = key
            .sign_prehash(&challenge)
            .map_err(|_| AuthError::Crypto)?;
        public
            .verify_prehash(&challenge, &signature)
            .map_err(|_| AuthError::Identity("private key does not match certificate"))?;
        Ok(Self {
            key,
            certificate: certificate_der_or_pkcs7.to_vec(),
            self_check: IdentitySelfCheck {
                algorithm: "P-256 ECDSA prehashed SHA-256 / MFi v3",
                certificate_sha256: Sha256::digest(certificate_der_or_pkcs7).into(),
                key_matches_certificate: true,
                iphone_trust_verified: false,
            },
        })
    }

    pub fn self_check(&self) -> &IdentitySelfCheck {
        &self.self_check
    }

    pub fn certificate_bounded(&self, maximum: usize) -> Result<Vec<u8>> {
        if maximum == 0 || self.certificate.len() > maximum {
            return Err(AuthError::InvalidInput("certificate output limit"));
        }
        Ok(self.certificate.clone())
    }
}

impl AuthProvider for LocalIdentity {
    fn protocol_major(&self) -> u8 {
        3
    }
    fn certificate(&self) -> Result<Vec<u8>> {
        self.certificate_bounded(MAX_CERTIFICATE_BYTES)
    }
    fn sign_challenge(&self, challenge: &[u8]) -> Result<Vec<u8>> {
        if challenge.len() != 32 {
            return Err(AuthError::InvalidInput("MFi v3 requires a 32-byte digest"));
        }
        let signature: Signature = self
            .key
            .sign_prehash(challenge)
            .map_err(|_| AuthError::Crypto)?;
        Ok(signature.to_bytes().to_vec()) // fixed-width, big-endian r || s, not ASN.1 DER
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take((MAX_IDENTITY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_IDENTITY_BYTES {
        return Err(AuthError::Identity("empty or oversized identity file"));
    }
    Ok(bytes)
}

fn single_certificate(input: &[u8]) -> Result<x509_cert::Certificate> {
    if let Ok(cert) = x509_cert::Certificate::from_der(input) {
        return Ok(cert);
    }
    let content =
        ContentInfo::from_der(input).map_err(|_| AuthError::Identity("malformed X.509/PKCS#7"))?;
    if content.content_type.to_string() != "1.2.840.113549.1.7.2" {
        return Err(AuthError::Identity("PKCS#7 content is not SignedData"));
    }
    let signed: SignedData = content
        .content
        .decode_as()
        .map_err(|_| AuthError::Identity("malformed PKCS#7 SignedData"))?;
    let certificates = signed
        .certificates
        .ok_or(AuthError::Identity("certificate missing"))?;
    let mut entries = certificates.0.iter();
    let cert = match entries.next() {
        Some(CertificateChoices::Certificate(cert)) => cert.clone(),
        _ => {
            return Err(AuthError::Identity(
                "expected one X.509 accessory certificate",
            ));
        }
    };
    if entries.next().is_some() {
        return Err(AuthError::Identity(
            "expected exactly one accessory certificate",
        ));
    }
    Ok(cert)
}

/// MFiSAP binary response: accessory X25519 key followed by big-endian length
/// prefixed certificate and AES-128-CTR signature. The request is version 1 +
/// exactly 32 peer-key bytes. This endpoint must follow encrypted pair-verify.
pub fn mfi_sap_auth_setup(body: &[u8], provider: &dyn AuthProvider) -> Result<Vec<u8>> {
    if body.len() != 33 || body[0] != 1 {
        return Err(AuthError::InvalidInput("MFiSAP request version/length"));
    }
    let peer: [u8; 32] = body[1..]
        .try_into()
        .map_err(|_| AuthError::InvalidInput("peer key length"))?;
    let (private, public) = crypto::x25519_generate()?;
    let shared = crypto::x25519_shared(&private, &peer)?;
    let mut key_hash = sha1::Sha1::new();
    key_hash.update(b"AES-KEY");
    key_hash.update(shared.as_ref());
    let key = Zeroizing::new(key_hash.finalize());
    let mut iv_hash = sha1::Sha1::new();
    iv_hash.update(b"AES-IV");
    iv_hash.update(shared.as_ref());
    let iv = Zeroizing::new(iv_hash.finalize());
    let signed = [public.as_slice(), peer.as_slice()].concat();
    let (certificate, challenge, intermediate) = match provider.certificate_type() {
        CertificateType::Mfi => {
            let challenge = match provider.protocol_major() {
                2 => sha1::Sha1::digest(&signed).to_vec(),
                3 => Sha256::digest(&signed).to_vec(),
                _ => return Err(AuthError::Unsupported("MFi protocol major")),
            };
            (provider.certificate()?, challenge, None)
        }
        CertificateType::Baa => {
            let certs = provider.baa_certificates()?;
            if certs.intermediate.is_empty() || certs.intermediate.len() > MAX_CERTIFICATE_BYTES {
                return Err(AuthError::InvalidInput("BAA intermediate length"));
            }
            (
                certs.leaf,
                signed,
                Some(encode_baa_intermediate(&certs.intermediate)),
            )
        }
    };
    if certificate.is_empty() || certificate.len() > MAX_CERTIFICATE_BYTES {
        return Err(AuthError::InvalidInput("MFi certificate length"));
    }
    let mut signature = provider.sign_challenge(&challenge)?;
    if signature.is_empty() || signature.len() > MAX_CERTIFICATE_BYTES {
        return Err(AuthError::InvalidInput("MFi signature length"));
    }
    use ctr::cipher::{KeyIvInit, StreamCipher};
    ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&key[..16], &iv[..16])
        .map_err(|_| AuthError::Crypto)?
        .apply_keystream(&mut signature);
    let mut response = public.to_vec();
    for blob in [Some(certificate), Some(signature), intermediate]
        .into_iter()
        .flatten()
    {
        response.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        response.extend_from_slice(&blob);
    }
    Ok(response)
}

fn encode_baa_intermediate(value: &[u8]) -> Vec<u8> {
    let mut blob = b"\xe1\x44baIC".to_vec();
    match value.len() {
        0..=32 => blob.push(0x70 + value.len() as u8),
        33..=255 => blob.extend_from_slice(&[0x91, value.len() as u8]),
        _ => {
            blob.push(0x92);
            blob.extend_from_slice(&(value.len() as u16).to_le_bytes());
        }
    }
    blob.extend_from_slice(value);
    blob
}
