// SPDX-License-Identifier: GPL-3.0-only
//! DiPlay `airplay/{AirPlayIdentity,PairingStore,PairSetup,PairVerify}.kt`.
//! The host owns persistence and must protect identity material at rest (DPAPI
//! or private files). Handlers are per-connection and must not be shared across peers.
use crate::{AuthError, Result, crypto, srp::SrpSession, tlv};
use ed25519_dalek::SigningKey;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use zeroize::Zeroizing;

const IDENTIFIER: u8 = 1;
const SALT: u8 = 2;
const PUBLIC_KEY: u8 = 3;
const PROOF: u8 = 4;
const ENCRYPTED_DATA: u8 = 5;
const STATE: u8 = 6;
const ERROR: u8 = 7;
const SIGNATURE: u8 = 10;

pub struct AirPlayIdentity {
    pub pairing_id: String,
    seed: Zeroizing<[u8; 32]>,
}
impl AirPlayIdentity {
    pub fn generate() -> Result<Self> {
        Self::from_seed(uuid::Uuid::new_v4().to_string(), crypto::random_bytes()?)
    }
    pub fn from_seed(pairing_id: impl Into<String>, seed: [u8; 32]) -> Result<Self> {
        let pairing_id = pairing_id.into();
        validate_identifier(pairing_id.as_bytes())?;
        Ok(Self {
            pairing_id,
            seed: Zeroizing::new(seed),
        })
    }
    pub fn public_key(&self) -> [u8; 32] {
        SigningKey::from_bytes(&self.seed)
            .verifying_key()
            .to_bytes()
    }
    /// The caller must use protected storage and must never put this in logs.
    pub fn export_seed(&self) -> Zeroizing<[u8; 32]> {
        self.seed.clone()
    }
    pub fn sign(&self, data: &[u8]) -> [u8; 64] {
        crypto::ed25519_sign(&self.seed, data)
    }
}

pub trait PairingStore: Send + Sync {
    fn get(&self, identifier: &str) -> Result<Option<[u8; 32]>>;
    /// Must durably commit before returning success if persistence is required.
    fn save(&self, identifier: &str, public_key: [u8; 32]) -> Result<()>;
}

#[derive(Default)]
pub struct MemoryPairingStore {
    entries: RwLock<HashMap<String, [u8; 32]>>,
}
impl MemoryPairingStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&self) -> Result<()> {
        self.entries
            .write()
            .map_err(|_| AuthError::Store("lock poisoned".into()))?
            .clear();
        Ok(())
    }
}
impl PairingStore for MemoryPairingStore {
    fn get(&self, identifier: &str) -> Result<Option<[u8; 32]>> {
        Ok(self
            .entries
            .read()
            .map_err(|_| AuthError::Store("lock poisoned".into()))?
            .get(identifier)
            .copied())
    }
    fn save(&self, identifier: &str, public_key: [u8; 32]) -> Result<()> {
        validate_identifier(identifier.as_bytes())?;
        ed25519_dalek::VerifyingKey::from_bytes(&public_key)
            .map_err(|_| AuthError::InvalidInput("controller public key"))?;
        self.entries
            .write()
            .map_err(|_| AuthError::Store("lock poisoned".into()))?
            .insert(identifier.into(), public_key);
        Ok(())
    }
}

enum SetupPhase {
    Idle,
    Challenge(Box<SrpSession>),
    Proven(Zeroizing<[u8; 64]>),
    Complete,
    Failed,
}
pub struct PairSetup {
    identity: Arc<AirPlayIdentity>,
    store: Arc<dyn PairingStore>,
    phase: SetupPhase,
}
impl PairSetup {
    pub fn new(identity: Arc<AirPlayIdentity>, store: Arc<dyn PairingStore>) -> Self {
        Self {
            identity,
            store,
            phase: SetupPhase::Idle,
        }
    }
    pub fn complete(&self) -> bool {
        matches!(self.phase, SetupPhase::Complete)
    }
    /// Protocol failures return TLV State + Authentication Error, never key data.
    /// Start a new object/connection after failure; a repeated M1 may restart.
    pub fn handle(&mut self, body: &[u8]) -> Vec<u8> {
        let decoded = tlv::decode(body);
        let state = decoded
            .as_ref()
            .ok()
            .and_then(|v| state(v).ok())
            .unwrap_or(0);
        let reply = decoded.and_then(|fields| self.step(state, &fields));
        match reply {
            Ok(reply) => reply,
            Err(_) => {
                self.phase = SetupPhase::Failed;
                error_reply(response_state(state))
            }
        }
    }
    fn step(
        &mut self,
        state: u8,
        fields: &std::collections::BTreeMap<u8, Vec<u8>>,
    ) -> Result<Vec<u8>> {
        match state {
            1 => {
                if matches!(self.phase, SetupPhase::Complete) {
                    return Err(AuthError::Authentication);
                }
                let srp = SrpSession::start("Pair-Setup", "3939")?;
                let reply = tlv::encode(&[
                    (STATE, &[2]),
                    (PUBLIC_KEY, &srp.public_key()),
                    (SALT, &srp.salt),
                ]);
                self.phase = SetupPhase::Challenge(Box::new(srp));
                Ok(reply)
            }
            3 => {
                let SetupPhase::Challenge(srp) =
                    std::mem::replace(&mut self.phase, SetupPhase::Failed)
                else {
                    return Err(AuthError::Authentication);
                };
                let proof = (*srp).verify(field(fields, PUBLIC_KEY)?, field(fields, PROOF)?)?;
                let reply = tlv::encode(&[(STATE, &[4]), (PROOF, &proof.server_proof)]);
                self.phase = SetupPhase::Proven(proof.session_key);
                Ok(reply)
            }
            5 => {
                let SetupPhase::Proven(key) =
                    std::mem::replace(&mut self.phase, SetupPhase::Failed)
                else {
                    return Err(AuthError::Authentication);
                };
                let encryption_key = crypto::derive_key(
                    key.as_ref(),
                    b"Pair-Setup-Encrypt-Salt",
                    b"Pair-Setup-Encrypt-Info",
                )?;
                let plaintext = Zeroizing::new(crypto::chacha_open(
                    &encryption_key,
                    &crypto::nonce_label("PS-Msg05")?,
                    field(fields, ENCRYPTED_DATA)?,
                    &[],
                )?);
                let sub = tlv::decode(&plaintext)?;
                let id = field(&sub, IDENTIFIER)?;
                let controller_id = validate_identifier(id)?;
                let public: [u8; 32] = field(&sub, PUBLIC_KEY)?
                    .try_into()
                    .map_err(|_| AuthError::Authentication)?;
                let signing = crypto::derive_key(
                    key.as_ref(),
                    b"Pair-Setup-Controller-Sign-Salt",
                    b"Pair-Setup-Controller-Sign-Info",
                )?;
                let signed = [signing.as_ref(), id, &public].concat();
                if !crypto::ed25519_verify(&public, &signed, field(&sub, SIGNATURE)?) {
                    return Err(AuthError::Authentication);
                }
                let own_id = self.identity.pairing_id.as_bytes();
                let own_public = self.identity.public_key();
                let own_signing = crypto::derive_key(
                    key.as_ref(),
                    b"Pair-Setup-Accessory-Sign-Salt",
                    b"Pair-Setup-Accessory-Sign-Info",
                )?;
                let signature = self
                    .identity
                    .sign(&[own_signing.as_ref(), own_id, &own_public].concat());
                let reply = tlv::encode(&[
                    (IDENTIFIER, own_id),
                    (PUBLIC_KEY, &own_public),
                    (SIGNATURE, &signature),
                ]);
                let encrypted = crypto::chacha_seal(
                    &encryption_key,
                    &crypto::nonce_label("PS-Msg06")?,
                    &reply,
                    &[],
                )?;
                self.store.save(controller_id, public)?;
                self.phase = SetupPhase::Complete;
                Ok(tlv::encode(&[(STATE, &[6]), (ENCRYPTED_DATA, &encrypted)]))
            }
            _ => Err(AuthError::Authentication),
        }
    }
}

/// Keys are named from the accessory/server perspective, the reverse of the
/// controller's HKDF Read/Write labels. Avoid printing or copying them to logs.
#[derive(Clone)]
pub struct ControlKeys {
    pub read_key: Zeroizing<[u8; 32]>,
    pub write_key: Zeroizing<[u8; 32]>,
}
struct VerifyChallenge {
    public: [u8; 32],
    peer: [u8; 32],
    shared: Zeroizing<[u8; 32]>,
    encryption: Zeroizing<[u8; 32]>,
}
pub struct PairVerify {
    identity: Arc<AirPlayIdentity>,
    store: Arc<dyn PairingStore>,
    challenge: Option<VerifyChallenge>,
    keys: Option<ControlKeys>,
    verified_id: Option<String>,
    shared: Option<Zeroizing<[u8; 32]>>,
}
impl PairVerify {
    pub fn new(identity: Arc<AirPlayIdentity>, store: Arc<dyn PairingStore>) -> Self {
        Self {
            identity,
            store,
            challenge: None,
            keys: None,
            verified_id: None,
            shared: None,
        }
    }
    pub fn is_verified(&self) -> bool {
        self.verified_id.is_some()
    }
    pub fn verified_controller_id(&self) -> Option<&str> {
        self.verified_id.as_deref()
    }
    pub fn control_keys(&self) -> Option<&ControlKeys> {
        self.keys.as_ref()
    }
    pub fn shared_secret(&self) -> Option<&[u8; 32]> {
        self.shared.as_deref()
    }
    fn reset(&mut self) {
        self.challenge = None;
        self.keys = None;
        self.shared = None;
        self.verified_id = None;
    }
    pub fn handle(&mut self, body: &[u8]) -> Vec<u8> {
        let decoded = tlv::decode(body);
        let state = decoded
            .as_ref()
            .ok()
            .and_then(|v| state(v).ok())
            .unwrap_or(0);
        let reply = decoded.and_then(|fields| self.step(state, &fields));
        match reply {
            Ok(reply) => reply,
            Err(_) => {
                self.reset();
                error_reply(response_state(state))
            }
        }
    }
    fn step(
        &mut self,
        state: u8,
        fields: &std::collections::BTreeMap<u8, Vec<u8>>,
    ) -> Result<Vec<u8>> {
        match state {
            1 => {
                self.reset();
                let peer = field(fields, PUBLIC_KEY)?
                    .try_into()
                    .map_err(|_| AuthError::Authentication)?;
                let (private, public) = crypto::x25519_generate()?;
                let shared = crypto::x25519_shared(&private, &peer)?;
                let encryption = crypto::derive_key(
                    shared.as_ref(),
                    b"Pair-Verify-Encrypt-Salt",
                    b"Pair-Verify-Encrypt-Info",
                )?;
                let id = self.identity.pairing_id.as_bytes();
                let signature = self.identity.sign(&[public.as_slice(), id, &peer].concat());
                let sub = tlv::encode(&[(IDENTIFIER, id), (SIGNATURE, &signature)]);
                let encrypted =
                    crypto::chacha_seal(&encryption, &crypto::nonce_label("PV-Msg02")?, &sub, &[])?;
                self.challenge = Some(VerifyChallenge {
                    public,
                    peer,
                    shared,
                    encryption,
                });
                Ok(tlv::encode(&[
                    (STATE, &[2]),
                    (PUBLIC_KEY, &public),
                    (ENCRYPTED_DATA, &encrypted),
                ]))
            }
            3 => {
                let challenge = self.challenge.take().ok_or(AuthError::Authentication)?;
                let decrypted = Zeroizing::new(crypto::chacha_open(
                    &challenge.encryption,
                    &crypto::nonce_label("PV-Msg03")?,
                    field(fields, ENCRYPTED_DATA)?,
                    &[],
                )?);
                let sub = tlv::decode(&decrypted)?;
                let id = field(&sub, IDENTIFIER)?;
                let controller_id = validate_identifier(id)?;
                let ltpk = self
                    .store
                    .get(controller_id)?
                    .ok_or(AuthError::Authentication)?;
                let data = [challenge.peer.as_slice(), id, &challenge.public].concat();
                if !crypto::ed25519_verify(&ltpk, &data, field(&sub, SIGNATURE)?) {
                    return Err(AuthError::Authentication);
                }
                self.keys = Some(ControlKeys {
                    read_key: crypto::derive_key(
                        challenge.shared.as_ref(),
                        b"Control-Salt",
                        b"Control-Write-Encryption-Key",
                    )?,
                    write_key: crypto::derive_key(
                        challenge.shared.as_ref(),
                        b"Control-Salt",
                        b"Control-Read-Encryption-Key",
                    )?,
                });
                self.verified_id = Some(controller_id.into());
                self.shared = Some(challenge.shared);
                Ok(tlv::encode(&[(STATE, &[4])]))
            }
            _ => Err(AuthError::Authentication),
        }
    }
}

fn field(fields: &std::collections::BTreeMap<u8, Vec<u8>>, kind: u8) -> Result<&[u8]> {
    fields
        .get(&kind)
        .map(Vec::as_slice)
        .ok_or(AuthError::Authentication)
}
fn state(fields: &std::collections::BTreeMap<u8, Vec<u8>>) -> Result<u8> {
    let state = field(fields, STATE)?;
    if state.len() != 1 {
        return Err(AuthError::Authentication);
    }
    Ok(state[0])
}
fn validate_identifier(bytes: &[u8]) -> Result<&str> {
    if bytes.is_empty() || bytes.len() > 1024 || bytes.contains(&0) {
        return Err(AuthError::InvalidInput("pairing identifier"));
    }
    std::str::from_utf8(bytes).map_err(|_| AuthError::InvalidInput("pairing identifier UTF-8"))
}
fn response_state(state: u8) -> u8 {
    match state {
        1 | 3 | 5 => state + 1,
        _ => state,
    }
}
fn error_reply(state: u8) -> Vec<u8> {
    tlv::encode(&[(STATE, &[state]), (ERROR, &[2])])
}
