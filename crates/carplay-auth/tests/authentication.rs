// SPDX-License-Identifier: GPL-3.0-only
//! Synthetic, in-process controller interoperability. Never reads real MFi files.
use carplay_auth::{
    AirPlayIdentity, AuthProvider, ControlReader, ControlWriter, LocalIdentity, MemoryPairingStore,
    PairSetup, PairVerify, PairingStore, crypto, mfi_sap_auth_setup, tlv,
};
use cms::content_info::ContentInfo;
use der::{Decode, Encode};
use num_bigint::BigUint;
use p256::{
    ecdsa::{Signature, VerifyingKey, signature::hazmat::PrehashVerifier},
    pkcs8::DecodePublicKey,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;

fn bytes<const N: usize>(hex: &str) -> [u8; N] {
    hex::decode(hex).unwrap().try_into().unwrap()
}

#[test]
fn rfc7748_x25519_and_low_order_rejection() {
    let alice = bytes("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let bob = bytes("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    assert_eq!(
        *crypto::x25519_shared(&alice, &bob).unwrap(),
        bytes::<32>("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")
    );
    assert!(crypto::x25519_shared(&alice, &[0; 32]).is_err());
    let mut order_one = [0; 32];
    order_one[0] = 1;
    assert!(crypto::x25519_shared(&alice, &order_one).is_err());
}

#[test]
fn rfc8032_ed25519_empty_message() {
    let secret = bytes("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let public = bytes("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
    let signature = bytes::<64>(concat!(
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
        "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    ));
    assert_eq!(crypto::ed25519_sign(&secret, &[]), signature);
    assert!(crypto::ed25519_verify(&public, &[], &signature));
    assert!(!crypto::ed25519_verify(&public, b"changed", &signature));
}

#[test]
fn rfc8439_chacha_aead_vector_and_authentication_failures() {
    let key = bytes("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let nonce = bytes("070000004041424344454647");
    let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
    let message = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let expected = hex::decode(concat!(
        "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6",
        "3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36",
        "92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3f",
        "f4def08e4b7a9de576d26586cec64b6116",
        "1ae10b594f09e26a7e902ecbd0600691"
    ))
    .unwrap();
    assert_eq!(
        crypto::chacha_seal(&key, &nonce, message, &aad).unwrap(),
        expected
    );
    assert_eq!(
        crypto::chacha_open(&key, &nonce, &expected, &aad).unwrap(),
        message
    );
    assert!(crypto::chacha_open(&key, &nonce, &expected, b"wrong AAD").is_err());
    let mut tampered = expected.clone();
    tampered[0] ^= 1;
    assert!(crypto::chacha_open(&key, &nonce, &tampered, &aad).is_err());
    assert!(crypto::chacha_open(&key, &nonce, &[0; 15], &aad).is_err());
    assert_eq!(
        crypto::nonce64(0x0807060504030201),
        [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(&crypto::nonce_label("PV-Msg02").unwrap()[4..], b"PV-Msg02");
    assert!(crypto::nonce_label("ninebytes").is_err());
    assert!(crypto::nonce_label("中文").is_err());
}

#[test]
fn tlv_fragments_are_strict_and_bounded() {
    let payload = vec![42; 768];
    let encoded = tlv::encode(&[(3, &payload), (6, &[1])]);
    assert_eq!(tlv::decode(&encoded).unwrap()[&3], payload);
    for malformed in [
        vec![1],
        vec![1, 2, 0],
        vec![6, 1, 1, 6, 1, 3],
        vec![255, 1, 0],
        vec![0; 65537],
    ] {
        assert!(tlv::decode(&malformed).is_err());
    }
    assert!(tlv::decode(&tlv::encode(&[(1, b"a"), (1, b"b")])).is_err());
}

fn synthetic_identity() -> (Vec<u8>, Vec<u8>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec!["synthetic-auth-test.invalid".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    (key.serialize_der(), certificate.der().to_vec())
}

fn verifying_key(certificate: &[u8]) -> VerifyingKey {
    let cert = x509_cert::Certificate::from_der(certificate).unwrap();
    VerifyingKey::from_public_key_der(
        &cert
            .tbs_certificate
            .subject_public_key_info
            .to_der()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn local_mfi_der_pkcs7_and_prehashed_signature() {
    let (key, cert) = synthetic_identity();
    let pkcs7 = ContentInfo::try_from(x509_cert::Certificate::from_der(&cert).unwrap())
        .unwrap()
        .to_der()
        .unwrap();
    for body in [&cert, &pkcs7] {
        let identity = LocalIdentity::from_der(&key, body).unwrap();
        assert_eq!(identity.protocol_major(), 3);
        assert!(identity.self_check().key_matches_certificate);
        assert!(!identity.self_check().iphone_trust_verified);
        assert_eq!(&identity.certificate().unwrap(), body);
        assert!(identity.certificate_bounded(1).is_err());
        let digest = Sha256::digest(b"synthetic unique challenge");
        let signature = identity.sign_challenge(&digest).unwrap();
        assert_eq!(signature.len(), 64);
        let public = verifying_key(&cert);
        public
            .verify_prehash(&digest, &Signature::from_slice(&signature).unwrap())
            .unwrap();
        assert!(
            public
                .verify_prehash(
                    &Sha256::digest(digest),
                    &Signature::from_slice(&signature).unwrap()
                )
                .is_err()
        );
        for size in [0, 20, 31, 33, 64] {
            assert!(identity.sign_challenge(&vec![0; size]).is_err());
        }
    }
}

#[test]
fn identity_loading_rejects_mismatch_oversize_wrong_curve_and_multiple_certificates() {
    let (key, cert) = synthetic_identity();
    let (other_key, other_cert) = synthetic_identity();
    assert!(LocalIdentity::from_der(&other_key, &cert).is_err());
    assert!(LocalIdentity::from_der(&[], &cert).is_err());
    assert!(LocalIdentity::from_der(&key, &[0; 16385]).is_err());
    let pkcs7 = ContentInfo::try_from(vec![
        x509_cert::Certificate::from_der(&cert).unwrap(),
        x509_cert::Certificate::from_der(&other_cert).unwrap(),
    ])
    .unwrap()
    .to_der()
    .unwrap();
    assert!(LocalIdentity::from_der(&key, &pkcs7).is_err());
    let wrong = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
    let wrong_cert = rcgen::CertificateParams::new(vec!["wrong.invalid".into()])
        .unwrap()
        .self_signed(&wrong)
        .unwrap();
    assert!(LocalIdentity::from_der(&wrong.serialize_der(), wrong_cert.der()).is_err());
    let temporary = tempfile::tempdir().unwrap();
    assert!(LocalIdentity::load(temporary.path()).is_err());
    std::fs::write(temporary.path().join("identity.pk8"), &key).unwrap();
    std::fs::write(temporary.path().join("certificate.p7b"), &cert).unwrap();
    assert!(LocalIdentity::load(temporary.path()).is_ok());
    std::fs::write(temporary.path().join("identity.pk8"), [0; 16385]).unwrap();
    assert!(LocalIdentity::load(temporary.path()).is_err());
}

#[test]
fn mfi_sap_response_decrypts_and_verifies_without_hashing_twice() {
    let (key, cert) = synthetic_identity();
    let identity = LocalIdentity::from_der(&key, &cert).unwrap();
    let (private, public) = crypto::x25519_generate().unwrap();
    let response = mfi_sap_auth_setup(&[&[1], public.as_slice()].concat(), &identity).unwrap();
    let accessory: [u8; 32] = response[..32].try_into().unwrap();
    let shared = crypto::x25519_shared(&private, &accessory).unwrap();
    let certificate_len = u32::from_be_bytes(response[32..36].try_into().unwrap()) as usize;
    assert_eq!(&response[36..36 + certificate_len], &cert);
    let pos = 36 + certificate_len;
    let sig_len = u32::from_be_bytes(response[pos..pos + 4].try_into().unwrap()) as usize;
    assert_eq!(sig_len, 64);
    assert_eq!(response.len(), pos + 4 + sig_len);
    let mut signature = response[pos + 4..].to_vec();
    let aes_key = sha1::Sha1::digest([b"AES-KEY".as_slice(), shared.as_ref()].concat());
    let aes_iv = sha1::Sha1::digest([b"AES-IV".as_slice(), shared.as_ref()].concat());
    use ctr::cipher::{KeyIvInit, StreamCipher};
    ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&aes_key[..16], &aes_iv[..16])
        .unwrap()
        .apply_keystream(&mut signature);
    let challenge = Sha256::digest([accessory.as_slice(), &public].concat());
    verifying_key(&cert)
        .verify_prehash(&challenge, &Signature::from_slice(&signature).unwrap())
        .unwrap();
    for malformed in [
        vec![],
        vec![1; 32],
        vec![2; 33],
        vec![1; 34],
        [&[1], [0; 32].as_slice()].concat(),
    ] {
        assert!(mfi_sap_auth_setup(&malformed, &identity).is_err());
    }
}

#[test]
fn mfi_v2_and_baa_use_their_distinct_challenge_and_response_shapes() {
    struct Provider {
        major: u8,
        baa: bool,
        intermediate: Vec<u8>,
        seen: std::sync::Mutex<Vec<u8>>,
    }
    impl AuthProvider for Provider {
        fn protocol_major(&self) -> u8 {
            self.major
        }
        fn certificate_type(&self) -> carplay_auth::CertificateType {
            if self.baa {
                carplay_auth::CertificateType::Baa
            } else {
                carplay_auth::CertificateType::Mfi
            }
        }
        fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
            Ok(b"synthetic certificate".to_vec())
        }
        fn sign_challenge(&self, challenge: &[u8]) -> carplay_auth::Result<Vec<u8>> {
            *self.seen.lock().unwrap() = challenge.to_vec();
            Ok(vec![0xab; 128])
        }
        fn baa_certificates(&self) -> carplay_auth::Result<carplay_auth::BaaCertificates> {
            Ok(carplay_auth::BaaCertificates {
                leaf: b"synthetic BAA leaf".to_vec(),
                intermediate: self.intermediate.clone(),
            })
        }
    }
    let (_, peer) = crypto::x25519_generate().unwrap();
    let request = [&[1], peer.as_slice()].concat();
    let v2 = Provider {
        major: 2,
        baa: false,
        intermediate: vec![],
        seen: Default::default(),
    };
    let response = mfi_sap_auth_setup(&request, &v2).unwrap();
    assert_eq!(
        *v2.seen.lock().unwrap(),
        sha1::Sha1::digest([&response[..32], &peer].concat()).to_vec()
    );
    for length in [1, 32, 33, 255, 256, 65525] {
        let provider = Provider {
            major: 3,
            baa: true,
            intermediate: vec![0x4e; length],
            seen: Default::default(),
        };
        let response = mfi_sap_auth_setup(&request, &provider).unwrap();
        assert_eq!(
            *provider.seen.lock().unwrap(),
            [&response[..32], &peer].concat()
        );
        let mut position = 32;
        for _ in 0..2 {
            let length =
                u32::from_be_bytes(response[position..position + 4].try_into().unwrap()) as usize;
            position += 4 + length;
        }
        let encoded_len =
            u32::from_be_bytes(response[position..position + 4].try_into().unwrap()) as usize;
        let blob = &response[position + 4..];
        assert_eq!(blob.len(), encoded_len);
        assert_eq!(&blob[..6], b"\xe1\x44baIC");
        let prefix = match length {
            1..=32 => 7,
            33..=255 => 8,
            _ => 9,
        };
        assert_eq!(&blob[prefix..], &provider.intermediate);
        match length {
            1..=32 => assert_eq!(blob[6], 0x70 + length as u8),
            33..=255 => assert_eq!(&blob[6..8], &[0x91, length as u8]),
            _ => {
                assert_eq!(blob[6], 0x92);
                assert_eq!(
                    u16::from_le_bytes(blob[7..9].try_into().unwrap()) as usize,
                    length
                );
            }
        }
    }
}

fn pad(n: &BigUint) -> Vec<u8> {
    let encoded = n.to_bytes_be();
    let mut result = vec![0; 384];
    result[384 - encoded.len()..].copy_from_slice(&encoded);
    result
}
struct ClientSrp {
    a: Vec<u8>,
    proof: [u8; 64],
    key: [u8; 64],
}
/// Independent num-bigint controller arithmetic cross-checks the fixed-width server.
fn client_srp(salt: &[u8], server_b: &[u8], password: &str) -> ClientSrp {
    let n = BigUint::parse_bytes(carplay_auth::srp::MODULUS_HEX.as_bytes(), 16).unwrap();
    let g = BigUint::from(5u8);
    let a = BigUint::from_bytes_be(&[0x3d; 32]);
    let a_pub = pad(&g.modpow(&a, &n));
    let b = BigUint::from_bytes_be(server_b);
    let x = BigUint::from_bytes_be(&crypto::sha512(&[
        salt,
        &crypto::sha512(&[b"Pair-Setup:", password.as_bytes()]),
    ]));
    let k = BigUint::from_bytes_be(&crypto::sha512(&[&pad(&n), &pad(&g)]));
    let u = BigUint::from_bytes_be(&crypto::sha512(&[&a_pub, &pad(&b)]));
    let base = (&b + &n - (&k * g.modpow(&x, &n)) % &n) % &n;
    let s = base.modpow(&(a + u * x), &n);
    let key = crypto::sha512(&[&s.to_bytes_be()]);
    let mut xor = crypto::sha512(&[&pad(&n)]);
    for (a, b) in xor.iter_mut().zip(crypto::sha512(&[&[5]])) {
        *a ^= b;
    }
    let proof = crypto::sha512(&[
        &xor,
        &crypto::sha512(&[b"Pair-Setup"]),
        salt,
        &a_pub,
        &pad(&b),
        &key,
    ]);
    ClientSrp {
        a: a_pub,
        proof,
        key,
    }
}

fn run_setup(
    identity: Arc<AirPlayIdentity>,
    store: Arc<dyn PairingStore>,
    controller: &AirPlayIdentity,
) {
    let mut setup = PairSetup::new(identity.clone(), store);
    let m2 = tlv::decode(&setup.handle(&tlv::encode(&[(6, &[1])]))).unwrap();
    assert_eq!(m2[&6], [2]);
    assert_eq!(m2[&2].len(), 16);
    assert_eq!(m2[&3].len(), 384);
    let srp = client_srp(&m2[&2], &m2[&3], "3939");
    let m4 = tlv::decode(&setup.handle(&tlv::encode(&[(6, &[3]), (3, &srp.a), (4, &srp.proof)])))
        .unwrap();
    assert_eq!(m4[&6], [4]);
    assert_eq!(m4[&4], crypto::sha512(&[&srp.a, &srp.proof, &srp.key]));
    let signing = crypto::derive_key(
        &srp.key,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
    )
    .unwrap();
    let signature = controller.sign(
        &[
            signing.as_ref(),
            controller.pairing_id.as_bytes(),
            &controller.public_key(),
        ]
        .concat(),
    );
    let sub = tlv::encode(&[
        (1, controller.pairing_id.as_bytes()),
        (3, &controller.public_key()),
        (10, &signature),
    ]);
    let encryption = crypto::derive_key(
        &srp.key,
        b"Pair-Setup-Encrypt-Salt",
        b"Pair-Setup-Encrypt-Info",
    )
    .unwrap();
    let sealed = crypto::chacha_seal(
        &encryption,
        &crypto::nonce_label("PS-Msg05").unwrap(),
        &sub,
        &[],
    )
    .unwrap();
    let m5 = tlv::encode(&[(6, &[5]), (5, &sealed)]);
    let m6 = tlv::decode(&setup.handle(&m5)).unwrap();
    assert_eq!(m6[&6], [6]);
    assert!(!m6.contains_key(&7));
    assert!(setup.complete());
    let sub = tlv::decode(
        &crypto::chacha_open(
            &encryption,
            &crypto::nonce_label("PS-Msg06").unwrap(),
            &m6[&5],
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sub[&1], identity.pairing_id.as_bytes());
    assert_eq!(sub[&3], identity.public_key());
    let signing = crypto::derive_key(
        &srp.key,
        b"Pair-Setup-Accessory-Sign-Salt",
        b"Pair-Setup-Accessory-Sign-Info",
    )
    .unwrap();
    assert!(crypto::ed25519_verify(
        &identity.public_key(),
        &[signing.as_ref(), &sub[&1], &sub[&3]].concat(),
        &sub[&10]
    ));
    assert!(tlv::decode(&setup.handle(&m5)).unwrap().contains_key(&7)); // replay cannot persist again
    assert!(!setup.complete());
}

fn verify_request(
    verify: &mut PairVerify,
    identity: &AirPlayIdentity,
    controller: &AirPlayIdentity,
) -> (Vec<u8>, [u8; 32]) {
    let (private, public) = crypto::x25519_generate().unwrap();
    let m2 = tlv::decode(&verify.handle(&tlv::encode(&[(6, &[1]), (3, &public)]))).unwrap();
    assert_eq!(m2[&6], [2]);
    let accessory: [u8; 32] = m2[&3].as_slice().try_into().unwrap();
    let shared = crypto::x25519_shared(&private, &accessory).unwrap();
    let encryption = crypto::derive_key(
        shared.as_ref(),
        b"Pair-Verify-Encrypt-Salt",
        b"Pair-Verify-Encrypt-Info",
    )
    .unwrap();
    let sub = tlv::decode(
        &crypto::chacha_open(
            &encryption,
            &crypto::nonce_label("PV-Msg02").unwrap(),
            &m2[&5],
            &[],
        )
        .unwrap(),
    )
    .unwrap();
    assert!(crypto::ed25519_verify(
        &identity.public_key(),
        &[accessory.as_slice(), &sub[&1], &public].concat(),
        &sub[&10]
    ));
    let signature = controller.sign(
        &[
            public.as_slice(),
            controller.pairing_id.as_bytes(),
            &accessory,
        ]
        .concat(),
    );
    let sub = tlv::encode(&[(1, controller.pairing_id.as_bytes()), (10, &signature)]);
    let sealed = crypto::chacha_seal(
        &encryption,
        &crypto::nonce_label("PV-Msg03").unwrap(),
        &sub,
        &[],
    )
    .unwrap();
    (tlv::encode(&[(6, &[3]), (5, &sealed)]), *shared)
}

#[test]
fn full_pair_setup_verify_then_bidirectional_encrypted_control() {
    let accessory = Arc::new(AirPlayIdentity::generate().unwrap());
    let controller = AirPlayIdentity::generate().unwrap();
    let store = Arc::new(MemoryPairingStore::new());
    run_setup(accessory.clone(), store.clone(), &controller);
    assert_eq!(
        store.get(&controller.pairing_id).unwrap(),
        Some(controller.public_key())
    );
    let mut verify = PairVerify::new(accessory.clone(), store);
    let (m3, shared) = verify_request(&mut verify, &accessory, &controller);
    assert_eq!(verify.handle(&m3), tlv::encode(&[(6, &[4])]));
    assert!(verify.is_verified());
    assert_eq!(
        verify.verified_controller_id(),
        Some(controller.pairing_id.as_str())
    );
    assert_eq!(verify.shared_secret(), Some(&shared));
    let keys = verify.control_keys().unwrap();
    let controller_write =
        crypto::derive_key(&shared, b"Control-Salt", b"Control-Write-Encryption-Key").unwrap();
    let controller_read =
        crypto::derive_key(&shared, b"Control-Salt", b"Control-Read-Encryption-Key").unwrap();
    let mut sender = ControlWriter::new(*controller_write);
    let mut receiver = ControlReader::new(*keys.read_key);
    let message = vec![0x72; 100_001];
    let encrypted = sender.encrypt(&message).unwrap();
    let mut output = Vec::new();
    for fragment in encrypted.chunks(7) {
        output.extend(receiver.decrypt(fragment).unwrap());
    }
    assert_eq!(output, message);
    assert_eq!(receiver.pending_bytes(), 0);
    let reply = ControlWriter::new(*keys.write_key)
        .encrypt(b"RTSP/1.0 200 OK\r\n\r\n")
        .unwrap();
    assert_eq!(
        ControlReader::new(*controller_read)
            .decrypt(&reply)
            .unwrap(),
        b"RTSP/1.0 200 OK\r\n\r\n"
    );
    assert!(tlv::decode(&verify.handle(&m3)).unwrap().contains_key(&7));
    assert!(!verify.is_verified());
    assert!(verify.control_keys().is_none());
}

#[test]
fn setup_rejects_invalid_order_password_and_srp_public_values() {
    let identity = Arc::new(AirPlayIdentity::generate().unwrap());
    let store = Arc::new(MemoryPairingStore::new());
    let mut setup = PairSetup::new(identity, store);
    for request in [
        vec![6],
        tlv::encode(&[(6, &[5]), (5, &[0; 16])]),
        tlv::encode(&[(6, &[3]), (3, &[0]), (4, &[0; 64])]),
    ] {
        assert!(
            tlv::decode(&setup.handle(&request))
                .unwrap()
                .contains_key(&7)
        );
        assert!(!setup.complete());
    }
    let m2 = tlv::decode(&setup.handle(&tlv::encode(&[(6, &[1])]))).unwrap();
    let wrong = client_srp(&m2[&2], &m2[&3], "3940");
    assert!(
        tlv::decode(&setup.handle(&tlv::encode(&[(6, &[3]), (3, &wrong.a), (4, &wrong.proof)])))
            .unwrap()
            .contains_key(&7)
    );
    let srp = carplay_auth::srp::SrpSession::start("Pair-Setup", "3939").unwrap();
    assert!(srp.verify(&[0; 384], &[0; 64]).is_err());
}

#[test]
fn verify_rejects_unknown_controller_and_tamper_then_clears_session() {
    let identity = Arc::new(AirPlayIdentity::generate().unwrap());
    let controller = AirPlayIdentity::generate().unwrap();
    let store = Arc::new(MemoryPairingStore::new());
    let mut verify = PairVerify::new(identity.clone(), store.clone());
    let (m3, _) = verify_request(&mut verify, &identity, &controller);
    assert!(tlv::decode(&verify.handle(&m3)).unwrap().contains_key(&7));
    assert!(!verify.is_verified());
    store
        .save(&controller.pairing_id, controller.public_key())
        .unwrap();
    let (mut m3, _) = verify_request(&mut verify, &identity, &controller);
    *m3.last_mut().unwrap() ^= 1;
    assert!(tlv::decode(&verify.handle(&m3)).unwrap().contains_key(&7));
    assert!(verify.shared_secret().is_none());
    assert!(
        tlv::decode(&verify.handle(&tlv::encode(&[(6, &[1]), (3, &[0; 32])])))
            .unwrap()
            .contains_key(&7)
    );
    assert!(
        tlv::decode(&verify.handle(&tlv::encode(&[(6, &[1]), (3, &[0; 31])])))
            .unwrap()
            .contains_key(&7)
    );
}

#[test]
fn encrypted_record_corruption_replay_and_empty_messages() {
    let key = [0x81; 32];
    let mut writer = ControlWriter::new(key);
    let first = writer.encrypt(&[]).unwrap();
    assert_eq!(first.len(), 18);
    let mut reader = ControlReader::new(key);
    assert_eq!(reader.decrypt(&first).unwrap(), Vec::<u8>::new());
    assert!(reader.decrypt(&first).is_err());
    assert!(reader.decrypt(&writer.encrypt(b"later").unwrap()).is_err());
    let mut tampered = ControlWriter::new(key).encrypt(b"message").unwrap();
    tampered[3] ^= 1;
    assert!(ControlReader::new(key).decrypt(&tampered).is_err());
}
