// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay airplay/{IapTunnel,AirPlayIapTunnelStream}.kt and
// transport/{Iap2LinkChannel,Iap2IdentificationClient,Iap2WirelessControlClient}.kt.
use anyhow::{Result, bail, ensure};
use carplay_auth::{AuthProvider, CertificateType, ControlReader};
use carplay_protocol::{
    iap2,
    tlv::{self, ControlMessage, Identification, Parameter},
};
use std::{
    collections::{BTreeSet, VecDeque},
    io::Read,
    net::TcpStream,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) const CLIENT_UUID: &str = "E9459FD0-BCAD-4C45-820F-1E72447EF2F2";
const MAX_PACKAGE: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Packages {
    bytes: Vec<u8>,
}
impl Packages {
    fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        let mut messages = Vec::new();
        while !bytes.is_empty() {
            let target = if self.bytes.len() < 32 {
                32
            } else {
                let length = u32::from_be_bytes(self.bytes[..4].try_into()?) as usize;
                ensure!(
                    (32..=MAX_PACKAGE).contains(&length),
                    "invalid APTransportPackage length"
                );
                length
            };
            let count = (target - self.bytes.len()).min(bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.bytes.len() < 32 {
                continue;
            }
            let length = u32::from_be_bytes(self.bytes[..4].try_into()?) as usize;
            ensure!(
                (32..=MAX_PACKAGE).contains(&length),
                "invalid APTransportPackage length"
            );
            if self.bytes.len() < length {
                continue;
            }
            if &self.bytes[16..20] == b"comm" {
                messages.push(self.bytes[32..].to_vec());
            }
            self.bytes.clear();
        }
        Ok(messages)
    }
    fn finish(&self) -> Result<()> {
        ensure!(self.bytes.is_empty(), "truncated APTransportPackage");
        Ok(())
    }
}

#[derive(Default)]
struct ControlState {
    identification_sent: bool,
    identified: bool,
    certificate_sent: bool,
    response_sent: bool,
    ready: bool,
    transport_notified: bool,
    pre_wifi_sent: u8,
    post_wifi_sent: u8,
}
impl ControlState {
    fn handle(
        &mut self,
        message: &ControlMessage,
        auth: &dyn AuthProvider,
        identity: &Identification,
        endpoint: Option<&carplay_wireless::Endpoint>,
    ) -> Result<Vec<ControlMessage>> {
        match message.message_id {
            tlv::START_IDENTIFICATION if !self.identified => {
                self.identification_sent = true;
                Ok(vec![identity.build()?])
            }
            tlv::IDENTIFICATION_ACCEPTED if self.identification_sent && !self.identified => {
                self.identified = true;
                Ok(vec![])
            }
            tlv::IDENTIFICATION_REJECTED => bail!("iPhone rejected tunnel identification"),
            tlv::REQUEST_CERTIFICATE if self.identified && !self.ready => {
                let message = match auth.certificate_type() {
                    CertificateType::Mfi => {
                        let certificate = auth.certificate()?;
                        ensure!(
                            !certificate.is_empty() && certificate.len() <= 65_525,
                            "invalid tunnel certificate length"
                        );
                        tlv::accessory_certificate(&certificate)?
                    }
                    CertificateType::Baa => {
                        let certs = auth.baa_certificates()?;
                        ensure!(
                            !certs.leaf.is_empty() && !certs.intermediate.is_empty(),
                            "empty tunnel BAA certificate"
                        );
                        ControlMessage::new(
                            tlv::CERTIFICATE,
                            &[
                                Parameter::new(0, certs.leaf),
                                Parameter::u8(1, 1),
                                Parameter::new(2, certs.intermediate),
                            ],
                        )?
                    }
                };
                self.certificate_sent = true;
                Ok(vec![message])
            }
            tlv::REQUEST_CHALLENGE_RESPONSE if self.certificate_sent && !self.ready => {
                let signature = auth.sign_challenge(&tlv::authentication_challenge(message)?)?;
                ensure!(
                    !signature.is_empty() && signature.len() <= 65_525,
                    "invalid tunnel signature length"
                );
                self.response_sent = true;
                Ok(vec![tlv::authentication_response(&signature)?])
            }
            tlv::AUTHENTICATION_SUCCEEDED if self.response_sent && !self.ready => {
                self.ready = true;
                Ok(tlv::subscriptions()?
                    .into_iter()
                    .filter(|message| identity.sent_messages.contains(&message.message_id))
                    .collect())
            }
            tlv::AUTHENTICATION_FAILED => bail!("iPhone rejected tunnel authentication"),
            0x5702 if self.ready => self.wifi(endpoint),
            0x4300 if self.ready => {
                let endpoint = endpoint
                    .ok_or_else(|| anyhow::anyhow!("runtime wireless endpoint not configured"))?;
                Ok(vec![endpoint.start_session()?])
            }
            0x4e0e if self.ready => {
                for p in message.parameters()? {
                    if p.id <= 1 {
                        p.as_str()?;
                    }
                }
                self.transport_notified = true;
                self.wifi(endpoint)
            }
            0x4e0d if self.ready => {
                let parameters = message.parameters()?;
                let available = parameters
                    .iter()
                    .find(|p| p.id == 0)
                    .ok_or_else(|| anyhow::anyhow!("missing wireless availability"))?
                    .as_u8()?;
                ensure!(available <= 1, "invalid wireless availability");
                Ok(vec![])
            }
            _ if self.ready => Ok(vec![]),
            _ => bail!("unexpected iAP tunnel control message before authentication"),
        }
    }
    fn wifi(
        &mut self,
        endpoint: Option<&carplay_wireless::Endpoint>,
    ) -> Result<Vec<ControlMessage>> {
        let endpoint =
            endpoint.ok_or_else(|| anyhow::anyhow!("runtime wireless endpoint not configured"))?;
        if endpoint.transport == carplay_wireless::EndpointTransport::Wired {
            return Ok(vec![]);
        }
        let (count, limit) = if self.transport_notified {
            (&mut self.post_wifi_sent, 2)
        } else {
            (&mut self.pre_wifi_sent, 5)
        };
        if *count >= limit {
            return Ok(vec![]);
        }
        let message = endpoint.wifi()?;
        *count += 1;
        Ok(vec![message])
    }
}

#[derive(Default)]
struct Pending {
    messages: VecDeque<Outbound>,
    bytes: usize,
}
struct Outbound {
    bytes: zeroize::Zeroizing<Vec<u8>>,
    offset: usize,
    session_id: u8,
}
impl Pending {
    fn push(&mut self, message: ControlMessage) -> Result<()> {
        self.push_session(iap2::CONTROL_SESSION, message.encode()?)
    }
    fn push_session(&mut self, session_id: u8, bytes: Vec<u8>) -> Result<()> {
        let bytes = zeroize::Zeroizing::new(bytes);
        ensure!(
            self.messages.len() < 64 && bytes.len() <= 1_048_576 - self.bytes,
            "iAP tunnel outgoing queue limit"
        );
        self.bytes += bytes.len();
        self.messages.push_back(Outbound {
            bytes,
            offset: 0,
            session_id,
        });
        Ok(())
    }
    fn flush(&mut self, engine: &mut iap2::LinkEngine, now: u64) -> Result<()> {
        while engine.writable() {
            let Some(front) = self.messages.front_mut() else {
                break;
            };
            let maximum = usize::from(
                engine
                    .peer_synchronization()
                    .ok_or_else(|| anyhow::anyhow!("iAP tunnel synchronization missing"))?
                    .max_length,
            ) - 10;
            ensure!(
                front.session_id == iap2::CONTROL_SESSION || front.bytes.len() <= maximum,
                "peer cannot carry file transfer acknowledgement"
            );
            let end = (front.offset + maximum).min(front.bytes.len());
            engine.send_session(front.session_id, &front.bytes[front.offset..end], now)?;
            front.offset = end;
            if end == front.bytes.len() {
                self.bytes -= front.bytes.len();
                self.messages.pop_front();
            }
        }
        Ok(())
    }
}

/// Receive encrypted APTransportPackage records over TCP. All outgoing iAP2
/// bytes go through the caller's `iAPSendMessage` event-channel callback.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    mut stream: TcpStream,
    key: [u8; 32],
    auth: &dyn AuthProvider,
    identity: &Identification,
    endpoint: Option<&carplay_wireless::Endpoint>,
    cancel: &AtomicBool,
    mut send: impl FnMut(Vec<u8>) -> Result<()>,
    mut on_message: impl FnMut(ControlMessage) -> Result<()>,
    mut on_unsupported_session: impl FnMut(u8, usize) -> Result<()>,
    mut on_artwork: impl FnMut(carplay_protocol::file_transfer::Artwork) -> Result<()>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut engine = iap2::LinkEngine::new(iap2::Config {
        max_outgoing: 4,
        control_session_version: 2,
        zero_acknowledgements: true,
        ..Default::default()
    })?;
    engine.start(true, 0);
    let origin = Instant::now();
    let mut cipher = ControlReader::new(key);
    let mut packages = Packages::default();
    let mut csm = tlv::ControlDecoder::default();
    let mut state = ControlState::default();
    let mut pending = Pending::default();
    let mut unsupported_sessions = BTreeSet::new();
    let mut files = carplay_protocol::file_transfer::FileTransferReceiver::default();
    let mut buffer = [0; 16384];
    while !cancel.load(Ordering::Acquire) {
        let now = u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
        ensure!(
            state.ready || now < 60_000,
            "iAP tunnel handshake timed out"
        );
        engine.advance_time(now);
        while let Some(event) = engine.poll_event() {
            match event {
                iap2::Event::Control(bytes) => {
                    for message in csm.offer(&bytes) {
                        for reply in state.handle(&message, auth, identity, endpoint)? {
                            pending.push(reply)?;
                        }
                        if state.ready {
                            on_message(message)?;
                        }
                    }
                }
                iap2::Event::Dead(_) => bail!("iAP tunnel link ended"),
                iap2::Event::Session { session_id, bytes }
                    if session_id == iap2::FILE_TRANSFER_SESSION && state.ready =>
                {
                    let result = files.accept(&bytes);
                    for reply in result.replies {
                        pending.push_session(session_id, reply)?;
                    }
                    if let Some(artwork) = result.completed {
                        on_artwork(artwork)?;
                    }
                }
                iap2::Event::Session { session_id, bytes } => {
                    // EA payloads are independent of the control session. Report
                    // unsupported data once without tearing down active CarPlay.
                    if unsupported_sessions.insert(session_id) {
                        on_unsupported_session(session_id, bytes.len())?;
                    }
                }
                iap2::Event::Writable(_) => {}
            }
        }
        pending.flush(&mut engine, now)?;
        let output = engine.take_output();
        if !output.is_empty() {
            send(output)?;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                cipher.finish()?;
                packages.finish()?;
                ensure!(csm.buffered_bytes() == 0, "truncated iAP control message");
                return Ok(());
            }
            Ok(n) => {
                for bytes in packages.feed(&cipher.decrypt(&buffer[..n])?)? {
                    engine.feed(&bytes, now);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0; 32];
        out[..4].copy_from_slice(&((32 + payload.len()) as u32).to_be_bytes());
        out[16..20].copy_from_slice(b"comm");
        out.extend_from_slice(payload);
        out
    }
    #[test]
    fn package_fragmentation_coalescing_and_limits() {
        let wire = [package(b"one"), package(b"two")].concat();
        let mut decoder = Packages::default();
        let mut output = vec![];
        for byte in &wire {
            output.extend(decoder.feed(&[*byte]).unwrap());
        }
        assert_eq!(output, [b"one".to_vec(), b"two".to_vec()]);
        decoder.finish().unwrap();
        let mut decoder = Packages::default();
        assert_eq!(decoder.feed(&wire).unwrap().len(), 2);
        for length in [0u32, 31, MAX_PACKAGE as u32 + 1] {
            let mut header = [0; 32];
            header[..4].copy_from_slice(&length.to_be_bytes());
            assert!(Packages::default().feed(&header).is_err());
        }
        let mut partial = Packages::default();
        partial.feed(&wire[..33]).unwrap();
        assert!(partial.finish().is_err());
    }

    #[test]
    fn baa_uses_leaf_and_intermediate_provider_contract() {
        struct Baa;
        impl AuthProvider for Baa {
            fn protocol_major(&self) -> u8 {
                3
            }
            fn certificate_type(&self) -> CertificateType {
                CertificateType::Baa
            }
            fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
                panic!("BAA must use its dedicated provider contract")
            }
            fn baa_certificates(&self) -> carplay_auth::Result<carplay_auth::BaaCertificates> {
                Ok(carplay_auth::BaaCertificates {
                    leaf: vec![1; 100],
                    intermediate: vec![2; 180],
                })
            }
            fn sign_challenge(&self, _: &[u8]) -> carplay_auth::Result<Vec<u8>> {
                Err(carplay_auth::AuthError::Authentication)
            }
        }
        let identity = Identification {
            name: "Test".into(),
            model: "Test".into(),
            manufacturer: "Test".into(),
            serial: "Test".into(),
            firmware_version: "1".into(),
            hardware_version: "1".into(),
            language: "en".into(),
            external_accessory_protocol: "test".into(),
            transport: tlv::IdentificationTransport::Wired { usb_interface: 0 },
            sent_messages: vec![],
            received_messages: vec![],
            extra_components: vec![],
        };
        let mut state = ControlState {
            identified: true,
            ..Default::default()
        };
        let reply = state
            .handle(
                &ControlMessage::empty(tlv::REQUEST_CERTIFICATE),
                &Baa,
                &identity,
                None,
            )
            .unwrap();
        assert_eq!(
            reply[0].parameters().unwrap(),
            vec![
                Parameter::new(0, vec![1; 100]),
                Parameter::u8(1, 1),
                Parameter::new(2, vec![2; 180])
            ]
        );
        assert!(state.certificate_sent);
    }
}
