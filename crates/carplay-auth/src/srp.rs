// SPDX-License-Identifier: GPL-3.0-only
//! DiPlay `airplay/Srp6a.kt`: RFC 5054 3072-bit group / SHA-512 server.
//! AirPlay hashes the unpadded shared integer, but padded public integers.
//! Secret modular exponentiation uses crypto-bigint's fixed-width algorithms.
use crate::{AuthError, Result, crypto};
use crypto_bigint::{
    Encoding, U256, U512, U3072,
    modular::runtime_mod::{DynResidue, DynResidueParams},
};
use std::sync::OnceLock;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const MODULUS_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
    "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437",
    "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05",
    "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB",
    "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183",
    "995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A",
    "85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7AB",
    "F5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D8",
    "7602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E208",
    "E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"
);
type Modular = DynResidue<{ U3072::LIMBS }>;
fn modular(value: &U3072) -> Modular {
    static PARAMS: OnceLock<DynResidueParams<{ U3072::LIMBS }>> = OnceLock::new();
    Modular::new(
        value,
        *PARAMS.get_or_init(|| DynResidueParams::new(&U3072::from_be_hex(MODULUS_HEX))),
    )
}

pub struct SrpSession {
    pub salt: [u8; 16],
    public: U3072,
    identifier: Vec<u8>,
    verifier: Zeroizing<U3072>,
    exponent: Zeroizing<U256>,
}

pub struct SrpProof {
    pub session_key: Zeroizing<[u8; 64]>,
    pub server_proof: [u8; 64],
}

impl SrpSession {
    pub fn start(username: &str, password: &str) -> Result<Self> {
        Self::with_entropy(
            username,
            password,
            crypto::random_bytes()?,
            crypto::random_bytes()?,
        )
    }

    fn with_entropy(
        username: &str,
        password: &str,
        salt: [u8; 16],
        private: [u8; 32],
    ) -> Result<Self> {
        let private = Zeroizing::new(private);
        let exponent = Zeroizing::new(U256::from_be_slice(private.as_ref()));
        if *exponent == U256::ZERO {
            return Err(AuthError::Crypto);
        }
        let inner = Zeroizing::new(crypto::sha512(&[
            username.as_bytes(),
            b":",
            password.as_bytes(),
        ]));
        let x = Zeroizing::new(U512::from_be_slice(&crypto::sha512(&[
            &salt,
            inner.as_ref(),
        ])));
        let g = modular(&U3072::from_u8(5));
        let verifier = Zeroizing::new(g.pow(&x).retrieve());
        let n = U3072::from_be_hex(MODULUS_HEX).to_be_bytes();
        let g_padded = U3072::from_u8(5).to_be_bytes();
        let k = modular(&integer(&crypto::sha512(&[&n, &g_padded]))?);
        let public = (k * modular(&verifier) + g.pow(&exponent)).retrieve();
        Ok(Self {
            salt,
            public,
            identifier: username.as_bytes().to_vec(),
            verifier,
            exponent,
        })
    }

    pub fn public_key(&self) -> [u8; 384] {
        self.public.to_be_bytes()
    }

    /// A proof consumes its challenge so failed/repeated attempts cannot reuse it.
    pub fn verify(self, public_a: &[u8], client_proof: &[u8]) -> Result<SrpProof> {
        if client_proof.len() != 64 {
            return Err(AuthError::Authentication);
        }
        let a = integer(public_a)?;
        let n = U3072::from_be_hex(MODULUS_HEX);
        if a == U3072::ZERO || a >= n {
            return Err(AuthError::Authentication);
        }
        let a_bytes = a.to_be_bytes();
        let b_bytes = self.public.to_be_bytes();
        let u = U512::from_be_slice(&crypto::sha512(&[&a_bytes, &b_bytes]));
        if u == U512::ZERO {
            return Err(AuthError::Authentication);
        }
        let shared = Zeroizing::new(
            (modular(&a) * modular(&self.verifier).pow(&u))
                .pow(&self.exponent)
                .retrieve()
                .to_be_bytes(),
        );
        let first = shared
            .iter()
            .position(|&byte| byte != 0)
            .ok_or(AuthError::Authentication)?;
        let key = Zeroizing::new(crypto::sha512(&[&shared[first..]]));
        let mut xor = crypto::sha512(&[&n.to_be_bytes()]);
        let hash_g = crypto::sha512(&[&[5]]);
        for (a, b) in xor.iter_mut().zip(hash_g) {
            *a ^= b;
        }
        let expected = crypto::sha512(&[
            &xor,
            &crypto::sha512(&[&self.identifier]),
            &self.salt,
            &a_bytes,
            &b_bytes,
            key.as_ref(),
        ]);
        if !bool::from(expected.as_slice().ct_eq(client_proof)) {
            return Err(AuthError::Authentication);
        }
        Ok(SrpProof {
            server_proof: crypto::sha512(&[&a_bytes, client_proof, key.as_ref()]),
            session_key: key,
        })
    }
}

fn integer(input: &[u8]) -> Result<U3072> {
    if input.is_empty() || input.len() > 384 {
        return Err(AuthError::InvalidInput("SRP integer length"));
    }
    let mut padded = [0; 384];
    padded[384 - input.len()..].copy_from_slice(input);
    Ok(U3072::from_be_slice(&padded))
}
