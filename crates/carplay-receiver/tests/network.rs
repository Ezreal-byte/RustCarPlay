// SPDX-License-Identifier: GPL-3.0-only
//! Real localhost TCP/UDP tests with a synthetic already-paired controller.
use carplay_auth::{
    AirPlayIdentity, AuthProvider, ControlReader, ControlWriter, PairingStore, crypto, tlv,
};
use carplay_core::{
    config::ReceiverConfig,
    media::{MediaEvent, MediaSink},
    rtsp,
};
use carplay_receiver::{
    ReceiverEvent, ReceiverHandle, ReceiverOptions,
    info::{self, array, dict, number, text},
    start,
    storage::FilePairingStore,
};
use plist::Value;
use std::{
    io::{Read, Write},
    net::{TcpStream, UdpSocket},
    sync::{Arc, mpsc},
    time::Duration,
};

struct NoAuth;
impl AuthProvider for NoAuth {
    fn protocol_major(&self) -> u8 {
        3
    }
    fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
        Ok(b"synthetic".to_vec())
    }
    fn sign_challenge(&self, _: &[u8]) -> carplay_auth::Result<Vec<u8>> {
        Err(carplay_auth::AuthError::Authentication)
    }
}
struct Sink(mpsc::SyncSender<MediaEvent>);
impl MediaSink for Sink {
    fn send(&self, event: MediaEvent) -> Result<(), String> {
        self.0.try_send(event).map_err(|error| error.to_string())
    }
}
struct Peer {
    socket: TcpStream,
    parser: rtsp::Decoder,
    reader: Option<ControlReader>,
    writer: Option<ControlWriter>,
    sequence: u64,
}
impl Peer {
    fn connect(address: std::net::SocketAddr) -> Self {
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        Self {
            socket,
            parser: Default::default(),
            reader: None,
            writer: None,
            sequence: 0,
        }
    }
    fn send_wire(&mut self, wire: &[u8]) {
        let wire = self
            .writer
            .as_mut()
            .map_or_else(|| wire.to_vec(), |writer| writer.encrypt(wire).unwrap());
        // Exercise partial records and request headers on real sockets.
        for fragment in wire.chunks(31) {
            self.socket.write_all(fragment).unwrap();
        }
    }
    fn receive(&mut self) -> rtsp::Request {
        loop {
            if let Some(message) = self.parser.next_request().unwrap() {
                return message;
            }
            let mut buffer = [0u8; 137];
            let n = self.socket.read(&mut buffer).unwrap();
            assert_ne!(n, 0, "unexpected EOF");
            let decoded = self.reader.as_mut().map_or_else(
                || buffer[..n].to_vec(),
                |reader| reader.decrypt(&buffer[..n]).unwrap(),
            );
            self.parser.push(&decoded).unwrap();
        }
    }
    fn request(&mut self, method: &str, path: &str, body: &[u8]) -> rtsp::Request {
        self.sequence += 1;
        let header = format!(
            "{method} {path} RTSP/1.0\r\nCSeq: {}\r\nContent-Length: {}\r\n\r\n",
            self.sequence,
            body.len()
        );
        self.send_wire(&[header.as_bytes(), body].concat());
        let response = self.receive();
        assert_eq!(
            response.headers.get("cseq").unwrap(),
            &self.sequence.to_string()
        );
        response
    }
    fn plist(&mut self, method: &str, body: Value) -> rtsp::Request {
        self.request(method, "/session", &info::encode(&body).unwrap())
    }
}

fn verified() -> (
    tempfile::TempDir,
    ReceiverHandle,
    Peer,
    [u8; 32],
    mpsc::Receiver<MediaEvent>,
) {
    verified_with_iap(false)
}

type Session = (
    tempfile::TempDir,
    ReceiverHandle,
    Peer,
    [u8; 32],
    mpsc::Receiver<MediaEvent>,
);

struct TestAuth;
impl AuthProvider for TestAuth {
    fn protocol_major(&self) -> u8 {
        3
    }
    fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
        Ok(vec![0x30; 4096])
    }
    fn sign_challenge(&self, challenge: &[u8]) -> carplay_auth::Result<Vec<u8>> {
        assert_eq!(challenge, &[0x33; 32]);
        Ok(vec![0x91; 64])
    }
}

fn verified_with_iap(iap: bool) -> Session {
    verified_with_options(iap, None, false)
}

fn verified_with_options(
    iap: bool,
    capture: Option<Arc<dyn carplay_core::media::CaptureFactory>>,
    microphone: bool,
) -> Session {
    let directory = tempfile::tempdir().unwrap();
    let (accessory, store) = FilePairingStore::open(directory.path()).unwrap();
    let controller = AirPlayIdentity::generate().unwrap();
    store
        .save(&controller.pairing_id, controller.public_key())
        .unwrap();
    drop(store);
    let (sender, events) = mpsc::sync_channel(128);
    let options = ReceiverOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        config: ReceiverConfig {
            microphone,
            ..ReceiverConfig::default()
        },
        bluetooth_address: "02:00:00:00:00:01".into(),
        state_dir: directory.path().into(),
        advertise: false,
    };
    let server = if iap {
        use carplay_protocol::tlv::{Identification, IdentificationTransport};
        carplay_receiver::start_with_bootstrap(
            options,
            Arc::new(TestAuth),
            Arc::new(Sink(sender)),
            Identification {
                name: "Test Receiver".into(),
                model: "Test".into(),
                manufacturer: "Test".into(),
                serial: "test-1".into(),
                firmware_version: "1".into(),
                hardware_version: "1".into(),
                language: "en".into(),
                external_accessory_protocol: "com.example.test".into(),
                transport: IdentificationTransport::Wireless {
                    bluetooth_mac: [2, 0, 0, 0, 0, 1],
                    ssid: "test-network".into(),
                },
                sent_messages: carplay_wireless::SENT_MESSAGES.to_vec(),
                received_messages: carplay_wireless::RECEIVED_MESSAGES.to_vec(),
                extra_components: vec![],
            },
            carplay_wireless::Endpoint {
                transport: carplay_wireless::EndpointTransport::Wireless,
                ssid: "test-network".into(),
                passphrase: zeroize::Zeroizing::new("test-password".into()),
                channel: 6,
                security_type: 2,
                ip_addresses: vec!["127.0.0.1".into()],
                airplay_port: 0,
                device_identifier: String::new(),
                public_key: String::new(),
                source_version: String::new(),
                access_point_bssid: None,
            },
        )
        .unwrap()
    } else if capture.is_some() {
        carplay_receiver::start_with_runtime(
            options,
            Arc::new(NoAuth),
            Arc::new(Sink(sender)),
            carplay_receiver::RuntimeOptions {
                capture,
                ..Default::default()
            },
        )
        .unwrap()
    } else {
        start(options, Arc::new(NoAuth), Arc::new(Sink(sender))).unwrap()
    };
    let mut peer = Peer::connect(server.address);
    let (private, public) = crypto::x25519_generate().unwrap();
    let response = peer.request(
        "POST",
        "/pair-verify",
        &tlv::encode(&[(6, &[1]), (3, &public)]),
    );
    assert_eq!(response.path, "200");
    let m2 = tlv::decode(&response.body).unwrap();
    assert_eq!(m2[&6], [2]);
    let accessory_public = m2[&3].as_slice().try_into().unwrap();
    let shared = crypto::x25519_shared(&private, &accessory_public).unwrap();
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
        &accessory.public_key(),
        &[accessory_public.as_slice(), &sub[&1], &public].concat(),
        &sub[&10]
    ));
    let signature = controller.sign(
        &[
            public.as_slice(),
            controller.pairing_id.as_bytes(),
            &accessory_public,
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
    let m4 = peer.request(
        "POST",
        "/pair-verify",
        &tlv::encode(&[(6, &[3]), (5, &sealed)]),
    );
    assert_eq!(m4.body, tlv::encode(&[(6, &[4])]));
    peer.reader = Some(ControlReader::new(
        *crypto::derive_key(
            shared.as_ref(),
            b"Control-Salt",
            b"Control-Read-Encryption-Key",
        )
        .unwrap(),
    ));
    peer.writer = Some(ControlWriter::new(
        *crypto::derive_key(
            shared.as_ref(),
            b"Control-Salt",
            b"Control-Write-Encryption-Key",
        )
        .unwrap(),
    ));
    (directory, server, peer, *shared, events)
}
fn response_plist(response: &rtsp::Request) -> Value {
    assert_eq!(response.path, "200");
    Value::from_reader(std::io::Cursor::new(&response.body)).unwrap()
}

#[test]
fn paired_controller_encrypted_info_setup_two_audio_types_and_partial_teardown() {
    let (_directory, mut server, mut peer, shared, media) = verified();
    let info = response_plist(&peer.request("GET", "/info", &[]));
    assert_eq!(
        info.as_dictionary().unwrap()["name"].as_string(),
        Some("RustCarPlay")
    );
    let session = response_plist(&peer.plist("SETUP", dict([])));
    let event_port = session.as_dictionary().unwrap()["eventPort"]
        .as_unsigned_integer()
        .unwrap();
    assert_ne!(event_port, 0);
    let specs = [("media", 101u64), ("alert", 102u64)].map(|(kind, id)| {
        dict([
            ("type", number(100)),
            ("audioType", text(kind)),
            ("audioFormat", number(0x8000)),
            ("streamConnectionID", number(id)),
        ])
    });
    let setup = response_plist(&peer.plist("SETUP", dict([("streams", array(specs))])));
    let streams = setup.as_dictionary().unwrap()["streams"]
        .as_array()
        .unwrap();
    assert_eq!(streams.len(), 2);
    for expected in ["media", "alert"] {
        assert!(matches!(
            media.recv_timeout(Duration::from_secs(3)).unwrap(),
            MediaEvent::AudioConfig { stream: 100, audio_type, .. } if audio_type == expected
        ));
    }
    for stream in streams {
        let d = stream.as_dictionary().unwrap();
        let id = d["streamConnectionID"].as_unsigned_integer().unwrap();
        let port = d["dataPort"].as_unsigned_integer().unwrap() as u16;
        let key = crypto::derive_key(
            &shared,
            format!("DataStream-Salt{id}").as_bytes(),
            b"DataStream-Output-Encryption-Key",
        )
        .unwrap();
        let mut wire = vec![0x80, 100, 0, 1, 0, 0, 0, 50, 1, 2, 3, 4];
        let payload =
            crypto::chacha_seal(&key, &crypto::nonce64(0), &[0, 1, 0, 2], &wire[4..12]).unwrap();
        wire.extend(payload);
        wire.extend(0u64.to_le_bytes());
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .send_to(&wire, ("127.0.0.1", port))
            .unwrap();
    }
    let mut kinds = Vec::new();
    for _ in 0..2 {
        match media.recv_timeout(Duration::from_secs(3)).unwrap() {
            MediaEvent::Audio {
                stream,
                audio_type,
                data,
                ..
            } => {
                assert_eq!(stream, 100);
                assert_eq!(data, [0, 1, 0, 2]);
                kinds.push(audio_type);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    kinds.sort();
    assert_eq!(kinds, ["alert", "media"]);
    assert_eq!(
        peer.plist(
            "TEARDOWN",
            dict([("streams", array([dict([("type", number(100))])]))])
        )
        .path,
        "200"
    );
    for _ in 0..2 {
        assert!(matches!(
            media.recv_timeout(Duration::from_secs(3)).unwrap(),
            MediaEvent::Stop { stream: 100 }
        ));
    }
    assert_eq!(peer.request("GET", "/info", &[]).path, "200");
    assert_eq!(peer.request("TEARDOWN", "/session", &[]).path, "200");
    server.stop();
}

#[test]
fn setup_rejects_entire_invalid_batch_without_leaking_a_stream() {
    let (_directory, mut server, mut peer, _shared, _media) = verified();
    let valid = dict([("type", number(110)), ("streamConnectionID", number(77))]);
    let bad = dict([("type", number(999)), ("streamConnectionID", number(78))]);
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([valid.clone(), bad]))]))
            .path,
        "501"
    );
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([valid.clone()]))]))
            .path,
        "200"
    );
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([valid]))]))
            .path,
        "455"
    );
    assert_eq!(
        peer.plist("TEARDOWN", dict([("streams", array([number(110)]))]))
            .path,
        "200"
    );
    let unsupported = info::encode(&dict([("type", text("unsupported-command"))])).unwrap();
    assert_eq!(peer.request("POST", "/command", &unsupported).path, "501");
    assert_eq!(peer.request("GET", "/info", &[]).path, "200");
    server.stop();
}

#[test]
fn encrypted_event_channel_handles_multiword_status_and_real_ui_request() {
    let (_directory, mut server, mut peer, shared, _media) = verified();
    let setup = response_plist(&peer.plist("SETUP", dict([])));
    let port = setup.as_dictionary().unwrap()["eventPort"]
        .as_unsigned_integer()
        .unwrap() as u16;
    let mut event = Peer::connect(([127, 0, 0, 1], port).into());
    event.reader = Some(ControlReader::new(
        *crypto::derive_key(&shared, b"Events-Salt", b"Events-Write-Encryption-Key").unwrap(),
    ));
    event.writer = Some(ControlWriter::new(
        *crypto::derive_key(&shared, b"Events-Salt", b"Events-Read-Encryption-Key").unwrap(),
    ));
    // Send first to synchronize acceptance without a timing-dependent sleep.
    let request = info::encode(&dict([
        ("type", text("requestUI")),
        ("params", dict([("url", text("carplay:"))])),
    ]))
    .unwrap();
    assert_eq!(event.request("POST", "/command", &request).path, "200");
    assert!(server.night(true));
    let command = event.receive();
    assert_eq!(command.method, "POST");
    let parsed = Value::from_reader(std::io::Cursor::new(command.body)).unwrap();
    assert_eq!(
        parsed.as_dictionary().unwrap()["type"].as_string(),
        Some("setNightMode")
    );
    // Preserve both edges and the release position over the real encrypted wire.
    for down in [true, false] {
        let report = carplay_core::input::touch_report(
            &[carplay_core::input::Contact {
                x: 0.25,
                y: 0.75,
                down,
            }],
            1280,
            720,
            carplay_core::input::Rotation::None,
        );
        assert!(server.hid(carplay_core::input::TOUCH_UID, report.to_vec()));
    }
    for (index, down) in [1, 0].into_iter().enumerate() {
        let command = event.receive();
        assert_eq!(command.headers["cseq"], (index + 2).to_string());
        let parsed = Value::from_reader(std::io::Cursor::new(command.body)).unwrap();
        let values = parsed.as_dictionary().unwrap();
        assert_eq!(values["type"].as_string(), Some("hidSendReport"));
        assert_eq!(values["uuid"].as_string(), Some("2a2a2a2a"));
        assert_eq!(
            values["hidReport"].as_data().unwrap(),
            &[0, down, 64, 1, 28, 2, 1, 0, 0, 0, 0, 0]
        );
    }
    event.send_wire(
        b"RTSP/1.0 455 Method Not Valid in This State\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n",
    );
    assert_eq!(event.request("POST", "/command", &request).path, "200");
    let observed: Vec<_> = server.events.try_iter().collect();
    assert!(
        observed
            .iter()
            .any(|event| matches!(event,ReceiverEvent::UiRequested(url) if url=="carplay:"))
    );
    assert!(
        !observed
            .iter()
            .any(|event| matches!(event, ReceiverEvent::Error(_)))
    );
    server.stop();
}

struct TunnelPhone {
    event: Peer,
    socket: TcpStream,
    cipher: ControlWriter,
    sequence: u8,
    acknowledgement: u8,
    csm: carplay_protocol::tlv::ControlDecoder,
    messages: std::collections::VecDeque<carplay_protocol::tlv::ControlMessage>,
    sessions: std::collections::VecDeque<(u8, Vec<u8>)>,
}
impl TunnelPhone {
    fn wire(&mut self, wire: &[u8]) {
        let mut package = vec![0u8; 32];
        package[..4].copy_from_slice(&((32 + wire.len()) as u32).to_be_bytes());
        package[16..20].copy_from_slice(b"comm");
        package.extend_from_slice(wire);
        for fragment in self.cipher.encrypt(&package).unwrap().chunks(23) {
            self.socket.write_all(fragment).unwrap();
        }
    }
    fn send(&mut self, message: carplay_protocol::tlv::ControlMessage) {
        self.send_session(
            carplay_protocol::iap2::CONTROL_SESSION,
            message.encode().unwrap(),
        );
    }
    fn send_session(&mut self, session_id: u8, bytes: Vec<u8>) {
        use carplay_protocol::iap2::{ACK, LinkPacket};
        self.sequence = self.sequence.wrapping_add(1);
        self.wire(
            &LinkPacket {
                control: ACK,
                sequence: self.sequence,
                acknowledgement: self.acknowledgement,
                session_id,
                payload: Some(bytes),
            }
            .encode()
            .unwrap(),
        );
    }
    fn receive_packets(&mut self) -> Vec<carplay_protocol::iap2::LinkPacket> {
        use carplay_protocol::iap2::PacketDecoder;
        let command = self.event.receive();
        assert_eq!(command.method, "POST");
        let body = Value::from_reader(std::io::Cursor::new(&command.body)).unwrap();
        let command_body = body.as_dictionary().unwrap();
        assert_eq!(command_body["type"].as_string(), Some("iAPSendMessage"));
        let data = command_body["params"].as_dictionary().unwrap()["data"]
            .as_data()
            .unwrap();
        let mut parser = PacketDecoder::default();
        parser.push(data).unwrap();
        let mut packets = Vec::new();
        while let Some(packet) = parser.next_packet().unwrap() {
            packets.push(packet);
        }
        self.event
            .send_wire(&rtsp::response(&command, 200, None, &[]));
        packets
    }
    fn receive(&mut self, id: u16) -> carplay_protocol::tlv::ControlMessage {
        loop {
            if let Some(message) = self.messages.pop_front() {
                assert_eq!(message.message_id, id);
                return message;
            }
            self.pump();
        }
    }
    fn receive_session(&mut self, id: u8) -> Vec<u8> {
        loop {
            if let Some((session, bytes)) = self.sessions.pop_front() {
                assert_eq!(session, id);
                return bytes;
            }
            self.pump();
        }
    }
    fn pump(&mut self) {
        use carplay_protocol::iap2::{ACK, CONTROL_SESSION, LinkPacket};
        for packet in self.receive_packets() {
            if let Some(payload) = packet.payload {
                assert!(
                    payload.len() + 10 <= 192,
                    "negotiated maximum frame exceeded"
                );
                self.acknowledgement = packet.sequence;
                if packet.session_id == CONTROL_SESSION {
                    self.messages.extend(self.csm.offer(&payload));
                } else {
                    self.sessions.push_back((packet.session_id, payload));
                }
                self.wire(
                    &LinkPacket {
                        control: ACK,
                        sequence: self.sequence,
                        acknowledgement: self.acknowledgement,
                        session_id: 0,
                        payload: None,
                    }
                    .encode()
                    .unwrap(),
                );
            }
        }
    }
}

#[test]
fn type130_encrypted_tunnel_authenticates_fragments_and_uses_actual_endpoint() {
    use carplay_protocol::{
        iap2::{ACK, Config, LinkPacket, SYN},
        tlv::{self as iap, ControlMessage, Parameter},
    };
    let (_directory, mut server, mut peer, shared, _media) = verified_with_iap(true);
    let setup = response_plist(&peer.plist("SETUP", dict([])));
    let event_port = setup.as_dictionary().unwrap()["eventPort"]
        .as_unsigned_integer()
        .unwrap() as u16;
    let mut event = Peer::connect(([127, 0, 0, 1], event_port).into());
    event.reader = Some(ControlReader::new(
        *crypto::derive_key(&shared, b"Events-Salt", b"Events-Write-Encryption-Key").unwrap(),
    ));
    event.writer = Some(ControlWriter::new(
        *crypto::derive_key(&shared, b"Events-Salt", b"Events-Read-Encryption-Key").unwrap(),
    ));
    assert_eq!(
        event
            .request(
                "POST",
                "/command",
                &info::encode(&dict([("type", text("requestUI"))])).unwrap()
            )
            .path,
        "200"
    );
    let streams = response_plist(&peer.plist(
        "SETUP",
        dict([(
            "streams",
            array([dict([
                ("type", number(130)),
                ("seed", number(u64::MAX)),
                ("streamConnectionID", number(42)),
                (
                    "clientTypeUUID",
                    text("E9459FD0-BCAD-4C45-820F-1E72447EF2F2"),
                ),
            ])]),
        )]),
    ));
    let stream = streams.as_dictionary().unwrap()["streams"]
        .as_array()
        .unwrap()[0]
        .as_dictionary()
        .unwrap();
    assert_eq!(stream["streamID"].as_unsigned_integer(), Some(1));
    assert_eq!(stream["streamConnectionID"].as_unsigned_integer(), Some(42));
    let port = stream["dataPort"].as_unsigned_integer().unwrap() as u16;
    let socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut phone = TunnelPhone {
        event,
        socket,
        cipher: ControlWriter::new(
            *crypto::derive_key(
                &shared,
                b"DataStream-Salt18446744073709551615",
                b"DataStream-Output-Encryption-Key",
            )
            .unwrap(),
        ),
        sequence: 1,
        acknowledgement: 0,
        csm: Default::default(),
        messages: Default::default(),
        sessions: Default::default(),
    };
    let syn = phone
        .receive_packets()
        .into_iter()
        .find(|packet| packet.control & SYN != 0)
        .unwrap();
    assert_eq!(syn.control, SYN);
    let mut sync = Config::default().synchronization();
    sync.max_length = 192;
    sync.max_outgoing = 2;
    phone.acknowledgement = syn.sequence;
    phone.wire(
        &LinkPacket {
            control: SYN | ACK,
            sequence: 1,
            acknowledgement: syn.sequence,
            session_id: 0,
            payload: Some(sync.encode().unwrap()),
        }
        .encode()
        .unwrap(),
    );
    phone.send(ControlMessage::empty(iap::START_IDENTIFICATION));
    phone.receive(0x1d01);
    phone.send(ControlMessage::empty(iap::IDENTIFICATION_ACCEPTED));
    phone.send(ControlMessage::empty(iap::REQUEST_CERTIFICATE));
    let certificate = phone.receive(iap::CERTIFICATE);
    assert_eq!(certificate.parameters().unwrap()[0].value, vec![0x30; 4096]);
    phone.send(
        ControlMessage::new(
            iap::REQUEST_CHALLENGE_RESPONSE,
            &[Parameter::new(0, vec![0x33; 32])],
        )
        .unwrap(),
    );
    assert_eq!(
        phone.receive(iap::CHALLENGE_RESPONSE).parameters().unwrap()[0].value,
        vec![0x91; 64]
    );
    phone.send(ControlMessage::empty(iap::AUTHENTICATION_SUCCEEDED));
    for id in [0x5000, 0x5200, 0xae00, 0x4157, 0x4154] {
        phone.receive(id);
    }
    let setup_art = [vec![7, 4], 5u64.to_be_bytes().to_vec(), vec![0, 2]].concat();
    phone.send_session(12, setup_art);
    assert_eq!(phone.receive_session(12), [7, 1]);
    phone.send_session(12, vec![7, 0x80, 1, 2]);
    phone.send_session(12, vec![7, 0x40, 3, 4, 5]);
    assert_eq!(phone.receive_session(12), [7, 5]);
    phone.send_session(
        12,
        [vec![8, 4], 5u64.to_be_bytes().to_vec(), vec![0, 3]].concat(),
    );
    assert_eq!(phone.receive_session(12), [8, 2]);
    phone.send_session(11, vec![1, 2, 3]);
    phone.send(ControlMessage::empty(0x5702));
    let wifi = phone.receive(0x5703).parameters().unwrap();
    assert_eq!(
        wifi.iter().find(|p| p.id == 1).unwrap().as_str().unwrap(),
        "test-network"
    );
    assert_eq!(
        wifi.iter().find(|p| p.id == 2).unwrap().as_str().unwrap(),
        "test-password"
    );
    phone.send(ControlMessage::empty(0x4300));
    let session = phone.receive(0x4301).parameters().unwrap();
    assert_eq!(
        session
            .iter()
            .find(|p| p.id == 2)
            .unwrap()
            .as_u32()
            .unwrap(),
        u32::from(server.address.port())
    );
    assert_eq!(
        session
            .iter()
            .find(|p| p.id == 5)
            .unwrap()
            .as_str()
            .unwrap(),
        carplay_core::SOURCE_VERSION
    );
    assert_eq!(
        session
            .iter()
            .find(|p| p.id == 3)
            .unwrap()
            .as_str()
            .unwrap(),
        "02:00:00:00:00:01"
    );
    phone.send(ControlMessage::new(0x4e0e, &[]).unwrap());
    phone.receive(0x5703);
    assert_eq!(
        peer.plist("TEARDOWN", dict([("streams", array([number(130)]))]))
            .path,
        "200"
    );
    assert_eq!(peer.request("GET", "/info", &[]).path, "200");
    let events: Vec<_> = server.events.try_iter().collect();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ReceiverEvent::Error(_)))
    );
    assert!(events.iter().any(
        |event| matches!(event, ReceiverEvent::IapArtwork { id: 7, data } if data == &[1,2,3,4,5])
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        ReceiverEvent::UnsupportedIapSession {
            session_id: 11,
            bytes: 3
        }
    )));
    server.stop();
}

#[derive(Default)]
struct TestCapture {
    starts: Arc<std::sync::atomic::AtomicUsize>,
    stops: Arc<std::sync::atomic::AtomicUsize>,
}
struct TestCaptureSession {
    stops: Arc<std::sync::atomic::AtomicUsize>,
    stopped: bool,
}
impl carplay_core::media::CaptureSession for TestCaptureSession {
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
impl Drop for TestCaptureSession {
    fn drop(&mut self) {
        carplay_core::media::CaptureSession::stop(self);
    }
}
impl carplay_core::media::CaptureFactory for TestCapture {
    fn supports(&self, config: &carplay_core::media::CaptureConfig) -> bool {
        config.format.codec == carplay_core::media::AudioCodec::Lpcm && config.format.channels == 1
    }
    fn start(
        &self,
        config: carplay_core::media::CaptureConfig,
        output: Arc<dyn carplay_core::media::CaptureSink>,
    ) -> Result<Box<dyn carplay_core::media::CaptureSession>, String> {
        assert!(self.supports(&config));
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Already in the CaptureSink contract's S16BE wire byte order.
        output.packet([0x12u8, 0x34].repeat(config.samples_per_packet as usize))?;
        Ok(Box::new(TestCaptureSession {
            stops: self.stops.clone(),
            stopped: false,
        }))
    }
}

#[test]
fn microphone_starts_only_on_valid_media_and_encrypts_uplink_with_input_key() {
    use std::sync::atomic::Ordering::SeqCst;
    let capture = Arc::new(TestCapture::default());
    let (_directory, mut server, mut peer, shared, media) =
        verified_with_options(false, Some(capture.clone()), true);
    let info = response_plist(&peer.request("GET", "/info", &[]));
    let formats = info.as_dictionary().unwrap()["audioFormats"]
        .as_array()
        .unwrap();
    for format in formats {
        if let Some(bits) = format.as_dictionary().unwrap().get("audioInputFormats") {
            assert_eq!(
                bits.as_unsigned_integer().unwrap() & 0x70000000,
                0,
                "unsupported Opus must not be advertised"
            );
        }
    }
    assert_eq!(capture.starts.load(SeqCst), 0);
    let microphone = UdpSocket::bind("127.0.0.1:0").unwrap();
    microphone
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let stream = |bits| {
        dict([
            ("type", number(100)),
            ("audioType", text("speechRecognition")),
            ("audioFormat", number(bits)),
            ("streamConnectionID", number(991)),
            ("framesPerPacket", number(320)),
            (
                "dataPort",
                number(microphone.local_addr().unwrap().port().into()),
            ),
        ])
    };
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([stream(0x20000000)]))]))
            .path,
        "501"
    );
    let setup = response_plist(&peer.plist("SETUP", dict([("streams", array([stream(0x10)]))])));
    let port = setup.as_dictionary().unwrap()["streams"]
        .as_array()
        .unwrap()[0]
        .as_dictionary()
        .unwrap()["dataPort"]
        .as_unsigned_integer()
        .unwrap() as u16;
    assert_eq!(
        capture.starts.load(SeqCst),
        0,
        "SETUP must not start capture"
    );
    let down = crypto::derive_key(
        &shared,
        b"DataStream-Salt991",
        b"DataStream-Output-Encryption-Key",
    )
    .unwrap();
    let input = crypto::derive_key(
        &shared,
        b"DataStream-Salt991",
        b"DataStream-Input-Encryption-Key",
    )
    .unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut packet = vec![0x80, 100, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    packet
        .extend(crypto::chacha_seal(&down, &crypto::nonce64(0), &[0, 0], &packet[4..12]).unwrap());
    packet.extend_from_slice(&0u64.to_le_bytes());
    let mut corrupt = packet.clone();
    corrupt[12] ^= 1;
    sender.send_to(&corrupt, ("127.0.0.1", port)).unwrap();
    sender.send_to(&packet, ("127.0.0.1", port)).unwrap();
    let mut buffer = [0u8; 2048];
    let (length, _) = microphone.recv_from(&mut buffer).unwrap();
    let uplink = &buffer[..length];
    assert_eq!(&uplink[..12], &[0x80, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&uplink[length - 8..], &[0; 8]);
    let plaintext = crypto::chacha_open(
        &input,
        &crypto::nonce64(0),
        &uplink[12..length - 8],
        &uplink[4..12],
    )
    .unwrap();
    assert_eq!(plaintext, [0x12u8, 0x34].repeat(320));
    assert!(
        crypto::chacha_open(
            &down,
            &crypto::nonce64(0),
            &uplink[12..length - 8],
            &uplink[4..12]
        )
        .is_err()
    );
    assert_eq!(capture.starts.load(SeqCst), 1);
    assert!(matches!(
        media.recv_timeout(Duration::from_secs(3)).unwrap(),
        MediaEvent::AudioConfig { stream: 100, .. }
    ));
    assert!(matches!(
        media.recv_timeout(Duration::from_secs(3)).unwrap(),
        MediaEvent::Audio { .. }
    ));
    assert_eq!(
        peer.plist("TEARDOWN", dict([("streams", array([number(100)]))]))
            .path,
        "200"
    );
    assert_eq!(capture.stops.load(SeqCst), 1);
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([stream(0x10)]))]))
            .path,
        "455",
        "a microphone key cannot restart its nonce after TEARDOWN"
    );
    server.stop();
    assert_eq!(capture.stops.load(SeqCst), 1);
}

#[test]
fn disabled_microphone_does_not_start_supplied_factory() {
    let capture = Arc::new(TestCapture::default());
    let (_directory, mut server, mut peer, _shared, _media) =
        verified_with_options(false, Some(capture.clone()), false);
    let info = response_plist(&peer.request("GET", "/info", &[]));
    assert!(
        info.as_dictionary().unwrap()["audioFormats"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| !f.as_dictionary().unwrap().contains_key("audioInputFormats"))
    );
    let stream = dict([
        ("type", number(100)),
        ("audioType", text("telephony")),
        ("audioFormat", number(0x10)),
        ("streamConnectionID", number(1)),
        ("dataPort", number(12345)),
    ]);
    assert_eq!(
        peer.plist("SETUP", dict([("streams", array([stream]))]))
            .path,
        "501"
    );
    server.stop();
    assert_eq!(capture.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[cfg(feature = "gstreamer-capture-tests")]
#[test]
#[ignore = "requires GStreamer; uses only audiotestsrc and never opens a microphone"]
fn native_generated_pcm_and_opus_reach_encrypted_udp_uplink() {
    for (bits, expected_step) in [(0x10u64, 320u32), (0x20000000, 480)] {
        let capture = Arc::new(carplay_media::GStreamerCaptureFactory::synthetic().unwrap());
        let (_directory, mut server, mut peer, shared, _media) =
            verified_with_options(false, Some(capture), true);
        let microphone = UdpSocket::bind("127.0.0.1:0").unwrap();
        microphone
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let setup = response_plist(&peer.plist(
            "SETUP",
            dict([(
                "streams",
                array([dict([
                    ("type", number(100)),
                    ("audioType", text("speechRecognition")),
                    ("audioFormat", number(bits)),
                    ("streamConnectionID", number(553)),
                    ("framesPerPacket", number(320)),
                    (
                        "dataPort",
                        number(microphone.local_addr().unwrap().port().into()),
                    ),
                ])]),
            )]),
        ));
        let port = setup.as_dictionary().unwrap()["streams"]
            .as_array()
            .unwrap()[0]
            .as_dictionary()
            .unwrap()["dataPort"]
            .as_unsigned_integer()
            .unwrap() as u16;
        let down = crypto::derive_key(
            &shared,
            b"DataStream-Salt553",
            b"DataStream-Output-Encryption-Key",
        )
        .unwrap();
        let input = crypto::derive_key(
            &shared,
            b"DataStream-Salt553",
            b"DataStream-Input-Encryption-Key",
        )
        .unwrap();
        let mut packet = vec![0x80, 100, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        packet.extend(
            crypto::chacha_seal(&down, &crypto::nonce64(0), &[0, 0], &packet[4..12]).unwrap(),
        );
        packet.extend_from_slice(&0u64.to_le_bytes());
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .send_to(&packet, ("127.0.0.1", port))
            .unwrap();
        for sequence in 0..3u16 {
            let mut wire = [0u8; 2048];
            let (length, _) = microphone.recv_from(&mut wire).unwrap();
            assert_eq!(u16::from_be_bytes(wire[2..4].try_into().unwrap()), sequence);
            assert_eq!(
                u32::from_be_bytes(wire[4..8].try_into().unwrap()),
                u32::from(sequence) * expected_step
            );
            let counter = u64::from_le_bytes(wire[length - 8..length].try_into().unwrap());
            assert_eq!(counter, u64::from(sequence));
            let body = crypto::chacha_open(
                &input,
                &crypto::nonce64(counter),
                &wire[12..length - 8],
                &wire[4..12],
            )
            .unwrap();
            if bits == 0x10 {
                assert_eq!(body.len(), 640);
                let samples: Vec<_> = body
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_be_bytes([b[0], b[1]]))
                    .collect();
                assert!(samples.iter().any(|&sample| sample != 0));
                assert!(samples.iter().all(|&sample| i32::from(sample).abs() < 9000));
            } else {
                assert!(!body.is_empty() && body.len() <= 1275);
                assert!(!body.starts_with(b"OpusHead") && !body.starts_with(b"OpusTags"));
            }
        }
        assert_eq!(
            peer.plist("TEARDOWN", dict([("streams", array([number(100)]))]))
                .path,
            "200"
        );
        server.stop();
        assert!(
            !server
                .events
                .try_iter()
                .any(|event| matches!(event, ReceiverEvent::Error(_)))
        );
    }
}
