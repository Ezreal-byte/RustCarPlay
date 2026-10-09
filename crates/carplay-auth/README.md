# carplay-auth

Rust port of DiPlay's accessory-side authentication and AirPlay pairing. Derived
sources are `shared/src/main/java/com/shilapi/xcertplay/airplay/{AirPlayCrypto,
Srp6a,PairSetup,PairVerify,MfiSapAuthSetup,ControlCipher,Tlv8Codec}.kt` and
`mfi/LocalMfiAuthenticationClient.kt`, under GPL-3.0-only.

- `LocalIdentity::load(directory)` reads only external `identity.pk8` and
  `certificate.p7b`, bounded to 16 KiB and 65,525 bytes respectively. It accepts P-256 PKCS#8 and one
  X.509 certificate, either raw DER or PKCS#7 SignedData. It signs the supplied
  SHA-256 digest directly as fixed 64-byte `r || s`, with no second hash.
- A successful self-check proves key/certificate consistency only. It does not
  verify an Apple trust chain, iPhone acceptance, or distribution rights.
- `AuthProvider` can be supplied by hardware/network integrations. MFiSAP
  supports version-2 SHA-1 and version-3 SHA-256 challenge construction and the
  BAA response format; this crate ships only the local P-256 provider.
- `AirPlayIdentity` is the separate Ed25519 pairing identity. Persist its seed
  using protected host storage. `PairingStore` commits controller public keys;
  the included memory implementation is intentionally not persistent.
- Create `PairSetup` / `PairVerify` per TCP session. Call `handle` with the RTSP
  TLV8 body and return its TLV8 bytes. Preserve the source's fixed setup PIN
  `3939`. After M4 pair-verify response, use the verified `ControlKeys` for
  control-channel encryption. `shared_secret` supports event/media derivations.
- `ControlReader` retains partial frames; `ControlWriter` splits plaintext at
  16 KiB. Counters never wrap, invalid tags poison the reader, and `finish`
  rejects truncated EOF. Keys use the accessory perspective (read = incoming).

The host enforces timeouts, connection ownership, and the encrypted transition.
MFiSAP may precede pair verification, as in the reference. No key material is logged here.
Pairing TLV parsing rejects truncation and ambiguous duplicate fields rather
than accepting partially parsed messages.

Run `cargo test -p carplay-auth`. Tests use ephemeral synthetic certificates,
RFC 7748/8032/8439 vectors, an independent big-integer SRP controller and full
setup/verify/encrypted-control exchanges. They do not claim physical iPhone
interoperability. `cargo run -p carplay-auth --example identity_check -- <dir>`
performs the bounded external-identity consistency check without printing keys.
