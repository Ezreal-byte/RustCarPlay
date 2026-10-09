<!-- SPDX-License-Identifier: GPL-3.0-only -->
# carplay-wireless

RFCOMM iAP2 bootstrap ported from DiPlay commit `9e244d9` (`Iap2WirelessControlClient`, `Iap2IdentificationClient`, `Iap2MfiAuthenticationClient`).

The caller starts Wi-Fi, the AirPlay listener and mDNS first. Build an `Endpoint` from the actual SSID, passphrase, local IP addresses, listener port and receiver identity. Use a wireless `Identification` with the local Bluetooth adapter address and matching SSID; `SENT_MESSAGES` and `RECEIVED_MESSAGES` list this coordinator's bootstrap capabilities. Runtime location and vehicle declarations belong on the AirPlay iAP2 tunnel and are rejected on this Bluetooth link.

`Coordinator` is a deterministic state machine. Supply monotonic millisecond timestamps to `start`, `feed` and `advance_time`. Drain events regularly. Write all bytes from `take_output`, flush the transport, then call `confirm_output_written`; only that confirmation increments successful Wi-Fi/start-session send counts. An unconfirmed batch cannot be drained again. Stop the coordinator if a write fails. `next_deadline_ms` determines the next timer wakeup.

`run` owns a `Transport` and performs this loop, honoring an `AtomicBool` cancellation flag and closing the stream on every return path. Native RFCOMM and TCP streams implement `Transport`; custom streams must honor read/write timeouts. Run it on an I/O worker. Auth-provider methods and event callbacks must themselves be bounded. A callback can return `Action::Stop` to cancel.

The default deadline is 60 seconds. Continued control after that deadline requires the caller to provide proof of the current live AirPlay session as well as an authenticated bootstrap and a successfully written `0x4301`. Revoking the live-session proof ends the extension. An outgoing start command, authentication response or a simulated test is not evidence of successful phone display.

Authentication passes the challenge unchanged to `AuthProvider`; the local P-256 implementation signs the supplied digest. MFi and BAA certificate framing are supported. Identification acceptance is accepted only after an identification response, and authentication success only after a challenge has been signed. This is stricter than DiPlay's acceptance of standalone success messages. No trusted accessory private identity is bundled.

Tests use a byte-level phone peer with independent sequence/acknowledgement generation, constrained windows and 42-byte packet limits. They cover the full bootstrap order, large certificate fragmentation, retry limits, write confirmation, invalid challenges, privacy of debug output, deadlines and transport lifecycle. They do not claim iPhone compatibility or Apple trust.


The coordinator also accepts `EndpointTransport::Wired` for Linux CarKit streams.
Wired identification must match the endpoint type and advertise `0xAE03`.
It sends a conservative PowerSourceUpdate after authentication, keeps Wi-Fi
configuration out of wired replies, and builds the IPv6 wired group in `0x4301`.
This is shared protocol code; Linux USB and Windows driver interoperability must
still be tested on actual devices.
