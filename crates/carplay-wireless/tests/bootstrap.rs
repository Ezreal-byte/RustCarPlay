// SPDX-License-Identifier: GPL-3.0-only
// Scenarios ported from DiPlay 9e244d9 shared/src/test/java/com/shilapi/xcertplay/
// transport/{Iap2WirelessControlClientTest,Iap2WirelessLinkRoleTest}.kt.
// Peer simulator adds independent link framing, bounded-window and I/O lifecycle checks.
use carplay_auth::{AuthProvider, BaaCertificates, CertificateType};
use carplay_protocol::{
    iap2::{ACK, CONTROL_SESSION, Config, LinkPacket, MARKER, PacketDecoder, SYN},
    tlv::{
        self, ControlDecoder, ControlMessage, Identification, IdentificationTransport, Parameter,
    },
};
use carplay_wireless::*;
use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zeroize::Zeroizing;

struct FakeAuth {
    certificate: Vec<u8>,
    challenges: Mutex<Vec<Vec<u8>>>,
    baa: bool,
}
impl AuthProvider for FakeAuth {
    fn protocol_major(&self) -> u8 {
        3
    }
    fn certificate_type(&self) -> CertificateType {
        if self.baa {
            CertificateType::Baa
        } else {
            CertificateType::Mfi
        }
    }
    fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
        Ok(self.certificate.clone())
    }
    fn sign_challenge(&self, bytes: &[u8]) -> carplay_auth::Result<Vec<u8>> {
        self.challenges.lock().unwrap().push(bytes.to_vec());
        Ok(vec![0x91; 64])
    }
    fn baa_certificates(&self) -> carplay_auth::Result<BaaCertificates> {
        Ok(BaaCertificates {
            leaf: vec![0x11; 100],
            intermediate: vec![0x22; 180],
        })
    }
}
fn endpoint() -> Endpoint {
    Endpoint {
        transport: EndpointTransport::Wireless,
        ssid: "test-network".into(),
        passphrase: Zeroizing::new("test-password".into()),
        channel: 6,
        security_type: 2,
        ip_addresses: vec!["192.168.1.5".into(), "fe80::1".into()],
        airplay_port: 7000,
        device_identifier: "02:00:00:00:00:01".into(),
        public_key: "1a".repeat(32),
        source_version: "320.20".into(),
        access_point_bssid: None,
    }
}
fn identification() -> Identification {
    Identification {
        name: "Rust Receiver".into(),
        model: "Desktop1,1".into(),
        manufacturer: "OpenSource".into(),
        serial: "test-1".into(),
        firmware_version: "0.1".into(),
        hardware_version: "1".into(),
        language: "en".into(),
        external_accessory_protocol: "com.example.test".into(),
        transport: IdentificationTransport::Wireless {
            bluetooth_mac: [2, 0, 0, 0, 0, 1],
            ssid: "test-network".into(),
        },
        sent_messages: SENT_MESSAGES.to_vec(),
        received_messages: RECEIVED_MESSAGES.to_vec(),
        extra_components: vec![],
    }
}
fn auth(baa: bool) -> Arc<FakeAuth> {
    Arc::new(FakeAuth {
        certificate: vec![0x30; 4096],
        challenges: Mutex::new(vec![]),
        baa,
    })
}
fn coordinator(options: Options) -> Coordinator {
    Coordinator::new(endpoint(), identification(), auth(false), options).unwrap()
}

struct Phone {
    coordinator: Coordinator,
    seq: u8,
    ack: u8,
    now: u64,
    csm: ControlDecoder,
    messages: Vec<ControlMessage>,
    sessions: Vec<(u8, Vec<u8>)>,
    events: Vec<Event>,
}
impl Phone {
    fn new(max_frame: u16, baa: bool) -> (Self, Arc<FakeAuth>) {
        Self::with_endpoint(max_frame, baa, endpoint(), identification())
    }
    fn with_endpoint(
        max_frame: u16,
        baa: bool,
        endpoint: Endpoint,
        identification: Identification,
    ) -> (Self, Arc<FakeAuth>) {
        let provider = auth(baa);
        let mut coordinator = Coordinator::new(
            endpoint,
            identification,
            provider.clone(),
            Options::default(),
        )
        .unwrap();
        coordinator.start(0).unwrap();
        assert_eq!(coordinator.take_output().unwrap().as_slice(), MARKER);
        coordinator.confirm_output_written(0).unwrap();
        coordinator.feed(&MARKER, 1).unwrap();
        assert!(!coordinator.take_output().unwrap().is_empty());
        coordinator.confirm_output_written(1).unwrap();
        let mut sync = Config::default().synchronization();
        sync.max_length = max_frame;
        sync.max_outgoing = 2;
        let bytes = LinkPacket {
            control: SYN | ACK,
            sequence: 1,
            acknowledgement: 99,
            session_id: 0,
            payload: Some(sync.encode().unwrap()),
        }
        .encode()
        .unwrap();
        coordinator.feed(&bytes, 2).unwrap();
        let mut phone = Self {
            coordinator,
            seq: 1,
            ack: 99,
            now: 2,
            csm: ControlDecoder::default(),
            messages: vec![],
            sessions: vec![],
            events: vec![],
        };
        phone.drain().unwrap();
        assert_eq!(
            phone.coordinator.state(),
            carplay_wireless::State::Identifying
        );
        (phone, provider)
    }
    fn drain(&mut self) -> Result<()> {
        for _ in 0..5000 {
            while let Some(event) = self.coordinator.poll_event() {
                self.events.push(event);
            }
            let output = self.coordinator.take_output()?;
            if output.is_empty() {
                return Ok(());
            }
            self.coordinator.confirm_output_written(self.now)?;
            let mut decoder = PacketDecoder::default();
            decoder.push(&output)?;
            let mut data = false;
            while let Some(packet) = decoder.next_packet()? {
                if let Some(payload) = packet.payload {
                    if packet.session_id == CONTROL_SESSION {
                        self.messages.extend(self.csm.offer(&payload));
                    } else {
                        self.sessions.push((packet.session_id, payload));
                    }
                    self.ack = packet.sequence;
                    data = true;
                }
            }
            if data {
                self.now += 1;
                self.coordinator.feed(
                    &LinkPacket {
                        control: ACK,
                        sequence: self.seq,
                        acknowledgement: self.ack,
                        session_id: 0,
                        payload: None,
                    }
                    .encode()?,
                    self.now,
                )?;
            }
        }
        panic!("output failed to become quiescent")
    }
    fn send_without_draining(&mut self, message: ControlMessage) -> Result<()> {
        self.seq = self.seq.wrapping_add(1);
        self.now += 1;
        self.coordinator.feed(
            &LinkPacket {
                control: ACK,
                sequence: self.seq,
                acknowledgement: self.ack,
                session_id: CONTROL_SESSION,
                payload: Some(message.encode()?),
            }
            .encode()?,
            self.now,
        )
    }
    fn send(&mut self, message: ControlMessage) -> Result<()> {
        self.send_without_draining(message)?;
        self.drain()
    }
    fn send_session(&mut self, session_id: u8, bytes: Vec<u8>) -> Result<()> {
        self.seq = self.seq.wrapping_add(1);
        self.now += 1;
        self.coordinator.feed(
            &LinkPacket {
                control: ACK,
                sequence: self.seq,
                acknowledgement: self.ack,
                session_id,
                payload: Some(bytes),
            }
            .encode()?,
            self.now,
        )?;
        self.drain()
    }
    fn send_empty(&mut self, id: u16) -> Result<()> {
        self.send(ControlMessage::empty(id))
    }
    fn identify(&mut self) {
        self.send_empty(tlv::START_IDENTIFICATION).unwrap();
        self.send_empty(tlv::IDENTIFICATION_ACCEPTED).unwrap();
    }
    fn authenticate(&mut self) {
        self.identify();
        self.send_empty(tlv::REQUEST_CERTIFICATE).unwrap();
        self.send(
            ControlMessage::new(
                tlv::REQUEST_CHALLENGE_RESPONSE,
                &[Parameter::new(0, vec![0x33; 32])],
            )
            .unwrap(),
        )
        .unwrap();
        self.send_empty(tlv::AUTHENTICATION_SUCCEEDED).unwrap();
    }
}

#[test]
fn wired_bootstrap_authenticates_and_advertises_only_ncm_ipv6() {
    let mut endpoint = endpoint();
    endpoint.transport = EndpointTransport::Wired;
    endpoint.ssid.clear();
    endpoint.passphrase = Zeroizing::new(String::new());
    endpoint.ip_addresses = vec!["fe80::1234".into()];
    let mut identification = identification();
    identification.transport = IdentificationTransport::Wired { usb_interface: 2 };
    identification.sent_messages.push(0xae03);
    identification.sent_messages.retain(|id| *id != 0x5703);
    identification
        .received_messages
        .retain(|id| ![0x5702, 0x4e0d, 0x4e0e].contains(id));
    let (mut phone, provider) = Phone::with_endpoint(4096, false, endpoint, identification);
    phone.authenticate();
    assert_eq!(*provider.challenges.lock().unwrap(), vec![vec![0x33; 32]]);
    let ids: Vec<_> = phone
        .messages
        .iter()
        .map(|message| message.message_id)
        .collect();
    assert!(
        ids.iter().position(|id| *id == 0xae03).unwrap()
            < ids.iter().position(|id| *id == 0x5000).unwrap()
    );
    let power = phone
        .messages
        .iter()
        .find(|message| message.message_id == 0xae03)
        .unwrap();
    assert_eq!(power.parameters().unwrap(), vec![Parameter::u16(0, 0)]);
    // A spurious wireless request must not turn USB into a Wi-Fi reconfiguration.
    phone.send_empty(0x5702).unwrap();
    phone.send_empty(0x4e0e).unwrap();
    assert!(
        phone
            .messages
            .iter()
            .all(|message| message.message_id != 0x5703)
    );
    phone.send_empty(0x4300).unwrap();
    let session = phone.messages.last().unwrap();
    assert_eq!(session.message_id, 0x4301);
    let parameters = session.parameters().unwrap();
    assert!(!parameters.iter().any(|parameter| parameter.id == 1));
    let wired = parameters
        .iter()
        .find(|parameter| parameter.id == 0)
        .unwrap()
        .parameters()
        .unwrap();
    assert_eq!(wired, vec![Parameter::string(0, "fe80::1234").unwrap()]);
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 1);
    assert_eq!(phone.coordinator.progress().wifi_configurations_sent, 0);
}

#[test]
fn wired_endpoint_rejects_credentials_ipv4_and_mismatched_identification() {
    let mut endpoint = endpoint();
    endpoint.transport = EndpointTransport::Wired;
    assert!(endpoint.validate().is_err());
    endpoint.ssid.clear();
    endpoint.passphrase = Zeroizing::new(String::new());
    assert!(endpoint.validate().is_err());
    endpoint.ip_addresses = vec!["fe80::1234".into()];
    assert!(endpoint.validate().is_ok());
    assert!(endpoint.wifi().is_err());
    assert!(Coordinator::new(endpoint, identification(), auth(false), Options::default()).is_err());
}

#[test]
fn complete_bootstrap_preserves_challenge_and_endpoint_wire_fields() {
    let (mut phone, auth) = Phone::new(4096, false);
    phone.authenticate();
    assert_eq!(phone.coordinator.state(), carplay_wireless::State::Running);
    assert_eq!(*auth.challenges.lock().unwrap(), vec![vec![0x33; 32]]);
    let ids: Vec<_> = phone.messages.iter().map(|m| m.message_id).collect();
    assert_eq!(
        ids,
        vec![
            0x1d01, 0xaa01, 0xaa03, 0x5000, 0x5200, 0xae00, 0x4157, 0x4154
        ]
    );
    assert_eq!(
        phone.messages[1].parameters().unwrap()[0].value,
        vec![0x30; 4096]
    );
    assert_eq!(
        phone.messages[2].parameters().unwrap()[0].value,
        vec![0x91; 64]
    );
    phone.send_empty(0x5702).unwrap();
    let params = phone.messages.last().unwrap().parameters().unwrap();
    assert_eq!(
        params.iter().find(|p| p.id == 1).unwrap().as_str().unwrap(),
        "test-network"
    );
    assert_eq!(
        params.iter().find(|p| p.id == 2).unwrap().as_str().unwrap(),
        "test-password"
    );
    phone
        .send(ControlMessage::new(0x4e0d, &[Parameter::u8(0, 1)]).unwrap())
        .unwrap();
    phone
        .send(
            ControlMessage::new(
                0x4e0e,
                &[Parameter::string(0, "02:00:00:00:00:02").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    // Upstream sends a session even if optional availability metadata is malformed.
    phone
        .send(ControlMessage {
            message_id: 0x4300,
            body: vec![0xff],
        })
        .unwrap();
    let session = phone.messages.last().unwrap();
    assert_eq!(session.message_id, 0x4301);
    let params = session.parameters().unwrap();
    assert_eq!(
        params.iter().find(|p| p.id == 2).unwrap().as_u32().unwrap(),
        7000
    );
    let wireless = params
        .iter()
        .find(|p| p.id == 1)
        .unwrap()
        .parameters()
        .unwrap();
    assert_eq!(wireless.iter().filter(|p| p.id == 3).count(), 2);
    let progress = phone.coordinator.progress();
    assert!(
        progress.identified
            && progress.authenticated
            && progress.subscribed
            && progress.transport_notification_seen
            && progress.wireless_available_seen
    );
    assert_eq!(
        (
            progress.wifi_configurations_sent,
            progress.post_transport_wifi_configurations_sent,
            progress.start_sessions_sent
        ),
        (2, 1, 1)
    );
}

#[test]
fn small_negotiated_frames_fragment_certificate_and_identification_with_backpressure() {
    let (mut phone, _) = Phone::new(42, false);
    phone.authenticate();
    assert_eq!(
        phone
            .messages
            .iter()
            .find(|m| m.message_id == 0xaa01)
            .unwrap()
            .parameters()
            .unwrap()[0]
            .value
            .len(),
        4096
    );
    assert!(phone.coordinator.progress().subscribed);
}

#[test]
fn baa_certificate_preserves_reference_tlv_layout() {
    let (mut phone, _) = Phone::new(4096, true);
    phone.identify();
    phone.send_empty(0xaa00).unwrap();
    let params = phone.messages.last().unwrap().parameters().unwrap();
    assert_eq!(
        params,
        vec![
            Parameter::new(0, vec![0x11; 100]),
            Parameter::u8(1, 1),
            Parameter::new(2, vec![0x22; 180])
        ]
    );
}

#[test]
fn wifi_retry_limits_are_five_before_transport_and_two_after() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    for _ in 0..9 {
        phone.send_empty(0x5702).unwrap();
    }
    assert_eq!(phone.coordinator.progress().wifi_configurations_sent, 5);
    for _ in 0..5 {
        phone.send_empty(0x4e0e).unwrap();
        phone.send_empty(0x5702).unwrap();
    }
    assert_eq!(phone.coordinator.progress().wifi_configurations_sent, 7);
    assert_eq!(
        phone
            .coordinator
            .progress()
            .post_transport_wifi_configurations_sent,
        2
    );
}

#[test]
fn start_session_is_counted_only_after_the_complete_output_is_written() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    phone
        .send_without_draining(ControlMessage::empty(0x4300))
        .unwrap();
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 0);
    let output = phone.coordinator.take_output().unwrap();
    assert!(!output.is_empty());
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 0);
    assert!(phone.coordinator.take_output().is_err());
    phone.coordinator.confirm_output_written(phone.now).unwrap();
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 1);
}

#[test]
fn rejected_and_out_of_sequence_authentication_are_terminal() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.identify();
    assert!(matches!(
        phone.send_empty(0xaa04),
        Err(Error::AuthenticationRejected)
    ));
    assert_eq!(phone.coordinator.terminal(), Some(Terminal::Failed));
    let (mut phone, _) = Phone::new(4096, false);
    phone.identify();
    assert!(matches!(
        phone.send_empty(0xaa05),
        Err(Error::UnexpectedMessage { .. })
    ));
    assert!(!phone.coordinator.progress().authenticated);
    let (mut phone, _) = Phone::new(4096, false);
    assert!(matches!(
        phone.send_empty(0x1d03),
        Err(Error::IdentificationRejected { .. })
    ));
}

#[test]
fn identification_rejection_reports_only_sorted_parameter_ids() {
    let (mut phone, _) = Phone::new(4096, false);
    let rejection = ControlMessage::new(
        tlv::IDENTIFICATION_REJECTED,
        &[
            Parameter::new(24, b"private network".to_vec()),
            Parameter::new(6, b"private capability data".to_vec()),
            Parameter::empty(24),
        ],
    )
    .unwrap();
    let error = phone.send(rejection).unwrap_err();
    assert!(matches!(
        &error,
        Error::IdentificationRejected { parameter_ids } if parameter_ids == &[6, 24]
    ));
    let text = error.to_string();
    assert!(text.contains("0x0006") && text.contains("0x0018"));
    assert!(!text.contains("private"));
}

#[test]
fn bootstrap_declares_all_source_start_stop_pairs() {
    let identity = identification().build().unwrap();
    let parameters = identity.parameters().unwrap();
    let messages = &parameters.iter().find(|p| p.id == 6).unwrap().value;
    let declared: Vec<_> = messages
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(
        declared,
        [
            0xaa01, 0xaa03, 0x5000, 0x5002, 0x5200, 0x5203, 0xae00, 0xae02, 0x4157, 0x4159, 0x4154,
            0x4156, 0x4301, 0x5703,
        ]
    );
    for stop in tlv::stop_subscriptions() {
        assert!(declared.contains(&stop.message_id));
        assert!(stop.parameters().unwrap().is_empty());
        assert_eq!(stop.encode().unwrap().len(), 6);
    }
}

#[test]
fn artwork_datagrams_reply_without_breaking_bootstrap_control() {
    let (mut phone, _) = Phone::new(192, false);
    phone.authenticate();
    let mut setup = vec![0x81, 4];
    setup.extend_from_slice(&5u64.to_be_bytes());
    setup.extend_from_slice(&2u16.to_be_bytes());
    phone.send_session(12, setup.clone()).unwrap();
    assert_eq!(phone.sessions.pop().unwrap(), (12, vec![0x81, 1]));
    phone.send_session(12, vec![0x81, 0x80, 1, 2, 3]).unwrap();
    phone.send_session(12, vec![0x81, 0x40, 4, 5]).unwrap();
    assert_eq!(phone.sessions.pop().unwrap(), (12, vec![0x81, 5]));
    assert!(phone.events.iter().any(|event| matches!(
        event, Event::Artwork(artwork) if artwork.id == 0x81 && artwork.bytes == [1,2,3,4,5]
    )));
    setup[11] = 7;
    phone.send_session(12, setup).unwrap();
    assert_eq!(phone.sessions.pop().unwrap(), (12, vec![0x81, 2]));
    phone.send_empty(0x5702).unwrap();
    phone.send_empty(0x4300).unwrap();
    assert_eq!(phone.coordinator.progress().wifi_configurations_sent, 1);
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 1);
}

#[test]
fn empty_duplicate_and_oversized_challenges_never_reach_auth_provider() {
    for parameters in [
        vec![Parameter::new(0, vec![])],
        vec![Parameter::new(0, vec![1; 129])],
        vec![
            Parameter::new(0, vec![1; 32]),
            Parameter::new(0, vec![2; 32]),
        ],
    ] {
        let (mut phone, auth) = Phone::new(4096, false);
        phone.identify();
        phone.send_empty(0xaa00).unwrap();
        assert!(
            phone
                .send(ControlMessage::new(0xaa02, &parameters).unwrap())
                .is_err()
        );
        assert!(auth.challenges.lock().unwrap().is_empty());
    }
}

#[test]
fn metadata_is_forwarded_but_never_printed_and_availability_is_sticky() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    phone
        .send(
            ControlMessage::new(
                0x5001,
                &[Parameter::string(0, "private-song-title").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
    let event = phone
        .events
        .iter()
        .find(|e| matches!(e, Event::Incoming(_)))
        .unwrap();
    assert!(!format!("{event:?}").contains("private-song-title"));
    for value in [1, 0] {
        phone
            .send(ControlMessage::new(0x4e0d, &[Parameter::u8(0, value)]).unwrap())
            .unwrap();
    }
    assert!(phone.coordinator.progress().wireless_available_seen);
    assert_eq!(phone.coordinator.progress().forwarded_messages, 1);
    assert!(!format!("{:?}", endpoint()).contains("test-password"));
}

#[test]
fn invalid_wireless_boolean_is_rejected() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    assert!(
        phone
            .send(ControlMessage::new(0x4e0d, &[Parameter::u8(0, 2)]).unwrap())
            .is_err()
    );
}

#[test]
fn deadline_extension_needs_actual_start_and_current_live_session_proof() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    phone.coordinator.set_live_session(true);
    phone.coordinator.advance_time(60_000).unwrap();
    assert_eq!(phone.coordinator.terminal(), Some(Terminal::TimedOut));
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    phone.send_empty(0x4300).unwrap();
    phone.coordinator.set_live_session(true);
    phone.coordinator.advance_time(60_000).unwrap();
    assert_eq!(phone.coordinator.terminal(), None);
    phone.coordinator.set_live_session(false);
    phone.coordinator.advance_time(60_001).unwrap();
    assert_eq!(phone.coordinator.terminal(), Some(Terminal::TimedOut));
}

#[test]
fn stop_discards_queued_output_and_retains_only_terminal_event() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    phone
        .send_without_draining(ControlMessage::empty(0x4300))
        .unwrap();
    phone.coordinator.stop();
    assert!(phone.coordinator.take_output().unwrap().is_empty());
    assert_eq!(phone.coordinator.progress().start_sessions_sent, 0);
    assert!(matches!(
        phone.coordinator.poll_event(),
        Some(Event::Stopped(Terminal::Cancelled))
    ));
    assert!(phone.coordinator.poll_event().is_none());
}

#[test]
fn endpoint_and_bootstrap_capability_validation() {
    for change in 0..5 {
        let (mut endpoint, mut id) = (endpoint(), identification());
        match change {
            0 => endpoint.ip_addresses = vec!["fe80::1%eth0".into()],
            1 => endpoint.airplay_port = 0,
            2 => endpoint.security_type = 9,
            3 => id.sent_messages.push(0xa101),
            _ => id.received_messages.push(0xfffa),
        }
        assert!(Coordinator::new(endpoint, id, auth(false), Options::default()).is_err());
    }
}

#[test]
fn undrained_event_flood_fails_closed_with_a_terminal_event() {
    let (mut phone, _) = Phone::new(4096, false);
    phone.authenticate();
    let mut failure = None;
    for _ in 0..300 {
        if let Err(error) = phone.send_without_draining(ControlMessage::empty(0x5001)) {
            failure = Some(error);
            break;
        }
    }
    assert!(matches!(failure, Some(Error::QueueLimit)));
    assert_eq!(phone.coordinator.terminal(), Some(Terminal::Failed));
    assert!(matches!(
        phone.coordinator.poll_event(),
        Some(Event::Stopped(Terminal::Failed))
    ));
    assert!(phone.coordinator.take_output().unwrap().is_empty());
}

#[test]
fn protocol_session_debug_does_not_expose_wifi_credentials() {
    let session = tlv::WirelessSession {
        ssid: "sensitive-network".into(),
        passphrase: "sensitive-secret".into(),
        channel: 6,
        ip_addresses: vec!["192.168.1.1".into()],
        security_type: 2,
    };
    let debug = format!("{session:?}");
    assert!(!debug.contains("sensitive-secret"));
    assert!(!debug.contains("sensitive-network"));
}

struct FakeStream {
    closed: Arc<AtomicBool>,
    written: Arc<Mutex<Vec<u8>>>,
    eof: bool,
}
impl Read for FakeStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if self.eof {
            Ok(0)
        } else {
            std::thread::sleep(Duration::from_millis(1));
            Err(io::ErrorKind::TimedOut.into())
        }
    }
}
impl Write for FakeStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.written.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Transport for FakeStream {
    fn set_io_timeout(&self, timeout: Duration) -> io::Result<()> {
        assert!(!timeout.is_zero() && timeout <= Duration::from_millis(100));
        Ok(())
    }
    fn close(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}
#[test]
fn blocking_runner_closes_on_cancel_eof_and_timeout() {
    for (expected, cancel, eof) in [
        (Terminal::Cancelled, true, false),
        (Terminal::ChannelClosed, false, true),
        (Terminal::TimedOut, false, false),
    ] {
        let closed = Arc::new(AtomicBool::new(false));
        let written = Arc::new(Mutex::new(Vec::new()));
        let stream = FakeStream {
            closed: closed.clone(),
            written: written.clone(),
            eof,
        };
        let options = Options {
            timeout: Duration::from_millis(5),
            ..Default::default()
        };
        let outcome = run(
            stream,
            coordinator(options),
            &AtomicBool::new(cancel),
            || false,
            |_| Action::Continue,
        )
        .unwrap();
        assert_eq!(outcome.terminal, expected);
        assert!(closed.load(Ordering::Acquire));
        if cancel {
            assert!(written.lock().unwrap().is_empty());
        } else {
            assert_eq!(*written.lock().unwrap(), MARKER);
        }
    }
}

#[test]
fn handoff_cancels_idle_real_transport_without_waiting_for_incoming_events() {
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    for _ in 0..3 {
        let stream = TcpStream::connect(address).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let application_stop = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(AtomicBool::new(false));
        let handed_off = handoff.clone();
        let stopped = application_stop.clone();
        let worker = std::thread::spawn(move || {
            run_with_cancel(
                stream,
                coordinator(Options::default()),
                || stopped.load(Ordering::Acquire) || handed_off.load(Ordering::Acquire),
                || false,
                |_| Action::Continue,
            )
        });
        let mut marker = vec![0; MARKER.len()];
        peer.read_exact(&mut marker).unwrap();
        assert_eq!(marker, MARKER);
        let started = std::time::Instant::now();
        handoff.store(true, Ordering::Release);
        assert_eq!(
            worker.join().unwrap().unwrap().terminal,
            Terminal::Cancelled
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert!(!application_stop.load(Ordering::Acquire));
    }
}
