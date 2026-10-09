// SPDX-License-Identifier: GPL-3.0-only
// Control routing and media wire formats derived from DiPlay airplay/AirPlaySession.kt.
use crate::{
    info::{self, array, dict, number, text},
    storage::FilePairingStore,
};
use anyhow::{Context, Result, bail, ensure};
use carplay_auth::{
    AirPlayIdentity, AuthProvider, ControlReader, ControlWriter, PairSetup, PairVerify,
    PairingStore, crypto,
};
use carplay_core::{
    config::ReceiverConfig,
    media::{self, MediaEvent, MediaSink},
    rtsp,
};
use plist::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct ReceiverOptions {
    pub bind: SocketAddr,
    pub config: ReceiverConfig,
    pub bluetooth_address: String,
    pub state_dir: PathBuf,
    pub advertise: bool,
}

#[derive(Default)]
pub struct RuntimeOptions {
    pub identification: Option<carplay_protocol::tlv::Identification>,
    pub endpoint: Option<carplay_wireless::Endpoint>,
    /// Constructing this factory must not open an input device.
    pub capture: Option<Arc<dyn media::CaptureFactory>>,
}

#[derive(Clone)]
pub enum ReceiverEvent {
    Listening(SocketAddr),
    TcpAccepted,
    PairingComplete,
    Verified,
    StreamStarted(u16),
    FirstVideoFrame(u16),
    Disconnected,
    UiRequested(String),
    IapMessage { message_id: u16, body: Vec<u8> },
    UnsupportedIapSession { session_id: u8, bytes: usize },
    IapArtwork { id: u8, data: Vec<u8> },
    Error(String),
}

impl std::fmt::Debug for ReceiverEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Listening(address) => f.debug_tuple("Listening").field(address).finish(),
            Self::TcpAccepted => f.write_str("TcpAccepted"),
            Self::PairingComplete => f.write_str("PairingComplete"),
            Self::Verified => f.write_str("Verified"),
            Self::StreamStarted(kind) => f.debug_tuple("StreamStarted").field(kind).finish(),
            Self::FirstVideoFrame(kind) => f.debug_tuple("FirstVideoFrame").field(kind).finish(),
            Self::Disconnected => f.write_str("Disconnected"),
            Self::UiRequested(_) => f.write_str("UiRequested([redacted])"),
            Self::IapMessage { message_id, body } => f
                .debug_struct("IapMessage")
                .field("message_id", message_id)
                .field("body_bytes", &body.len())
                .finish(),
            Self::Error(error) => f.debug_tuple("Error").field(error).finish(),
            Self::UnsupportedIapSession { session_id, bytes } => f
                .debug_struct("UnsupportedIapSession")
                .field("session_id", session_id)
                .field("bytes", bytes)
                .finish(),
            Self::IapArtwork { id, data } => f
                .debug_struct("IapArtwork")
                .field("id", id)
                .field("bytes", &data.len())
                .finish(),
        }
    }
}

enum Command {
    Hid(u32, Vec<u8>),
    Night(bool),
}
pub struct ReceiverHandle {
    pub address: SocketAddr,
    pub events: Receiver<ReceiverEvent>,
    stop: Arc<AtomicBool>,
    commands: SyncSender<Command>,
    input_overflow: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl ReceiverHandle {
    pub fn hid(&self, uid: u32, report: Vec<u8>) -> bool {
        match self.commands.try_send(Command::Hid(uid, report)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                // Losing a release can leave the phone holding a contact indefinitely.
                // Reset the session instead of continuing with incomplete input state.
                self.input_overflow.store(true, Ordering::Release);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
    pub fn night(&self, value: bool) -> bool {
        self.commands.try_send(Command::Night(value)).is_ok()
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for ReceiverHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn event(tx: &SyncSender<ReceiverEvent>, value: ReceiverEvent) {
    let _ = tx.try_send(value);
}

pub fn start(
    options: ReceiverOptions,
    auth: Arc<dyn AuthProvider>,
    sink: Arc<dyn MediaSink>,
) -> Result<ReceiverHandle> {
    start_with_runtime(options, auth, sink, RuntimeOptions::default())
}

/// Start with the same identification used by the wireless bootstrap, allowing
/// the authenticated runtime iAP2 tunnel to take over from Bluetooth.
pub fn start_with_iap(
    options: ReceiverOptions,
    auth: Arc<dyn AuthProvider>,
    sink: Arc<dyn MediaSink>,
    identification: carplay_protocol::tlv::Identification,
) -> Result<ReceiverHandle> {
    start_with_runtime(
        options,
        auth,
        sink,
        RuntimeOptions {
            identification: Some(identification),
            ..Default::default()
        },
    )
}

/// Enable runtime Wi-Fi reconfiguration using the caller's actual network.
/// The receiver replaces the endpoint's port and identity fields after binding.
pub fn start_with_bootstrap(
    options: ReceiverOptions,
    auth: Arc<dyn AuthProvider>,
    sink: Arc<dyn MediaSink>,
    identification: carplay_protocol::tlv::Identification,
    endpoint: carplay_wireless::Endpoint,
) -> Result<ReceiverHandle> {
    start_with_runtime(
        options,
        auth,
        sink,
        RuntimeOptions {
            identification: Some(identification),
            endpoint: Some(endpoint),
            capture: None,
        },
    )
}

pub fn start_with_runtime(
    options: ReceiverOptions,
    auth: Arc<dyn AuthProvider>,
    sink: Arc<dyn MediaSink>,
    runtime: RuntimeOptions,
) -> Result<ReceiverHandle> {
    options.config.validate().map_err(anyhow::Error::msg)?;
    let RuntimeOptions {
        identification,
        mut endpoint,
        capture,
    } = runtime;
    if let Some(identity) = &identification {
        identity.build()?;
        if let Some(endpoint) = &endpoint {
            ensure!(
                matches!(
                    (&identity.transport, endpoint.transport),
                    (
                        carplay_protocol::tlv::IdentificationTransport::Wired { .. },
                        carplay_wireless::EndpointTransport::Wired
                    ) | (
                        carplay_protocol::tlv::IdentificationTransport::Wireless { .. },
                        carplay_wireless::EndpointTransport::Wireless
                    )
                ),
                "identification and runtime transport differ"
            );
        }
        if let (
            carplay_protocol::tlv::IdentificationTransport::Wireless { ssid, .. },
            Some(endpoint),
        ) = (&identity.transport, &endpoint)
        {
            ensure!(
                ssid == &endpoint.ssid,
                "identification and runtime Wi-Fi SSID differ"
            );
        }
    }
    ensure!(
        endpoint.is_none() || identification.is_some(),
        "runtime Wi-Fi requires iAP identification"
    );
    let iap = identification.map(Arc::new);
    ensure!(
        !options.config.microphone || (options.config.audio_output && capture.is_some()),
        "enabled microphone requires audio output and a capture factory"
    );
    let (identity, pairings) = FilePairingStore::open(&options.state_dir)?;
    let listener = TcpListener::bind(options.bind).context("bind AirPlay control port")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    if let Some(endpoint) = endpoint.as_mut() {
        endpoint.airplay_port = address.port();
        endpoint.device_identifier = options.bluetooth_address.clone();
        endpoint.public_key = identity
            .public_key()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        endpoint.source_version = carplay_core::SOURCE_VERSION.into();
        endpoint.validate()?;
    }
    let endpoint = endpoint.map(Arc::new);
    let (tx, rx) = mpsc::sync_channel(128);
    let (commands, command_rx) = mpsc::sync_channel(128);
    let input_overflow = Arc::new(AtomicBool::new(false));
    let overflow = input_overflow.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    // Registration is kept alive by the listener thread and explicitly removed on shutdown.
    let discovery = if options.advertise {
        Some(advertise(&options, address, &identity)?)
    } else {
        None
    };
    let join = thread::Builder::new()
        .name("carplay-control".into())
        .spawn(move || {
            let _discovery = discovery;
            event(&tx, ReceiverEvent::Listening(address));
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, peer)) => {
                        overflow.store(false, Ordering::Release);
                        while command_rx.try_recv().is_ok() {} // do not replay stale input on reconnect
                        event(&tx, ReceiverEvent::TcpAccepted);
                        let ctx = ContextState {
                            options: &options,
                            identity: identity.clone(),
                            pairings: pairings.clone(),
                            auth: auth.clone(),
                            sink: sink.clone(),
                            stop: stopped.clone(),
                            events: tx.clone(),
                            commands: &command_rx,
                            input_overflow: &overflow,
                            iap: iap.clone(),
                            endpoint: endpoint.clone(),
                            capture: capture.clone(),
                        };
                        if let Err(error) = connection(socket, peer, ctx) {
                            event(&tx, ReceiverEvent::Error(error.to_string()));
                        }
                        event(&tx, ReceiverEvent::Disconnected);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(30))
                    }
                    Err(e) => {
                        event(&tx, ReceiverEvent::Error(e.to_string()));
                        break;
                    }
                }
            }
        })?;
    Ok(ReceiverHandle {
        address,
        events: rx,
        stop,
        commands,
        input_overflow,
        thread: Some(join),
    })
}

struct Discovery {
    daemon: mdns_sd::ServiceDaemon,
    names: Vec<String>,
}
impl Drop for Discovery {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = self.daemon.unregister(name);
        }
        let _ = self.daemon.shutdown();
    }
}
/// Keep the IPv6 interface scope on USB NCM link-local sockets.
fn with_port(mut address: SocketAddr, port: u16) -> SocketAddr {
    address.set_port(port);
    address
}

fn advertise(
    o: &ReceiverOptions,
    address: SocketAddr,
    identity: &AirPlayIdentity,
) -> Result<Discovery> {
    ensure!(
        !address.ip().is_unspecified() && !address.ip().is_loopback(),
        "discovery requires an explicit LAN interface address"
    );
    let daemon = mdns_sd::ServiceDaemon::new()?;
    let mut d = Discovery {
        daemon,
        names: Vec::new(),
    };
    let host = format!(
        "rustcarplay-{}.local.",
        &identity.pairing_id.replace('-', "")[..12]
    );
    let public_key = identity
        .public_key()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut properties = BTreeMap::new();
    properties.insert("deviceid".to_owned(), o.bluetooth_address.clone());
    properties.insert(
        "features".into(),
        format!(
            "{:#x},{:#x}",
            info::features(&o.config) & 0xffffffff,
            info::features(&o.config) >> 32
        ),
    );
    properties.insert("flags".into(), "0x4".into());
    properties.insert("model".into(), "RustCarPlay".into());
    properties.insert("srcvers".into(), carplay_core::SOURCE_VERSION.into());
    properties.insert("protovers".into(), "1.1".into());
    properties.insert("pi".into(), identity.pairing_id.clone());
    properties.insert("pk".into(), public_key);
    {
        let kind = "_airplay._tcp.local.";
        let props: Vec<(&str, &str)> = properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let service = mdns_sd::ServiceInfo::new(
            kind,
            &o.config.name,
            &host,
            address.ip(),
            address.port(),
            props.as_slice(),
        )?;
        d.names.push(service.get_fullname().into());
        d.daemon.register(service)?;
    }
    Ok(d)
}

struct ContextState<'a> {
    options: &'a ReceiverOptions,
    identity: Arc<AirPlayIdentity>,
    pairings: Arc<dyn PairingStore>,
    auth: Arc<dyn AuthProvider>,
    sink: Arc<dyn MediaSink>,
    stop: Arc<AtomicBool>,
    events: SyncSender<ReceiverEvent>,
    commands: &'a Receiver<Command>,
    input_overflow: &'a AtomicBool,
    iap: Option<Arc<carplay_protocol::tlv::Identification>>,
    endpoint: Option<Arc<carplay_wireless::Endpoint>>,
    capture: Option<Arc<dyn media::CaptureFactory>>,
}

struct Workers {
    cancel: Arc<AtomicBool>,
    failure: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    sink: Arc<dyn MediaSink>,
    active: Vec<u16>,
    streams: Vec<(StreamKey, Workers)>,
    diagnostics: Option<SyncSender<ReceiverEvent>>,
    used_microphone_keys: BTreeSet<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct StreamKey {
    kind: u16,
    audio_type: String,
}
impl Workers {
    fn new(sink: Arc<dyn MediaSink>) -> Self {
        Self {
            cancel: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            sink,
            active: Vec::new(),
            streams: Vec::new(),
            diagnostics: None,
            used_microphone_keys: BTreeSet::new(),
        }
    }
    fn contains(&self, key: &StreamKey) -> bool {
        self.streams.iter().any(|(existing, _)| existing == key)
    }
    fn teardown(&mut self, types: Option<&[u16]>) {
        for (key, worker) in &self.streams {
            if types.is_none_or(|types| types.contains(&key.kind)) {
                worker.cancel.store(true, Ordering::Release);
            }
        }
        self.streams
            .retain(|(key, _)| types.is_some_and(|types| !types.contains(&key.kind)));
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        for (_, worker) in &self.streams {
            worker.cancel.store(true, Ordering::Release);
        }
        self.streams.clear();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        for stream in &self.active {
            let _ = self.sink.send(MediaEvent::Stop { stream: *stream });
        }
    }
}

struct EventWriter {
    socket: TcpStream,
    cipher: ControlWriter,
    cseq: u64,
}
impl EventWriter {
    fn send(&mut self, command: Value) -> Result<()> {
        let body = info::encode(&command)?;
        self.cseq += 1;
        let mut bytes=format!("POST /command RTSP/1.0\r\nCSeq: {}\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",self.cseq,body.len()).into_bytes();
        bytes.extend(body);
        self.socket.write_all(&self.cipher.encrypt(&bytes)?)?;
        Ok(())
    }
}

fn connection(mut socket: TcpStream, peer: SocketAddr, ctx: ContextState<'_>) -> Result<()> {
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let local = socket.local_addr()?;
    let mut setup = PairSetup::new(ctx.identity.clone(), ctx.pairings.clone());
    let mut verify = PairVerify::new(ctx.identity.clone(), ctx.pairings.clone());
    let mut reader: Option<ControlReader> = None;
    let mut writer: Option<ControlWriter> = None;
    let mut parser = rtsp::Decoder::default();
    let mut buffer = [0u8; 16384];
    let mut workers = Workers::new(ctx.sink.clone());
    let events_writer: Arc<Mutex<Option<EventWriter>>> = Arc::new(Mutex::new(None));
    let mut event_open = false;
    let started = Instant::now();
    let mut last_activity = Instant::now();
    loop {
        if ctx.stop.load(Ordering::Acquire) || workers.failure.load(Ordering::Acquire) {
            break;
        }
        ensure!(
            !ctx.input_overflow.load(Ordering::Acquire),
            "input queue overflow: reconnect required to reset touch state"
        );
        if (!verify.is_verified() && started.elapsed() > Duration::from_secs(60))
            || last_activity.elapsed() > Duration::from_secs(90)
        {
            bail!("control connection timed out");
        }
        while let Ok(command) = ctx.commands.try_recv() {
            ensure!(
                !ctx.input_overflow.load(Ordering::Acquire),
                "input queue overflow: reconnect required to reset touch state"
            );
            if let Ok(mut writer) = events_writer.lock()
                && let Some(writer) = writer.as_mut()
            {
                let body = match command {
                    Command::Hid(uid, report) => dict([
                        ("type", text("hidSendReport")),
                        ("uuid", text(format!("{uid:x}"))),
                        ("hidReport", Value::Data(report)),
                    ]),
                    Command::Night(value) => dict([
                        ("type", text("setNightMode")),
                        ("params", dict([("nightMode", Value::Boolean(value))])),
                    ]),
                };
                // A failed write may already have sent part of an encrypted record.
                // Its cipher state cannot safely be reused for subsequent input.
                writer.send(body).context("input channel write failed")?;
            }
        }
        match socket.read(&mut buffer) {
            Ok(0) => {
                if let Some(reader) = reader.as_mut() {
                    reader.finish()?;
                }
                ensure!(parser.buffered_len() == 0, "truncated RTSP message at EOF");
                break;
            }
            Ok(n) => {
                last_activity = Instant::now();
                let bytes = if let Some(r) = reader.as_mut() {
                    r.decrypt(&buffer[..n])?
                } else {
                    buffer[..n].to_vec()
                };
                parser.push(&bytes)?;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        while let Some(request) = parser.next_request()? {
            let mut status = 200;
            let mut kind = None;
            let mut body = Vec::new();
            let mut enable_cipher = false;
            let normalized_path = request
                .path
                .split('?')
                .next()
                .unwrap_or(&request.path)
                .trim_end_matches('/');
            match (request.method.as_str(), normalized_path) {
                ("POST", "/pair-setup") if reader.is_none() => {
                    body = setup.handle(&request.body);
                    kind = Some("application/pairing+tlv8");
                    if setup.complete() {
                        event(&ctx.events, ReceiverEvent::PairingComplete);
                    }
                }
                ("POST", "/pair-verify") if reader.is_none() => {
                    body = verify.handle(&request.body);
                    kind = Some("application/pairing+tlv8");
                    enable_cipher = verify.is_verified();
                }
                ("POST", "/auth-setup") => {
                    match carplay_auth::mfi_sap_auth_setup(&request.body, ctx.auth.as_ref()) {
                        Ok(b) => {
                            body = b;
                            kind = Some("application/octet-stream");
                        }
                        Err(_) => status = 401,
                    }
                }
                ("GET" | "POST", "/info") => {
                    let mut declaration = info::build(
                        &ctx.options.config,
                        &ctx.options.bluetooth_address,
                        &ctx.options.bluetooth_address,
                    );
                    info::filter_capture(&mut declaration, ctx.capture.as_deref());
                    body = info::encode(&declaration)?;
                    kind = Some("application/x-apple-binary-plist");
                }
                ("SETUP", _) => {
                    if !verify.is_verified() {
                        status = 401;
                    } else {
                        let decoded = Value::from_reader(std::io::Cursor::new(&request.body));
                        if let Ok(Value::Dictionary(d)) = decoded {
                            let shared =
                                verify.shared_secret().context("verified secret missing")?;
                            if let Some(Value::Array(streams)) = d.get("streams") {
                                let (result, replies) = setup_streams(
                                    streams,
                                    &mut workers,
                                    &ctx,
                                    local,
                                    peer,
                                    shared,
                                    events_writer.clone(),
                                )?;
                                status = result;
                                if status == 200 {
                                    body = info::encode(&dict([("streams", array(replies))]))?;
                                }
                            } else if d.contains_key("streams") {
                                status = 400;
                            } else if !event_open {
                                let timing_port = timing(
                                    &mut workers,
                                    local,
                                    peer,
                                    info::get_number(&d, "timingPort")
                                        .and_then(|n| u16::try_from(n).ok()),
                                )?;
                                let event_port = event_channel(
                                    &mut workers,
                                    local,
                                    peer,
                                    shared.as_ref(),
                                    events_writer.clone(),
                                    ctx.events.clone(),
                                )?;
                                event_open = true;
                                body = info::encode(&dict([
                                    ("timingPort", number(timing_port.into())),
                                    ("eventPort", number(event_port.into())),
                                    ("enabledFeatures", array([text("viewAreas")])),
                                ]))?;
                            } else {
                                status = 455;
                            }
                            kind = Some("application/x-apple-binary-plist");
                        } else {
                            status = 400;
                        }
                    }
                }
                ("RECORD", _) if verify.is_verified() => {}
                ("TEARDOWN", _) if verify.is_verified() => match teardown_types(&request.body) {
                    Ok(types) => workers.teardown(types.as_deref()),
                    Err(_) => status = 400,
                },
                ("OPTIONS", _) => {}
                ("POST", "/feedback") if verify.is_verified() => {}
                ("POST", "/command") if verify.is_verified() => {
                    status = handle_command(&request.body, &ctx.events);
                }
                _ => status = 404,
            }
            let response = rtsp::response(&request, status, kind, &body);
            socket.write_all(&if let Some(w) = writer.as_mut() {
                w.encrypt(&response)?
            } else {
                response
            })?;
            if enable_cipher {
                let keys = verify.control_keys().context("missing control keys")?;
                reader = Some(ControlReader::new(*keys.read_key));
                writer = Some(ControlWriter::new(*keys.write_key));
                event(&ctx.events, ReceiverEvent::Verified);
                let rest = parser.take_buffer();
                if !rest.is_empty() {
                    parser.push(&reader.as_mut().unwrap().decrypt(&rest)?)?;
                }
            }
        }
    }
    Ok(())
}

struct StreamSpec {
    id: StreamKey,
    seed: u64,
    connection_id: Option<u64>,
    format: Option<media::AudioFormat>,
    microphone: Option<crate::microphone::Spec>,
}

fn unsigned_bits(d: &plist::Dictionary, key: &str) -> Option<u64> {
    let value = d.get(key)?;
    value
        .as_unsigned_integer()
        .or_else(|| value.as_signed_integer().map(|n| n as u64))
}

fn validate_streams(
    streams: &[Value],
    workers: &Workers,
    ctx: &ContextState<'_>,
) -> std::result::Result<Vec<StreamSpec>, u16> {
    if streams.len() > 16 || streams.len() + workers.streams.len() > 16 {
        return Err(453);
    }
    let mut specifications: Vec<StreamSpec> = Vec::new();
    for value in streams {
        let d = value.as_dictionary().ok_or(400u16)?;
        let kind = info::get_number(d, "type")
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(400u16)?;
        let (audio_type, format, seed) = match kind {
            110 | 111 if kind == 110 || ctx.options.config.second_screen => (
                String::new(),
                None,
                unsigned_bits(d, "streamConnectionID").ok_or(400u16)?,
            ),
            100..=102 if ctx.options.config.audio_output => {
                let name = match d.get("audioType") {
                    None => "default",
                    Some(Value::String(s)) => s,
                    _ => return Err(400),
                };
                if name.is_empty() || name.len() > 64 {
                    return Err(400);
                }
                let format = media::AudioFormat::from_bits(
                    info::get_number(d, "audioFormat").ok_or(400u16)?,
                )
                .map_err(|_| 400u16)?;
                (
                    name.to_ascii_lowercase(),
                    Some(format),
                    unsigned_bits(d, "streamConnectionID").ok_or(400u16)?,
                )
            }
            130 if ctx.iap.is_some() => {
                if !d
                    .get("clientTypeUUID")
                    .and_then(Value::as_string)
                    .is_some_and(|uuid| uuid.eq_ignore_ascii_case(crate::tunnel::CLIENT_UUID))
                {
                    return Err(501);
                }
                (String::new(), None, unsigned_bits(d, "seed").ok_or(400u16)?)
            }
            _ => return Err(501),
        };
        let id = StreamKey { kind, audio_type };
        let microphone = if (100..=102).contains(&kind) {
            crate::microphone::negotiate(
                kind,
                &id.audio_type,
                d,
                ctx.options.config.microphone,
                ctx.capture.as_ref(),
            )?
        } else {
            None
        };
        if workers.contains(&id) || specifications.iter().any(|spec| spec.id == id) {
            return Err(455);
        }
        // A new packetizer starts at nonce zero. Reusing its key after TEARDOWN
        // would reuse AEAD nonces, even when the audioType differs.
        if microphone.is_some() {
            if workers.used_microphone_keys.contains(&seed)
                || specifications
                    .iter()
                    .any(|spec| spec.microphone.is_some() && spec.seed == seed)
            {
                return Err(455);
            }
            if workers.used_microphone_keys.len() >= 4096 {
                return Err(453);
            }
        }
        specifications.push(StreamSpec {
            id,
            seed,
            connection_id: unsigned_bits(d, "streamConnectionID"),
            format,
            microphone,
        });
    }
    Ok(specifications)
}

fn setup_streams(
    streams: &[Value],
    workers: &mut Workers,
    ctx: &ContextState<'_>,
    local: SocketAddr,
    peer: SocketAddr,
    shared: &[u8],
    slot: Arc<Mutex<Option<EventWriter>>>,
) -> Result<(u16, Vec<Value>)> {
    let specifications = match validate_streams(streams, workers, ctx) {
        Ok(specs) => specs,
        Err(status) => return Ok((status, vec![])),
    };
    let mut replies = Vec::new();
    // Validation precedes all side effects. A subsequent bind/start failure closes
    // the owning session, releasing every already-created stream.
    for spec in specifications {
        let salt = format!("DataStream-Salt{}", spec.seed);
        let key =
            *crypto::derive_key(shared, salt.as_bytes(), b"DataStream-Output-Encryption-Key")?;
        let mut worker = Workers::new(ctx.sink.clone());
        worker.failure = workers.failure.clone();
        worker.diagnostics = Some(ctx.events.clone());
        if spec.microphone.is_some() {
            workers.used_microphone_keys.insert(spec.seed);
        }
        let kind = spec.id.kind;
        // Register ownership before preparation: a later bind or decoder error
        // must still stop every partially prepared stream on this session.
        worker.active.push(kind);
        let response = match kind {
            110 | 111 => {
                let port = screen(&mut worker, local, peer, key, kind, ctx.events.clone())?;
                dict([
                    ("type", number(kind.into())),
                    ("dataPort", number(port.into())),
                ])
            }
            100..=102 => {
                let microphone = spec
                    .microphone
                    .map(|specification| -> Result<_> {
                        Ok(crate::microphone::Pending {
                            spec: specification,
                            factory: ctx.capture.clone().context("capture factory missing")?,
                            key: crypto::derive_key(
                                shared,
                                salt.as_bytes(),
                                b"DataStream-Input-Encryption-Key",
                            )?,
                            local,
                            peer,
                            cancel: worker.cancel.clone(),
                            failure: worker.failure.clone(),
                            events: ctx.events.clone(),
                        })
                    })
                    .transpose()?;
                let (data, control) = audio(
                    &mut worker,
                    local,
                    peer,
                    key,
                    kind,
                    spec.id.audio_type.clone(),
                    spec.format.context("audio format missing")?,
                    microphone,
                )?;
                dict([
                    ("type", number(kind.into())),
                    ("audioType", text(&spec.id.audio_type)),
                    ("dataPort", number(data.into())),
                    ("controlPort", number(control.into())),
                    ("streamConnectionID", number(spec.seed)),
                ])
            }
            130 => {
                let listener = TcpListener::bind(with_port(local, 0))?;
                let port = listener.local_addr()?.port();
                let cancel = worker.cancel.clone();
                let failed = worker.failure.clone();
                let auth = ctx.auth.clone();
                let identification = ctx.iap.clone().context("tunnel identification missing")?;
                let endpoint = ctx.endpoint.clone();
                let events = ctx.events.clone();
                let slot = slot.clone();
                worker.threads.push(thread::spawn(move || {
                    let run = || -> Result<()> {
                        let Some(stream) = accept(listener, peer, &cancel)? else {
                            return Ok(());
                        };
                        crate::tunnel::run(
                            stream,
                            key,
                            auth.as_ref(),
                            &identification,
                            endpoint.as_deref(),
                            &cancel,
                            |data| {
                                let deadline = Instant::now() + Duration::from_secs(10);
                                loop {
                                    if cancel.load(Ordering::Acquire) {
                                        bail!("iAP tunnel cancelled");
                                    }
                                    if let Some(writer) = slot
                                        .lock()
                                        .map_err(|_| anyhow::anyhow!("event lock poisoned"))?
                                        .as_mut()
                                    {
                                        return writer.send(dict([
                                            ("type", text("iAPSendMessage")),
                                            ("params", dict([("data", Value::Data(data))])),
                                        ]));
                                    }
                                    ensure!(
                                        Instant::now() < deadline,
                                        "iAP tunnel event channel not ready"
                                    );
                                    thread::sleep(Duration::from_millis(20));
                                }
                            },
                            |message| {
                                events
                                    .try_send(ReceiverEvent::IapMessage {
                                        message_id: message.message_id,
                                        body: message.body,
                                    })
                                    .map_err(|_| {
                                        anyhow::anyhow!("iAP receiver event queue full or closed")
                                    })
                            },
                            |session_id, bytes| {
                                events
                                    .try_send(ReceiverEvent::UnsupportedIapSession {
                                        session_id,
                                        bytes,
                                    })
                                    .map_err(|_| {
                                        anyhow::anyhow!("iAP receiver event queue full or closed")
                                    })
                            },
                            |artwork| {
                                events
                                    .try_send(ReceiverEvent::IapArtwork {
                                        id: artwork.id,
                                        data: artwork.bytes,
                                    })
                                    .map_err(|_| {
                                        anyhow::anyhow!("iAP artwork event queue full or closed")
                                    })
                            },
                        )
                    };
                    if let Err(error) = run()
                        && !cancel.load(Ordering::Acquire)
                    {
                        event(
                            &events,
                            ReceiverEvent::Error(format!("iAP tunnel: {error}")),
                        );
                    }
                    if !cancel.load(Ordering::Acquire) {
                        failed.store(true, Ordering::Release);
                    }
                }));
                let mut response = dict([
                    ("type", number(130)),
                    ("dataPort", number(port.into())),
                    ("streamID", number(1)),
                ]);
                if let Some(id) = spec.connection_id {
                    response
                        .as_dictionary_mut()
                        .unwrap()
                        .insert("streamConnectionID".into(), number(id));
                }
                response
            }
            _ => unreachable!("validated stream type"),
        };
        workers.streams.push((spec.id, worker));
        event(&ctx.events, ReceiverEvent::StreamStarted(kind));
        replies.push(response);
    }
    Ok((200, replies))
}

fn teardown_types(body: &[u8]) -> Result<Option<Vec<u16>>> {
    if body.is_empty() {
        return Ok(None);
    }
    let value = Value::from_reader(std::io::Cursor::new(body))?;
    let d = value
        .as_dictionary()
        .context("TEARDOWN dictionary required")?;
    let Some(streams) = d.get("streams") else {
        return Ok(None);
    };
    let streams = streams
        .as_array()
        .context("TEARDOWN streams array required")?;
    let mut kinds = Vec::new();
    for stream in streams {
        let number = stream
            .as_unsigned_integer()
            .or_else(|| {
                stream
                    .as_dictionary()
                    .and_then(|d| info::get_number(d, "type"))
            })
            .context("TEARDOWN stream type required")?;
        kinds.push(u16::try_from(number)?);
    }
    Ok(if kinds.is_empty() { None } else { Some(kinds) })
}

fn handle_command(body: &[u8], events: &SyncSender<ReceiverEvent>) -> u16 {
    let Ok(Value::Dictionary(command)) = Value::from_reader(std::io::Cursor::new(body)) else {
        return 400;
    };
    match command.get("type").and_then(Value::as_string) {
        Some("requestUI") => {
            let url = command
                .get("params")
                .and_then(Value::as_dictionary)
                .and_then(|d| d.get("url"))
                .and_then(Value::as_string)
                .unwrap_or("");
            if url == "videoplayback:" {
                return 501;
            }
            match events.try_send(ReceiverEvent::UiRequested(url.to_owned())) {
                Ok(()) => 200,
                Err(_) => 503,
            }
        }
        Some(_) => 501,
        None => 400,
    }
}

fn accept(
    listener: TcpListener,
    peer: SocketAddr,
    cancel: &AtomicBool,
) -> Result<Option<TcpStream>> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cancel.load(Ordering::Acquire) && Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, remote)) => {
                if remote.ip() != peer.ip() {
                    continue;
                }
                stream.set_read_timeout(Some(Duration::from_millis(200)))?;
                stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                stream.set_nodelay(true)?;
                return Ok(Some(stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}
fn read_exact_cancel(s: &mut TcpStream, out: &mut [u8], cancel: &AtomicBool) -> Result<bool> {
    let mut offset = 0;
    while offset < out.len() {
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        match s.read(&mut out[offset..]) {
            Ok(0) => return Ok(false),
            Ok(n) => offset += n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

fn screen(
    w: &mut Workers,
    local: SocketAddr,
    peer: SocketAddr,
    key: [u8; 32],
    kind: u16,
    events: SyncSender<ReceiverEvent>,
) -> Result<u16> {
    let listener = TcpListener::bind(with_port(local, 0))?;
    let port = listener.local_addr()?.port();
    let cancel = w.cancel.clone();
    let sink = w.sink.clone();
    let failed = w.failure.clone();
    w.threads.push(thread::spawn(move || {
        let run = || -> Result<()> {
            let Some(mut stream) = accept(listener, peer, &cancel)? else {
                return Ok(());
            };
            let mut counter = 0u64;
            let mut first = true;
            loop {
                let mut header = [0u8; 128];
                if !read_exact_cancel(&mut stream, &mut header, &cancel)? {
                    break;
                }
                let (length, op, sender_ns) = media::screen_header(&header)?;
                let mut body = vec![0u8; length];
                if !read_exact_cancel(&mut stream, &mut body, &cancel)? {
                    break;
                }
                match op {
                    0 => {
                        ensure!(body.len() >= 16, "screen frame missing authentication tag");
                        let plain =
                            crypto::chacha_open(&key, &crypto::nonce64(counter), &body, &header)?;
                        counter = counter.checked_add(1).context("screen counter exhausted")?;
                        sink.send(MediaEvent::VideoFrame {
                            stream: kind,
                            data: media::annex_b(&plain)?,
                            sender_ns,
                        })
                        .map_err(anyhow::Error::msg)?;
                        if first {
                            event(&events, ReceiverEvent::FirstVideoFrame(kind));
                            first = false;
                        }
                    }
                    1 => {
                        let (codec, data) = media::codec_config(&body)?;
                        sink.send(MediaEvent::VideoConfig {
                            stream: kind,
                            codec,
                            data,
                        })
                        .map_err(anyhow::Error::msg)?;
                    }
                    _ => {}
                }
            }
            Ok(())
        };
        if let Err(e) = run()
            && !cancel.load(Ordering::Acquire)
        {
            event(&events, ReceiverEvent::Error(format!("screen {kind}: {e}")));
        }
        if !cancel.load(Ordering::Acquire) {
            failed.store(true, Ordering::Release);
        }
    }));
    Ok(port)
}

#[allow(clippy::too_many_arguments)]
fn audio(
    w: &mut Workers,
    local: SocketAddr,
    peer: SocketAddr,
    key: [u8; 32],
    kind: u16,
    audio_type: String,
    format: media::AudioFormat,
    mut microphone: Option<crate::microphone::Pending>,
) -> Result<(u16, u16)> {
    let data = UdpSocket::bind(with_port(local, 0))?;
    let control = UdpSocket::bind(with_port(local, 0))?;
    data.set_read_timeout(Some(Duration::from_millis(200)))?;
    let ports = (data.local_addr()?.port(), control.local_addr()?.port());
    // Opening an audio device can take hundreds of milliseconds. Complete it
    // before the SETUP reply lets the phone send high-rate wired PCM packets.
    w.sink
        .send(MediaEvent::AudioConfig {
            stream: kind,
            audio_type: audio_type.clone(),
            format,
        })
        .map_err(anyhow::Error::msg)?;
    let cancel = w.cancel.clone();
    let sink = w.sink.clone();
    let events = w.diagnostics.clone();
    let failed = w.failure.clone();
    w.threads.push(thread::spawn(move || {
        let _control = control;
        let mut buffer = [0u8; 65536];
        let mut replay = media::ReplayWindow::default();
        let mut capture: Option<Box<dyn media::CaptureSession>> = None;
        while !cancel.load(Ordering::Acquire) {
            let (n, remote) = match data.recv_from(&mut buffer) {
                Ok(value) => value,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    if let Some(events) = events.as_ref() {
                        event(
                            events,
                            ReceiverEvent::Error(format!("audio {kind}: {error}")),
                        );
                    }
                    failed.store(true, Ordering::Release);
                    break;
                }
            };
            if remote.ip() != peer.ip() || n < 36 || buffer[0] != 0x80 {
                continue;
            }
            let nonce_bytes = &buffer[n - 8..n];
            let counter = u64::from_le_bytes(nonce_bytes.try_into().unwrap());
            let Ok(payload) = crypto::chacha_open(
                &key,
                &crypto::nonce64(counter),
                &buffer[12..n - 8],
                &buffer[4..12],
            ) else {
                continue;
            };
            if payload.is_empty() || !replay.accept(counter) {
                continue;
            }
            if let Some(pending) = microphone.take() {
                match pending.start() {
                    Ok(session) => capture = Some(session),
                    Err(error) => {
                        if let Some(events) = events.as_ref() {
                            event(
                                events,
                                ReceiverEvent::Error(format!("microphone start: {error}")),
                            );
                        }
                        failed.store(true, Ordering::Release);
                        break;
                    }
                }
            }
            let timestamp = u32::from_be_bytes(buffer[4..8].try_into().unwrap());
            if let Err(error) = sink.send(MediaEvent::Audio {
                stream: kind,
                audio_type: audio_type.clone(),
                format,
                timestamp,
                data: payload,
            }) {
                if let Some(events) = events.as_ref() {
                    event(
                        events,
                        ReceiverEvent::Error(format!("audio sink {kind}: {error}")),
                    );
                }
                failed.store(true, Ordering::Release);
                break;
            }
        }
        if let Some(mut capture) = capture {
            capture.stop();
        }
    }));
    Ok(ports)
}

fn event_channel(
    w: &mut Workers,
    local: SocketAddr,
    peer: SocketAddr,
    shared: &[u8],
    slot: Arc<Mutex<Option<EventWriter>>>,
    events: SyncSender<ReceiverEvent>,
) -> Result<u16> {
    let listener = TcpListener::bind(with_port(local, 0))?;
    let port = listener.local_addr()?.port();
    let read_key = *crypto::derive_key(shared, b"Events-Salt", b"Events-Read-Encryption-Key")?;
    let write_key = *crypto::derive_key(shared, b"Events-Salt", b"Events-Write-Encryption-Key")?;
    let cancel = w.cancel.clone();
    let failed = w.failure.clone();
    w.threads.push(thread::spawn(move || {
        let run = || -> Result<()> {
            let Some(mut stream) = accept(listener, peer, &cancel)? else {
                return Ok(());
            };
            *slot
                .lock()
                .map_err(|_| anyhow::anyhow!("event lock poisoned"))? = Some(EventWriter {
                socket: stream.try_clone()?,
                cipher: ControlWriter::new(write_key),
                cseq: 0,
            });
            let mut reader = ControlReader::new(read_key);
            let mut parser = rtsp::Decoder::default();
            let mut buffer = [0u8; 16384];
            while !cancel.load(Ordering::Acquire) {
                match stream.read(&mut buffer) {
                    Ok(0) => {
                        reader.finish()?;
                        ensure!(parser.buffered_len() == 0, "truncated event RTSP message");
                        break;
                    }
                    Ok(n) => {
                        parser.push(&reader.decrypt(&buffer[..n])?)?;
                        while let Some(r) = parser.next_request()? {
                            if r.method.starts_with("RTSP/") || r.method.starts_with("HTTP/") {
                                continue;
                            }
                            let status = if r.method == "POST" && r.path == "/command" {
                                handle_command(&r.body, &events)
                            } else {
                                501
                            };
                            let response = rtsp::response(&r, status, None, &[]);
                            if let Some(writer) = slot
                                .lock()
                                .map_err(|_| anyhow::anyhow!("event lock poisoned"))?
                                .as_mut()
                            {
                                writer
                                    .socket
                                    .write_all(&writer.cipher.encrypt(&response)?)?;
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(())
        };
        if let Err(error) = run()
            && !cancel.load(Ordering::Acquire)
        {
            event(
                &events,
                ReceiverEvent::Error(format!("event channel: {error}")),
            );
        }
        if let Ok(mut writer) = slot.lock() {
            *writer = None;
        }
        if !cancel.load(Ordering::Acquire) {
            failed.store(true, Ordering::Release);
        }
    }));
    Ok(port)
}

fn ntp_now() -> u64 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (t.as_secs().wrapping_add(2_208_988_800) << 32)
        | (((t.subsec_nanos() as u64) << 32) / 1_000_000_000)
}
fn timing(w: &mut Workers, local: SocketAddr, peer: SocketAddr, port: Option<u16>) -> Result<u16> {
    let socket = UdpSocket::bind(with_port(local, 0))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let assigned = socket.local_addr()?.port();
    let cancel = w.cancel.clone();
    w.threads.push(thread::spawn(move || {
        let mut last = Instant::now() - Duration::from_secs(2);
        let mut buf = [0u8; 256];
        while !cancel.load(Ordering::Acquire) {
            if last.elapsed() >= Duration::from_secs(1) {
                if let Some(port) = port.filter(|p| *p > 0) {
                    let mut packet = [0u8; 32];
                    packet[..4].copy_from_slice(&[0x80, 210, 0, 7]);
                    packet[24..].copy_from_slice(&ntp_now().to_be_bytes());
                    let _ = socket.send_to(&packet, with_port(peer, port));
                }
                last = Instant::now();
            }
            if let Ok((n, remote)) = socket.recv_from(&mut buf)
                && remote.ip() == peer.ip()
                && n == 32
                && buf[1] == 210
            {
                let mut reply = [0u8; 32];
                reply[..4].copy_from_slice(&[0x80, 211, 0, 7]);
                reply[8..16].copy_from_slice(&buf[24..32]);
                reply[16..24].copy_from_slice(&ntp_now().to_be_bytes());
                reply[24..].copy_from_slice(&ntp_now().to_be_bytes());
                let _ = socket.send_to(&reply, remote);
            }
        }
    }));
    Ok(assigned)
}

#[cfg(test)]
mod tests {
    #[test]
    fn changing_port_preserves_usb_ipv6_scope() {
        let address = std::net::SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::1234".parse().unwrap(),
            7000,
            9,
            7,
        ));
        let std::net::SocketAddr::V6(result) = super::with_port(address, 6000) else {
            panic!("IPv6 expected")
        };
        assert_eq!(result.port(), 6000);
        assert_eq!(result.scope_id(), 7);
        assert_eq!(result.flowinfo(), 9);
    }

    use super::*;
    struct NoAuth;
    impl AuthProvider for NoAuth {
        fn protocol_major(&self) -> u8 {
            3
        }
        fn certificate_type(&self) -> carplay_auth::CertificateType {
            carplay_auth::CertificateType::Mfi
        }
        fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
            Ok(vec![])
        }
        fn sign_challenge(&self, _: &[u8]) -> carplay_auth::Result<Vec<u8>> {
            Err(carplay_auth::AuthError::Authentication)
        }
    }
    struct NoMedia;
    impl MediaSink for NoMedia {
        fn send(&self, _: MediaEvent) -> std::result::Result<(), String> {
            Err("no media".into())
        }
    }

    fn with_audio_setup_context<T>(
        sink: Arc<dyn MediaSink>,
        events: SyncSender<ReceiverEvent>,
        run: impl FnOnce(&ContextState<'_>) -> T,
    ) -> T {
        let dir = tempfile::tempdir().unwrap();
        let options = ReceiverOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            config: ReceiverConfig::default(),
            bluetooth_address: "02:00:00:00:00:01".into(),
            state_dir: dir.path().into(),
            advertise: false,
        };
        let (identity, pairings) = FilePairingStore::open(dir.path()).unwrap();
        let (_, commands) = mpsc::sync_channel(1);
        let overflow = AtomicBool::new(false);
        run(&ContextState {
            options: &options,
            identity,
            pairings,
            auth: Arc::new(NoAuth),
            sink,
            stop: Arc::new(AtomicBool::new(false)),
            events,
            commands: &commands,
            input_overflow: &overflow,
            iap: None,
            endpoint: None,
            capture: None,
        })
    }

    fn pcm_stream(kind: u16, seed: u64) -> Value {
        dict([
            ("type", number(kind.into())),
            ("audioType", text("media")),
            ("audioFormat", number(0x8000)),
            ("streamConnectionID", number(seed)),
        ])
    }

    #[test]
    fn audio_setup_waits_for_ready_sink_before_reply_and_udp_delivery() {
        struct GatedSink {
            events: SyncSender<MediaEvent>,
            ready: Mutex<Receiver<()>>,
        }
        impl MediaSink for GatedSink {
            fn send(&self, event: MediaEvent) -> std::result::Result<(), String> {
                let configure = matches!(event, MediaEvent::AudioConfig { .. });
                self.events.try_send(event).map_err(|e| e.to_string())?;
                if configure {
                    self.ready
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(3))
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            }
        }
        let (media_tx, media_rx) = mpsc::sync_channel(8);
        let (ready, gate) = mpsc::sync_channel(1);
        let sink = Arc::new(GatedSink {
            events: media_tx,
            ready: Mutex::new(gate),
        });
        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let (stop, stopped) = mpsc::sync_channel(1);
        let shared = [7u8; 32];
        let worker = thread::spawn(move || {
            with_audio_setup_context(sink, events_tx, |ctx| {
                let mut workers = Workers::new(ctx.sink.clone());
                let response = setup_streams(
                    &[pcm_stream(100, 17)],
                    &mut workers,
                    ctx,
                    ctx.options.bind,
                    "127.0.0.1:1".parse().unwrap(),
                    &shared,
                    Arc::new(Mutex::new(None)),
                );
                reply_tx.send(response).unwrap();
                stopped.recv_timeout(Duration::from_secs(3)).unwrap();
            });
        });
        let format = media::AudioFormat::from_bits(0x8000).unwrap();
        assert!(matches!(
            media_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            MediaEvent::AudioConfig { stream: 100, audio_type, format: actual }
                if audio_type == "media" && actual == format
        ));
        // setup_streams supplies the actual SETUP reply body. It must not return
        // ports or report StreamStarted while the output device is still opening.
        assert!(matches!(
            reply_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(events_rx.try_recv().is_err());
        assert!(media_rx.try_recv().is_err());
        ready.send(()).unwrap();
        let (status, replies) = reply_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        assert_eq!(status, 200);
        assert!(matches!(
            events_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            ReceiverEvent::StreamStarted(100)
        ));
        let reply = replies[0].as_dictionary().unwrap();
        let data_port = info::get_number(reply, "dataPort").unwrap() as u16;
        let control_port = info::get_number(reply, "controlPort").unwrap() as u16;
        let key = crypto::derive_key(
            &shared,
            b"DataStream-Salt17",
            b"DataStream-Output-Encryption-Key",
        )
        .unwrap();
        let payload = [0, 1, 0, 2];
        let mut wire = vec![0x80, 100, 0, 1, 0, 0, 0, 50, 1, 2, 3, 4];
        wire.extend(
            crypto::chacha_seal(&key, &crypto::nonce64(0), &payload, &wire[4..12]).unwrap(),
        );
        wire.extend(0u64.to_le_bytes());
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .send_to(&wire, ("127.0.0.1", data_port))
            .unwrap();
        assert!(matches!(
            media_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            MediaEvent::Audio { stream: 100, audio_type, format: actual, timestamp: 50, data }
                if audio_type == "media" && actual == format && data == payload
        ));
        stop.send(()).unwrap();
        worker.join().unwrap();
        assert!(matches!(
            media_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            MediaEvent::Stop { stream: 100 }
        ));
        let _data = UdpSocket::bind(("127.0.0.1", data_port)).unwrap();
        let _control = UdpSocket::bind(("127.0.0.1", control_port)).unwrap();
    }

    #[test]
    fn failed_audio_configuration_does_not_start_stream_and_session_drop_releases_ports() {
        struct FailingSink(SyncSender<MediaEvent>);
        impl MediaSink for FailingSink {
            fn send(&self, event: MediaEvent) -> std::result::Result<(), String> {
                let fail = matches!(event, MediaEvent::AudioConfig { stream: 101, .. });
                self.0.try_send(event).map_err(|e| e.to_string())?;
                if fail {
                    Err("test output device failed to initialize".into())
                } else {
                    Ok(())
                }
            }
        }
        let (media_tx, media_rx) = mpsc::sync_channel(8);
        let (events_tx, events_rx) = mpsc::sync_channel(8);
        let ports = with_audio_setup_context(Arc::new(FailingSink(media_tx)), events_tx, |ctx| {
            let mut workers = Workers::new(ctx.sink.clone());
            let (_, replies) = setup_streams(
                &[pcm_stream(100, 17)],
                &mut workers,
                ctx,
                ctx.options.bind,
                "127.0.0.1:1".parse().unwrap(),
                &[7; 32],
                Arc::new(Mutex::new(None)),
            )
            .unwrap();
            let error = setup_streams(
                &[pcm_stream(101, 18)],
                &mut workers,
                ctx,
                ctx.options.bind,
                "127.0.0.1:1".parse().unwrap(),
                &[7; 32],
                Arc::new(Mutex::new(None)),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("test output device failed to initialize")
            );
            assert_eq!(workers.streams.len(), 1);
            assert_eq!(workers.streams[0].0.kind, 100);
            // A setup error unwinds the owning connection and drops Workers,
            // including streams that had succeeded in earlier SETUP requests.
            let reply = replies[0].as_dictionary().unwrap();
            (
                info::get_number(reply, "dataPort").unwrap() as u16,
                info::get_number(reply, "controlPort").unwrap() as u16,
            )
        });
        assert!(matches!(
            events_rx.try_recv().unwrap(),
            ReceiverEvent::StreamStarted(100)
        ));
        assert!(
            events_rx.try_recv().is_err(),
            "failed stream must not start"
        );
        let events: Vec<_> = media_rx.try_iter().collect();
        assert!(matches!(
            events.first(),
            Some(MediaEvent::AudioConfig { stream: 100, .. })
        ));
        assert!(matches!(
            events.get(1),
            Some(MediaEvent::AudioConfig { stream: 101, .. })
        ));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, MediaEvent::Stop { stream: 100 }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, MediaEvent::Audio { .. }))
        );
        let _data = UdpSocket::bind(("127.0.0.1", ports.0)).unwrap();
        let _control = UdpSocket::bind(("127.0.0.1", ports.1)).unwrap();
    }

    #[test]
    fn localhost_control_rejects_setup_before_verification_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = start(
            ReceiverOptions {
                bind: "127.0.0.1:0".parse().unwrap(),
                config: ReceiverConfig::default(),
                bluetooth_address: "02:00:00:00:00:01".into(),
                state_dir: dir.path().into(),
                advertise: false,
            },
            Arc::new(NoAuth),
            Arc::new(NoMedia),
        )
        .unwrap();
        let mut client = TcpStream::connect(server.address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client
            .write_all(b"SETUP / RTSP/1.0\r\nCSeq: 7\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut buf = [0u8; 512];
        let n = client.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("401 Unauthorized"));
        assert!(String::from_utf8_lossy(&buf[..n]).contains("CSeq: 7"));
        server.stop();
    }

    #[test]
    fn hid_queue_overflow_disconnects_and_clears_on_reconnect() {
        struct GatedAuth {
            entered: SyncSender<()>,
            resume: Mutex<Receiver<()>>,
        }
        impl AuthProvider for GatedAuth {
            fn protocol_major(&self) -> u8 {
                3
            }
            fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
                self.entered.send(()).unwrap();
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
                Ok(vec![1])
            }
            fn sign_challenge(&self, _: &[u8]) -> carplay_auth::Result<Vec<u8>> {
                Err(carplay_auth::AuthError::Authentication)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (entered, blocked) = mpsc::sync_channel(1);
        let (resume, gate) = mpsc::sync_channel(1);
        let mut server = start(
            ReceiverOptions {
                bind: "127.0.0.1:0".parse().unwrap(),
                config: ReceiverConfig::default(),
                bluetooth_address: "02:00:00:00:00:01".into(),
                state_dir: dir.path().into(),
                advertise: false,
            },
            Arc::new(GatedAuth {
                entered,
                resume: Mutex::new(gate),
            }),
            Arc::new(NoMedia),
        )
        .unwrap();
        let mut client = TcpStream::connect(server.address).unwrap();
        let (_, public) = crypto::x25519_generate().unwrap();
        client
            .write_all(
                &[
                    b"POST /auth-setup RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 33\r\n\r\n"
                        .as_slice(),
                    &[1],
                    &public,
                ]
                .concat(),
            )
            .unwrap();
        // Hold the control consumer so queue pressure is deterministic.
        blocked.recv_timeout(Duration::from_secs(3)).unwrap();
        let uid = carplay_core::input::TOUCH_UID;
        for _ in 0..128 {
            assert!(server.hid(uid, vec![0, 1, 50, 0, 60, 0, 1, 0, 0, 0, 0, 0]));
        }
        let release = vec![0, 0, 50, 0, 60, 0, 1, 0, 0, 0, 0, 0];
        assert!(!server.hid(uid, release));
        resume.send(()).unwrap();
        let mut overflow_reported = false;
        loop {
            match server.events.recv_timeout(Duration::from_secs(3)).unwrap() {
                ReceiverEvent::Error(error) => {
                    assert!(error.contains("input queue overflow"), "{error}");
                    overflow_reported = true;
                }
                ReceiverEvent::Disconnected => break,
                _ => {}
            }
        }
        assert!(
            overflow_reported,
            "a lost release must be visible to the caller"
        );
        assert!(server.input_overflow.load(Ordering::Acquire));

        let mut reconnected = TcpStream::connect(server.address).unwrap();
        reconnected
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        reconnected
            .write_all(b"GET /info RTSP/1.0\r\nCSeq: 2\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut buf = [0; 512];
        let n = reconnected.read(&mut buf).unwrap();
        assert!(String::from_utf8_lossy(&buf[..n]).contains("200 OK"));
        assert!(!server.input_overflow.load(Ordering::Acquire));
        assert!(
            server.events.try_iter().all(|event| !matches!(
                event,
                ReceiverEvent::Error(_) | ReceiverEvent::Disconnected
            ))
        );
        server.stop();
    }

    #[test]
    fn diagnostic_events_redact_phone_data() {
        let event = ReceiverEvent::UiRequested("carplay:private-destination".into());
        assert!(!format!("{event:?}").contains("private-destination"));
        let event = ReceiverEvent::IapMessage {
            message_id: 0x5001,
            body: b"private-navigation".to_vec(),
        };
        let debug = format!("{event:?}");
        assert!(debug.contains("body_bytes"));
        assert!(!debug.contains("private-navigation"));
    }
}
