// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9:
// shared/src/main/java/com/shilapi/xcertplay/transport/{Iap2WirelessControlClient,Iap2IdentificationClient,Iap2LinkChannel}.kt
// shared/src/main/java/com/shilapi/xcertplay/mfi/Iap2MfiAuthenticationClient.kt
//! Bluetooth/USB iAP2 bootstrap with authentication, bounded queues and cancellation.
//! The caller owns Wi-Fi/mDNS/AirPlay startup and supplies its actual endpoint.
//! A successful iAP2 bootstrap is NOT evidence that AirPlay rendered a frame.
use carplay_auth::{AuthProvider, CertificateType};
use carplay_platform::{ReadWrite, bluetooth::RfcommStream};
use carplay_protocol::{
    file_transfer::{Artwork, FileTransferReceiver},
    iap2,
    tlv::{self, ControlMessage, Identification, IdentificationTransport, Parameter},
};
use std::{
    collections::VecDeque,
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub const IAP2_IPHONE_SERVICE_UUID: &str = "00000000-deca-fade-deca-deafdecacafe";
/// Bootstrap capabilities; incoming metadata is forwarded to the application.
pub const SENT_MESSAGES: &[u16] = &[
    0xaa01, 0xaa03, 0x5000, 0x5002, 0x5200, 0x5203, 0xae00, 0xae02, 0x4157, 0x4159, 0x4154, 0x4156,
    0x4301, 0x5703,
];
pub const RECEIVED_MESSAGES: &[u16] = &[
    0xaa00, 0xaa02, 0xaa04, 0xaa05, 0xea00, 0xea01, 0x5001, 0x5201, 0x5202, 0xae01, 0x4158, 0x4155,
    0x4300, 0x4e0d, 0x4e0e, 0x5702,
];
const MAX_PENDING_MESSAGES: usize = 64;
const MAX_PENDING_BYTES: usize = 1_048_576;
const MAX_EVENTS: usize = 256;
const MAX_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointTransport {
    Wireless,
    /// USB iAP2 control with an IPv6 CDC-NCM media network.
    Wired,
}

/// Never implement Display/serialization for this object: it contains Wi-Fi credentials.
#[derive(Clone)]
pub struct Endpoint {
    pub transport: EndpointTransport,
    pub ssid: String,
    pub passphrase: Zeroizing<String>,
    pub channel: u8,
    /// iAP2 values: 0 open, 1 WEP, 2 WPA/WPA2, 3 transition, 4 WPA3-only.
    pub security_type: u8,
    pub ip_addresses: Vec<String>,
    pub airplay_port: u16,
    pub device_identifier: String,
    pub public_key: String,
    pub source_version: String,
    pub access_point_bssid: Option<[u8; 6]>,
}
impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("transport", &self.transport)
            .field("network", &"[redacted]")
            .field("airplay_port", &self.airplay_port)
            .field("address_count", &self.ip_addresses.len())
            .finish_non_exhaustive()
    }
}
impl Endpoint {
    pub fn validate(&self) -> Result<()> {
        if self.ip_addresses.is_empty()
            || self.airplay_port == 0
            || self.device_identifier.trim().is_empty()
            || self.public_key.is_empty()
            || self.source_version.is_empty()
        {
            return Err(Error::Configuration("incomplete CarPlay endpoint"));
        }
        match self.transport {
            EndpointTransport::Wireless
                if self.ssid.trim().is_empty()
                    || self.security_type > 4
                    || (self.security_type != 0 && self.passphrase.is_empty()) =>
            {
                return Err(Error::Configuration("incomplete wireless endpoint"));
            }
            EndpointTransport::Wired
                if !self.ssid.is_empty()
                    || !self.passphrase.is_empty()
                    || self.access_point_bssid.is_some() =>
            {
                return Err(Error::Configuration(
                    "wired endpoint must not contain Wi-Fi credentials",
                ));
            }
            _ => {}
        }
        for value in std::iter::once(&self.ssid)
            .chain(std::iter::once(&*self.passphrase))
            .chain(self.ip_addresses.iter())
            .chain([
                &self.device_identifier,
                &self.public_key,
                &self.source_version,
            ])
        {
            if value.contains('\0') {
                return Err(Error::Configuration("embedded NUL in endpoint"));
            }
        }
        for address in &self.ip_addresses {
            // Zone identifiers belong to local socket binding and are never sent to the iPhone.
            if address.parse::<std::net::IpAddr>().is_err() {
                return Err(Error::Configuration(
                    "endpoint address must be an IP literal without a zone",
                ));
            }
            if self.transport == EndpointTransport::Wired
                && address.parse::<std::net::Ipv6Addr>().is_err()
            {
                return Err(Error::Configuration(
                    "wired endpoint requires IPv6 addresses",
                ));
            }
        }
        Ok(())
    }
    pub fn wifi(&self) -> Result<ControlMessage> {
        if self.transport == EndpointTransport::Wired {
            return Err(Error::Configuration(
                "wired endpoint has no Wi-Fi configuration",
            ));
        }
        Ok(tlv::wifi_configuration(
            &self.ssid,
            &self.passphrase,
            self.channel,
            self.security_type,
            self.access_point_bssid,
        )?)
    }
    pub fn start_session(&self) -> Result<ControlMessage> {
        self.validate()?;
        Ok(tlv::StartSession {
            airplay_port: self.airplay_port,
            public_key: self.public_key.clone(),
            source_version: self.source_version.clone(),
            wired_ipv6_addresses: if self.transport == EndpointTransport::Wired {
                self.ip_addresses.clone()
            } else {
                Vec::new()
            },
            wired_reserved: None,
            wireless: (self.transport == EndpointTransport::Wireless).then(|| {
                tlv::WirelessSession {
                    ssid: self.ssid.clone(),
                    passphrase: self.passphrase.to_string(),
                    channel: self.channel,
                    ip_addresses: self.ip_addresses.clone(),
                    security_type: self.security_type,
                }
            }),
            device_identifier: Some(self.device_identifier.clone()),
            sdk_version: None,
            cluster_asset: None,
            mutual_auth: None,
        }
        .build()?)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Total bootstrap deadline; ongoing control extends only with an external live-session proof.
    pub timeout: Duration,
    pub link_config: iap2::Config,
    /// Send only implemented/advertised update subscriptions.
    pub subscribe_to_updates: bool,
    /// Wired-only accessory power budget, in milliamps. Zero is the conservative
    /// default: this application promises no extra current beyond the OS-managed
    /// USB link. Set a nonzero value only from an actual platform power contract.
    pub available_current_milliamps: u16,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            link_config: iap2::Config {
                max_outgoing: 4,
                control_session_version: 2,
                ..Default::default()
            },
            subscribe_to_updates: true,
            available_current_milliamps: 0,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    LinkNegotiating,
    Identifying,
    Authenticating,
    Running,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Cancelled,
    TimedOut,
    ChannelClosed,
    Failed,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub identified: bool,
    pub authenticated: bool,
    pub subscribed: bool,
    pub wifi_configurations_sent: u32,
    pub post_transport_wifi_configurations_sent: u32,
    pub start_sessions_sent: u32,
    pub transport_notification_seen: bool,
    pub wireless_available_seen: bool,
    pub forwarded_messages: u32,
}
/// Debug prints only message IDs/sizes, never certificates, network credentials or raw phone data.
pub enum Event {
    StateChanged(State),
    MessageReceived { message_id: u16 },
    MessageSent { message_id: u16, at_ms: u64 },
    Authenticated,
    Subscribed,
    WirelessAvailability(bool),
    TransportNotified,
    Incoming(ControlMessage),
    SessionData { session_id: u8, bytes: Vec<u8> },
    Artwork(Artwork),
    Stopped(Terminal),
}
impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateChanged(state) => f.debug_tuple("StateChanged").field(state).finish(),
            Self::MessageReceived { message_id } => f
                .debug_struct("MessageReceived")
                .field("message_id", message_id)
                .finish(),
            Self::MessageSent { message_id, at_ms } => f
                .debug_struct("MessageSent")
                .field("message_id", message_id)
                .field("at_ms", at_ms)
                .finish(),
            Self::Authenticated => f.write_str("Authenticated"),
            Self::Subscribed => f.write_str("Subscribed"),
            Self::WirelessAvailability(value) => {
                f.debug_tuple("WirelessAvailability").field(value).finish()
            }
            Self::TransportNotified => f.write_str("TransportNotified"),
            Self::Incoming(message) => f
                .debug_struct("Incoming")
                .field("message_id", &message.message_id)
                .field("body_bytes", &message.body.len())
                .finish(),
            Self::SessionData { session_id, bytes } => f
                .debug_struct("SessionData")
                .field("session_id", session_id)
                .field("bytes", &bytes.len())
                .finish(),
            Self::Artwork(artwork) => f.debug_tuple("Artwork").field(artwork).finish(),
            Self::Stopped(terminal) => f.debug_tuple("Stopped").field(terminal).finish(),
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid CarPlay endpoint configuration: {0}")]
    Configuration(&'static str),
    #[error("iAP2 protocol error: {0}")]
    Protocol(#[from] carplay_protocol::Error),
    #[error("accessory authentication operation failed")]
    Authentication,
    #[error("iPhone rejected accessory authentication")]
    AuthenticationRejected,
    #[error("iPhone rejected identification parameters {parameter_ids:#06x?}")]
    IdentificationRejected { parameter_ids: Vec<u16> },
    #[error("unexpected message 0x{message_id:04x} while {state:?}")]
    UnexpectedMessage { message_id: u16, state: State },
    #[error("wireless session buffering limit exceeded")]
    QueueLimit,
    #[error("iAP2 link failed")]
    LinkFailed,
    #[error("transport I/O failed: {0}")]
    Io(#[from] io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
struct Outbound {
    message_id: Option<u16>,
    session_id: u8,
    bytes: Zeroizing<Vec<u8>>,
    offset: usize,
    post_transport_wifi: bool,
    last_subscription: bool,
}
struct Written {
    message_id: u16,
    post_transport_wifi: bool,
    last_subscription: bool,
}

pub struct Coordinator {
    endpoint: Endpoint,
    identification: Identification,
    auth: Arc<dyn AuthProvider>,
    options: Options,
    state: State,
    terminal: Option<Terminal>,
    progress: Progress,
    link: iap2::LinkEngine,
    csm: tlv::ControlDecoder,
    files: FileTransferReceiver,
    events: VecDeque<Event>,
    pending: VecDeque<Outbound>,
    pending_bytes: usize,
    pending_written: Vec<Written>,
    inflight_written: Vec<Written>,
    output_inflight: bool,
    deadline: Option<u64>,
    extended: bool,
    live_session: bool,
    identification_sent: bool,
    certificate_sent: bool,
    challenge_signed: bool,
    pre_wifi_queued: u32,
    post_wifi_queued: u32,
}
impl Coordinator {
    pub fn new(
        endpoint: Endpoint,
        identification: Identification,
        auth: Arc<dyn AuthProvider>,
        options: Options,
    ) -> Result<Self> {
        endpoint.validate()?;
        if options.timeout.is_zero() || options.timeout > Duration::from_secs(24 * 60 * 60) {
            return Err(Error::Configuration("timeout must be 1 ms to 24 hours"));
        }
        match (&identification.transport, endpoint.transport) {
            (IdentificationTransport::Wireless { ssid, .. }, EndpointTransport::Wireless)
                if ssid == &endpoint.ssid => {}
            (IdentificationTransport::Wired { .. }, EndpointTransport::Wired) => {}
            _ => {
                return Err(Error::Configuration(
                    "identification and endpoint transports differ",
                ));
            }
        }
        if endpoint.transport == EndpointTransport::Wired
            && !identification.sent_messages.contains(&0xae03)
        {
            return Err(Error::Configuration(
                "wired identification must declare PowerSourceUpdate",
            ));
        }
        // The bootstrap never claims runtime location or vehicle capabilities.
        if identification
            .sent_messages
            .iter()
            .any(|id| [0xfffb, 0xa101].contains(id))
            || identification
                .received_messages
                .iter()
                .any(|id| [0xfffa, 0xfffc, 0xa100, 0xa102].contains(id))
            || identification
                .extra_components
                .iter()
                .any(|p| [20, 21, 22].contains(&p.id))
        {
            return Err(Error::Configuration(
                "runtime vehicle/location belongs on the AirPlay tunnel",
            ));
        }
        identification.build()?;
        let link = iap2::LinkEngine::new(options.link_config.clone())?;
        Ok(Self {
            endpoint,
            identification,
            auth,
            options,
            state: State::Idle,
            terminal: None,
            progress: Progress::default(),
            link,
            csm: Default::default(),
            files: Default::default(),
            events: VecDeque::new(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            pending_written: Vec::new(),
            inflight_written: Vec::new(),
            output_inflight: false,
            deadline: None,
            extended: false,
            live_session: false,
            identification_sent: false,
            certificate_sent: false,
            challenge_signed: false,
            pre_wifi_queued: 0,
            post_wifi_queued: 0,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn terminal(&self) -> Option<Terminal> {
        self.terminal
    }
    pub fn progress(&self) -> &Progress {
        &self.progress
    }
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
    pub fn start(&mut self, now_ms: u64) -> Result<()> {
        if self.state != State::Idle {
            return Err(Error::Configuration("coordinator already started"));
        }
        self.deadline = Some(now_ms.saturating_add(self.options.timeout.as_millis().max(1) as u64));
        self.change_state(State::LinkNegotiating)?;
        self.link.start(false, now_ms);
        self.drive(now_ms)
    }
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) -> Result<()> {
        if self.terminal.is_some() {
            return Ok(());
        }
        self.advance_time(now_ms)?;
        if self.terminal.is_some() {
            return Ok(());
        }
        self.link.feed(bytes, now_ms);
        let result = self.drive(now_ms);
        self.fail_if_error(result)
    }
    pub fn advance_time(&mut self, now_ms: u64) -> Result<()> {
        if self.terminal.is_some() {
            return Ok(());
        }
        if self.extended && !self.live_session {
            self.finish(Terminal::TimedOut);
            return Ok(());
        }
        if self.deadline.is_some_and(|deadline| deadline <= now_ms) {
            if self.progress.authenticated
                && self.progress.start_sessions_sent > 0
                && self.live_session
            {
                self.extended = true;
                self.deadline = None;
            } else {
                self.finish(Terminal::TimedOut);
                return Ok(());
            }
        }
        self.link.advance_time(now_ms);
        let result = self.drive(now_ms);
        self.fail_if_error(result)
    }
    /// This flag must come from the current AirPlay session's authenticated/rendered state.
    /// Neither an outgoing 0x4301 nor an iAP2 success message proves a live session.
    pub fn set_live_session(&mut self, live: bool) {
        self.live_session = live;
    }
    pub fn next_deadline_ms(&self) -> Option<u64> {
        [self.deadline, self.link.next_deadline_ms()]
            .into_iter()
            .flatten()
            .min()
    }
    /// Drain one output batch. Call `confirm_output_written` ONLY after write_all + flush succeeds.
    pub fn take_output(&mut self) -> Result<Zeroizing<Vec<u8>>> {
        if self.output_inflight {
            return Err(Error::Configuration("previous output batch not confirmed"));
        }
        let bytes = self.link.take_output();
        if !bytes.is_empty() {
            self.output_inflight = true;
            self.inflight_written = std::mem::take(&mut self.pending_written);
        }
        Ok(Zeroizing::new(bytes))
    }
    pub fn confirm_output_written(&mut self, now_ms: u64) -> Result<()> {
        if !self.output_inflight {
            return Ok(());
        }
        self.output_inflight = false;
        for sent in std::mem::take(&mut self.inflight_written) {
            match sent.message_id {
                0x5703 => {
                    self.progress.wifi_configurations_sent += 1;
                    if sent.post_transport_wifi {
                        self.progress.post_transport_wifi_configurations_sent += 1;
                    }
                }
                0x4301 => self.progress.start_sessions_sent += 1,
                _ => {}
            }
            self.emit(Event::MessageSent {
                message_id: sent.message_id,
                at_ms: now_ms,
            })?;
            if sent.last_subscription {
                self.progress.subscribed = true;
                self.emit(Event::Subscribed)?;
            }
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        self.finish(Terminal::Cancelled);
    }
    pub fn channel_closed(&mut self) {
        self.finish(Terminal::ChannelClosed);
    }
    fn fail_if_error(&mut self, result: Result<()>) -> Result<()> {
        if result.is_err() {
            self.finish(Terminal::Failed);
        }
        result
    }
    fn change_state(&mut self, state: State) -> Result<()> {
        self.state = state;
        self.emit(Event::StateChanged(state))
    }
    fn emit(&mut self, event: Event) -> Result<()> {
        if self.events.len() >= MAX_EVENTS {
            self.finish(Terminal::Failed);
            return Err(Error::QueueLimit);
        }
        self.events.push_back(event);
        Ok(())
    }
    fn finish(&mut self, terminal: Terminal) {
        if self.terminal.is_some() {
            return;
        }
        self.state = State::Stopped;
        self.terminal = Some(terminal);
        self.deadline = None;
        self.link.feed_eof();
        self.pending.clear();
        self.pending_bytes = 0;
        self.pending_written.clear();
        self.inflight_written.clear();
        self.output_inflight = false;
        self.csm = Default::default();
        self.files.clear();
        self.events.clear();
        self.events.push_back(Event::Stopped(terminal));
    }
    fn drive(&mut self, now_ms: u64) -> Result<()> {
        while let Some(event) = self.link.poll_event() {
            match event {
                iap2::Event::Writable(true) if self.state == State::LinkNegotiating => {
                    self.change_state(State::Identifying)?
                }
                iap2::Event::Control(bytes) => {
                    for message in self.csm.offer(&bytes) {
                        self.handle(message)?;
                    }
                }
                iap2::Event::Session { session_id, bytes } => {
                    if session_id == iap2::FILE_TRANSFER_SESSION {
                        let outcome = self.files.accept(&bytes);
                        for reply in outcome.replies {
                            self.enqueue_file_reply(reply)?;
                        }
                        if let Some(artwork) = outcome.completed {
                            self.emit(Event::Artwork(artwork))?;
                        }
                    } else {
                        self.emit(Event::SessionData { session_id, bytes })?;
                    }
                }
                iap2::Event::Dead(_) => return Err(Error::LinkFailed),
                _ => {}
            }
        }
        self.flush_pending(now_ms)
    }
    fn enqueue(
        &mut self,
        message: ControlMessage,
        post_transport_wifi: bool,
        last_subscription: bool,
    ) -> Result<()> {
        let bytes = Zeroizing::new(message.encode()?);
        if self.pending.len() >= MAX_PENDING_MESSAGES
            || bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.pending_bytes)
        {
            return Err(Error::QueueLimit);
        }
        self.pending_bytes += bytes.len();
        self.pending.push_back(Outbound {
            message_id: Some(message.message_id),
            session_id: iap2::CONTROL_SESSION,
            bytes,
            offset: 0,
            post_transport_wifi,
            last_subscription,
        });
        Ok(())
    }
    fn enqueue_file_reply(&mut self, bytes: Vec<u8>) -> Result<()> {
        if self.pending.len() >= MAX_PENDING_MESSAGES
            || bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.pending_bytes)
        {
            return Err(Error::QueueLimit);
        }
        self.pending_bytes += bytes.len();
        self.pending.push_back(Outbound {
            message_id: None,
            session_id: iap2::FILE_TRANSFER_SESSION,
            bytes: Zeroizing::new(bytes),
            offset: 0,
            post_transport_wifi: false,
            last_subscription: false,
        });
        Ok(())
    }
    fn flush_pending(&mut self, now_ms: u64) -> Result<()> {
        while self.link.writable() {
            let Some(front) = self.pending.front_mut() else {
                break;
            };
            let limit = self
                .link
                .peer_synchronization()
                .ok_or(Error::LinkFailed)?
                .max_length as usize
                - 10;
            let end = (front.offset + limit).min(front.bytes.len());
            if front.session_id != iap2::CONTROL_SESSION && end != front.bytes.len() {
                return Err(carplay_protocol::Error::Limit.into());
            }
            self.link
                .send_session(front.session_id, &front.bytes[front.offset..end], now_ms)?;
            front.offset = end;
            if end == front.bytes.len() {
                let done = self.pending.pop_front().expect("front exists");
                self.pending_bytes -= done.bytes.len();
                if let Some(message_id) = done.message_id {
                    self.pending_written.push(Written {
                        message_id,
                        post_transport_wifi: done.post_transport_wifi,
                        last_subscription: done.last_subscription,
                    });
                }
            }
        }
        Ok(())
    }
    fn handle(&mut self, message: ControlMessage) -> Result<()> {
        self.emit(Event::MessageReceived {
            message_id: message.message_id,
        })?;
        match self.state {
            State::Identifying => match message.message_id {
                tlv::START_IDENTIFICATION => {
                    self.enqueue(self.identification.build()?, false, false)?;
                    self.identification_sent = true;
                }
                tlv::IDENTIFICATION_ACCEPTED if self.identification_sent => {
                    self.progress.identified = true;
                    self.change_state(State::Authenticating)?;
                }
                tlv::IDENTIFICATION_REJECTED => {
                    let mut parameter_ids: Vec<_> =
                        message.parameters()?.iter().map(|p| p.id).collect();
                    parameter_ids.sort_unstable();
                    parameter_ids.dedup();
                    return Err(Error::IdentificationRejected { parameter_ids });
                }
                _ => return Err(self.unexpected(message.message_id)),
            },
            State::Authenticating => match message.message_id {
                tlv::REQUEST_CERTIFICATE => {
                    let certificate = match self.auth.certificate_type() {
                        CertificateType::Mfi => {
                            let bytes =
                                self.auth.certificate().map_err(|_| Error::Authentication)?;
                            if bytes.is_empty() {
                                return Err(Error::Authentication);
                            }
                            tlv::accessory_certificate(&bytes)?
                        }
                        CertificateType::Baa => {
                            let certs = self
                                .auth
                                .baa_certificates()
                                .map_err(|_| Error::Authentication)?;
                            if certs.leaf.is_empty() || certs.intermediate.is_empty() {
                                return Err(Error::Authentication);
                            }
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
                    self.enqueue(certificate, false, false)?;
                    self.certificate_sent = true;
                }
                tlv::REQUEST_CHALLENGE_RESPONSE if self.certificate_sent => {
                    let challenge = Zeroizing::new(tlv::authentication_challenge(&message)?);
                    let signature = Zeroizing::new(
                        self.auth
                            .sign_challenge(&challenge)
                            .map_err(|_| Error::Authentication)?,
                    );
                    if signature.is_empty() {
                        return Err(Error::Authentication);
                    }
                    self.enqueue(tlv::authentication_response(&signature)?, false, false)?;
                    self.challenge_signed = true;
                }
                tlv::AUTHENTICATION_SUCCEEDED if self.challenge_signed => {
                    self.progress.authenticated = true;
                    self.emit(Event::Authenticated)?;
                    self.change_state(State::Running)?;
                    if self.endpoint.transport == EndpointTransport::Wired {
                        // DiPlay's wired sequence announces power before the
                        // subscriptions. The battery-charging field is optional;
                        // omit it because a USB data link cannot establish that
                        // the phone battery is currently charging.
                        self.enqueue(
                            ControlMessage::new(
                                0xae03,
                                &[Parameter::u16(0, self.options.available_current_milliamps)],
                            )?,
                            false,
                            false,
                        )?;
                    }
                    let subscriptions = if self.options.subscribe_to_updates {
                        tlv::subscriptions()?
                            .into_iter()
                            .filter(|s| self.identification.sent_messages.contains(&s.message_id))
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    let count = subscriptions.len();
                    for (i, message) in subscriptions.into_iter().enumerate() {
                        self.enqueue(message, false, i + 1 == count)?;
                    }
                    if count == 0 {
                        self.progress.subscribed = true;
                        self.emit(Event::Subscribed)?;
                    }
                }
                tlv::AUTHENTICATION_FAILED => return Err(Error::AuthenticationRejected),
                _ => return Err(self.unexpected(message.message_id)),
            },
            State::Running => match message.message_id {
                0x5702 => self.send_wifi(self.progress.transport_notification_seen)?,
                0x4300 => self.enqueue(self.endpoint.start_session()?, false, false)?,
                0x4e0d => {
                    let params = message.parameters()?;
                    let available = params
                        .iter()
                        .find(|p| p.id == 0)
                        .ok_or(carplay_protocol::Error::Invalid(
                            "missing wireless availability",
                        ))?
                        .as_u8()?;
                    if available > 1 {
                        return Err(carplay_protocol::Error::Invalid(
                            "wireless availability boolean",
                        )
                        .into());
                    }
                    if available == 1 {
                        self.progress.wireless_available_seen = true;
                    }
                    self.emit(Event::WirelessAvailability(available == 1))?;
                }
                0x4e0e => {
                    // Validate TLV framing/optional strings, but do not log phone identifiers.
                    for parameter in message.parameters()? {
                        if parameter.id <= 1 {
                            parameter.as_str()?;
                        }
                    }
                    self.progress.transport_notification_seen = true;
                    self.emit(Event::TransportNotified)?;
                    self.send_wifi(true)?;
                }
                tlv::AUTHENTICATION_FAILED => return Err(Error::AuthenticationRejected),
                _ => {
                    self.progress.forwarded_messages += 1;
                    self.emit(Event::Incoming(message))?;
                }
            },
            _ => return Err(self.unexpected(message.message_id)),
        }
        Ok(())
    }
    fn unexpected(&self, message_id: u16) -> Error {
        Error::UnexpectedMessage {
            message_id,
            state: self.state,
        }
    }
    fn send_wifi(&mut self, post: bool) -> Result<()> {
        if self.endpoint.transport == EndpointTransport::Wired {
            return Ok(());
        }
        let count = if post {
            self.post_wifi_queued
        } else {
            self.pre_wifi_queued
        };
        let maximum = if post { 2 } else { 5 };
        if count >= maximum {
            return Ok(());
        }
        self.enqueue(self.endpoint.wifi()?, post, false)?;
        if post {
            self.post_wifi_queued += 1;
        } else {
            self.pre_wifi_queued += 1;
        }
        Ok(())
    }
}

/// An owned stream must support bounded reads/writes and explicit shutdown.
/// Custom implementations must honor the requested timeout; AuthProvider calls and
/// application callbacks must also be bounded by their implementations.
pub trait Transport: ReadWrite {
    fn set_io_timeout(&self, timeout: Duration) -> io::Result<()>;
    fn close(&self) -> io::Result<()>;
}
impl Transport for RfcommStream {
    fn set_io_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))?;
        self.set_write_timeout(Some(timeout))
    }
    fn close(&self) -> io::Result<()> {
        self.shutdown()
    }
}
impl Transport for std::net::TcpStream {
    fn set_io_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))?;
        self.set_write_timeout(Some(timeout))
    }
    fn close(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Both)
    }
}
#[cfg(any(target_os = "linux", target_os = "windows"))]
impl Transport for carplay_platform::usb::system::CarKitStream {
    fn set_io_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.set_io_timeout(timeout)
    }
    fn close(&self) -> io::Result<()> {
        self.close()
    }
}
struct OwnedStream<S: Transport>(S);
impl<S: Transport> Drop for OwnedStream<S> {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Continue,
    Stop,
}
#[derive(Debug, Clone)]
pub struct Outcome {
    pub terminal: Terminal,
    pub progress: Progress,
}

/// Owns and closes `stream` on success, cancellation, authentication failure or I/O failure.
/// Never runs on a UI thread. Callbacks receive only redacted Debug events.
pub fn run<S: Transport>(
    stream: S,
    coordinator: Coordinator,
    cancelled: &AtomicBool,
    live_session: impl FnMut() -> bool,
    on_event: impl FnMut(Event) -> Action,
) -> Result<Outcome> {
    run_with_cancel(
        stream,
        coordinator,
        || cancelled.load(Ordering::Acquire),
        live_session,
        on_event,
    )
}

/// Like `run`, with independently owned cancellation signals (for example an
/// application stop and a verified handoff to the current AirPlay iAP tunnel).
/// The predicate is checked on every bounded I/O iteration, even when idle.
pub fn run_with_cancel<S: Transport>(
    stream: S,
    mut coordinator: Coordinator,
    mut cancelled: impl FnMut() -> bool,
    mut live_session: impl FnMut() -> bool,
    mut on_event: impl FnMut(Event) -> Action,
) -> Result<Outcome> {
    let mut stream = OwnedStream(stream);
    let started = Instant::now();
    let now = || started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    stream.0.set_io_timeout(MAX_POLL)?;
    coordinator.start(0)?;
    let mut buffer = [0u8; 8192];
    loop {
        if cancelled() {
            coordinator.stop();
        }
        coordinator.set_live_session(live_session());
        coordinator.advance_time(now())?;
        if coordinator.terminal().is_none() {
            let output = coordinator.take_output()?;
            if !output.is_empty() {
                stream.0.write_all(&output)?;
                stream.0.flush()?;
                coordinator.confirm_output_written(now())?;
            }
        }
        while let Some(event) = coordinator.poll_event() {
            if on_event(event) == Action::Stop {
                coordinator.stop();
                break;
            }
        }
        if let Some(terminal) = coordinator.terminal() {
            return Ok(Outcome {
                terminal,
                progress: coordinator.progress().clone(),
            });
        }
        let remaining = coordinator
            .next_deadline_ms()
            .map(|d| d.saturating_sub(now()).clamp(1, 100))
            .unwrap_or(100);
        stream.0.set_io_timeout(Duration::from_millis(remaining))?;
        match stream.0.read(&mut buffer) {
            Ok(0) => coordinator.channel_closed(),
            Ok(length) => coordinator.feed(&buffer[..length], now())?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
}
