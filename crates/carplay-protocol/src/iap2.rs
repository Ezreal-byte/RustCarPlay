// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9:
// shared/src/main/java/com/shilapi/xcertplay/transport/Iap2LinkEngine.kt
//! iAP2 link framing, bounded incremental parsing and reliable session transport.
//! Timers use caller-supplied monotonic milliseconds; this module never blocks.
use crate::{Error, Result};
use std::collections::VecDeque;

pub const MARKER: [u8; 6] = [0xff, 0x55, 0x02, 0x00, 0xee, 0x10];
pub const SYN: u8 = 0x80;
pub const ACK: u8 = 0x40;
pub const EAK: u8 = 0x20;
pub const RESET: u8 = 0x10;
pub const CONTROL_SESSION: u8 = 10;
pub const EA_SESSION: u8 = 11;
pub const FILE_TRANSFER_SESSION: u8 = 12;
pub const MAX_FRAME: usize = 65_535;
pub const MAX_PAYLOAD: usize = MAX_FRAME - 10;
const HEADER: usize = 9;

fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)))
}
fn u16be(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPacket {
    pub control: u8,
    pub sequence: u8,
    pub acknowledgement: u8,
    pub session_id: u8,
    /// None is a header-only acknowledgement; Some(empty) still has a payload checksum.
    pub payload: Option<Vec<u8>>,
}
impl LinkPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let length = HEADER + self.payload.as_ref().map_or(0, |p| p.len() + 1);
        if length > MAX_FRAME {
            return Err(Error::Limit);
        }
        let mut out = vec![
            0xff,
            0x5a,
            (length >> 8) as u8,
            length as u8,
            self.control,
            self.sequence,
            self.acknowledgement,
            self.session_id,
        ];
        out.push(checksum(&out));
        if let Some(payload) = &self.payload {
            out.extend(payload);
            out.push(checksum(payload));
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER {
            return Err(Error::Incomplete);
        }
        if bytes[..2] != [0xff, 0x5a] {
            return Err(Error::Invalid("link magic"));
        }
        let length = u16be(bytes, 2) as usize;
        if length < HEADER || length != bytes.len() {
            return Err(Error::Invalid("link length"));
        }
        if checksum(&bytes[..HEADER]) != 0 || (length > HEADER && checksum(&bytes[HEADER..]) != 0) {
            return Err(Error::Checksum);
        }
        Ok(Self {
            control: bytes[4],
            sequence: bytes[5],
            acknowledgement: bytes[6],
            session_id: bytes[7],
            payload: (length > HEADER).then(|| bytes[HEADER..length - 1].to_vec()),
        })
    }
}

/// Retains at most one maximum-size wire frame. Drain before pushing beyond available capacity.
#[derive(Debug, Default)]
pub struct PacketDecoder {
    bytes: VecDeque<u8>,
}
impl PacketDecoder {
    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len()
    }
    pub fn remaining_capacity(&self) -> usize {
        MAX_FRAME - self.bytes.len()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.remaining_capacity() {
            return Err(Error::Limit);
        }
        self.bytes.extend(bytes);
        Ok(())
    }
    /// Discards garbage and malformed frames, reporting one malformed frame per call.
    pub fn next_packet(&mut self) -> Result<Option<LinkPacket>> {
        while self.bytes.len() >= 2 && (self.bytes[0] != 0xff || self.bytes[1] != 0x5a) {
            self.bytes.pop_front();
        }
        if self.bytes.len() < HEADER {
            return Ok(None);
        }
        let raw = self.bytes.make_contiguous();
        let length = u16be(raw, 2) as usize;
        if checksum(&raw[..HEADER]) != 0 || length < HEADER {
            self.bytes.drain(..HEADER);
            return Err(Error::Invalid("link header"));
        }
        if self.bytes.len() < length {
            return Ok(None);
        }
        let raw: Vec<_> = self.bytes.drain(..length).collect();
        LinkPacket::decode(&raw).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub id: u8,
    pub kind: u8,
    pub version: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synchronization {
    pub max_outgoing: u8,
    pub max_length: u16,
    pub retransmission_timeout_ms: u16,
    pub acknowledgement_timeout_ms: u16,
    pub max_retransmissions: u8,
    pub max_acknowledgements: u8,
    pub sessions: Vec<SessionDescriptor>,
}
impl Synchronization {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.sessions.len() > (MAX_PAYLOAD - 10) / 3 {
            return Err(Error::Limit);
        }
        let mut out = vec![1, self.max_outgoing];
        out.extend(self.max_length.to_be_bytes());
        out.extend(self.retransmission_timeout_ms.to_be_bytes());
        out.extend(self.acknowledgement_timeout_ms.to_be_bytes());
        out.extend([self.max_retransmissions, self.max_acknowledgements]);
        for s in &self.sessions {
            out.extend([s.id, s.kind, s.version]);
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 10 || bytes[0] != 1 || !(bytes.len() - 10).is_multiple_of(3) {
            return Err(Error::Invalid("synchronization payload"));
        }
        Ok(Self {
            max_outgoing: bytes[1],
            max_length: u16be(bytes, 2),
            retransmission_timeout_ms: u16be(bytes, 4),
            acknowledgement_timeout_ms: u16be(bytes, 6),
            max_retransmissions: bytes[8],
            max_acknowledgements: bytes[9],
            sessions: bytes[10..]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|b| SessionDescriptor {
                    id: b[0],
                    kind: b[1],
                    version: b[2],
                })
                .collect(),
        })
    }
    fn usable(&self) -> bool {
        self.max_length > 10
            && self.max_outgoing > 0
            && (self.max_retransmissions == 0 || self.retransmission_timeout_ms > 0)
            && self
                .sessions
                .iter()
                .any(|s| s.id == CONTROL_SESSION && s.kind == 0)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub max_outgoing: u8,
    pub max_outgoing_delta: u8,
    pub acknowledgement_timeout_ms: u16,
    pub zero_acknowledgements: bool,
    pub control_session_version: u8,
    pub maximum_queued_packets: usize,
    pub maximum_out_of_order_packets: usize,
    pub maximum_pending_output_bytes: usize,
    pub maximum_pending_events: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_outgoing: 30,
            max_outgoing_delta: 0,
            acknowledgement_timeout_ms: 500,
            zero_acknowledgements: false,
            control_session_version: 1,
            maximum_queued_packets: 64,
            maximum_out_of_order_packets: 64,
            maximum_pending_output_bytes: 1_048_576,
            maximum_pending_events: 256,
        }
    }
}
impl Config {
    pub fn synchronization(&self) -> Synchronization {
        Synchronization {
            max_outgoing: self.max_outgoing,
            max_length: MAX_FRAME as u16,
            retransmission_timeout_ms: if self.zero_acknowledgements { 0 } else { 4000 },
            acknowledgement_timeout_ms: if self.zero_acknowledgements {
                0
            } else {
                self.acknowledgement_timeout_ms
            },
            max_retransmissions: if self.zero_acknowledgements { 0 } else { 4 },
            max_acknowledgements: if self.zero_acknowledgements { 0 } else { 3 },
            sessions: vec![
                SessionDescriptor {
                    id: CONTROL_SESSION,
                    kind: 0,
                    version: self.control_session_version,
                },
                SessionDescriptor {
                    id: EA_SESSION,
                    kind: 2,
                    version: 1,
                },
                SessionDescriptor {
                    id: FILE_TRANSFER_SESSION,
                    kind: 1,
                    version: 2,
                },
            ],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Detecting,
    Negotiating,
    Normal,
    Dead,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Control(Vec<u8>),
    Session { session_id: u8, bytes: Vec<u8> },
    Writable(bool),
    Dead(Option<String>),
}
#[derive(Debug, Clone)]
struct Pending {
    sequence: u8,
    session: u8,
    payload: Vec<u8>,
    retries: u8,
    deadline: u64,
}

pub struct LinkEngine {
    config: Config,
    state: State,
    peer: Synchronization,
    peer_received: bool,
    sent: u8,
    received: u8,
    last_acked_received: u8,
    writable: bool,
    accumulated_acks: u16,
    queued: VecDeque<Pending>,
    unacked: VecDeque<Pending>,
    out_of_order: Vec<Pending>,
    output: Vec<u8>,
    events: VecDeque<Event>,
    decoder: PacketDecoder,
    marker_received: Vec<u8>,
    marker_deadline: Option<u64>,
    syn_deadline: Option<u64>,
    ack_deadline: Option<u64>,
}
impl Default for LinkEngine {
    fn default() -> Self {
        Self::new(Config::default()).expect("valid default")
    }
}
impl LinkEngine {
    pub fn new(config: Config) -> Result<Self> {
        if config.max_outgoing == 0
            || !(1..=256).contains(&config.maximum_queued_packets)
            || !(1..=256).contains(&config.maximum_out_of_order_packets)
            || config.maximum_pending_events == 0
            || config.maximum_pending_output_bytes == 0
        {
            return Err(Error::Invalid("link configuration"));
        }
        Ok(Self {
            peer: config.synchronization(),
            config,
            state: State::Idle,
            peer_received: false,
            sent: 99,
            received: 0,
            last_acked_received: 0,
            writable: false,
            accumulated_acks: 0,
            queued: VecDeque::new(),
            unacked: VecDeque::new(),
            out_of_order: Vec::new(),
            output: Vec::new(),
            events: VecDeque::new(),
            decoder: PacketDecoder::default(),
            marker_received: Vec::new(),
            marker_deadline: None,
            syn_deadline: None,
            ack_deadline: None,
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn writable(&self) -> bool {
        self.writable
    }
    pub fn peer_synchronization(&self) -> Option<&Synchronization> {
        self.peer_received.then_some(&self.peer)
    }
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
    pub fn next_deadline_ms(&self) -> Option<u64> {
        [
            self.marker_deadline,
            self.syn_deadline,
            self.ack_deadline,
            self.unacked.iter().map(|p| p.deadline).min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    pub fn start(&mut self, wired_initiator: bool, now_ms: u64) {
        if self.state != State::Idle {
            return;
        }
        self.state = State::Detecting;
        self.append_output(&MARKER);
        if self.state == State::Dead {
            return;
        }
        self.marker_deadline = Some(now_ms.saturating_add(1000));
        if wired_initiator {
            self.negotiate(now_ms);
        }
    }
    fn negotiate(&mut self, now_ms: u64) {
        self.state = State::Negotiating;
        self.marker_deadline = None;
        self.send_syn();
        if self.state != State::Dead {
            self.syn_deadline = Some(now_ms.saturating_add(500));
        }
    }
    pub fn feed(&mut self, mut bytes: &[u8], now_ms: u64) {
        if matches!(self.state, State::Dead | State::Idle) {
            return;
        }
        if self.state == State::Detecting {
            let count = (MARKER.len() - self.marker_received.len()).min(bytes.len());
            self.marker_received.extend(&bytes[..count]);
            bytes = &bytes[count..];
            if self.marker_received != MARKER[..self.marker_received.len()] {
                self.die(Some("iAP2 marker was not received"));
                return;
            }
            if self.marker_received.len() < MARKER.len() {
                return;
            }
            self.negotiate(now_ms);
        }
        while !bytes.is_empty() && self.state != State::Dead {
            let count = self.decoder.remaining_capacity().min(bytes.len());
            if count == 0 {
                self.die(Some("inbound buffering limit"));
                return;
            }
            if self.decoder.push(&bytes[..count]).is_err() {
                self.die(Some("inbound buffering limit"));
                return;
            }
            bytes = &bytes[count..];
            loop {
                match self.decoder.next_packet() {
                    Ok(Some(packet)) => self.process(packet, now_ms),
                    Err(_) => continue, // corrupt frames are discarded; reliable transport retries them
                    Ok(None) => break,
                }
                if self.state == State::Dead {
                    break;
                }
            }
        }
    }
    pub fn feed_eof(&mut self) {
        self.die(None);
    }
    pub fn send_control(&mut self, bytes: &[u8], now_ms: u64) -> Result<()> {
        self.send_session(CONTROL_SESSION, bytes, now_ms)
    }
    pub fn send_session(&mut self, session: u8, bytes: &[u8], now_ms: u64) -> Result<()> {
        if self.state == State::Dead {
            return Err(Error::Closed);
        }
        if bytes.len() > MAX_PAYLOAD
            || (self.peer_received && bytes.len() + 10 > self.peer.max_length as usize)
        {
            return Err(Error::Limit);
        }
        if self.peer_received && !self.peer.sessions.iter().any(|s| s.id == session) {
            return Err(Error::Invalid("session not negotiated"));
        }
        let packet = Pending {
            sequence: 0,
            session,
            payload: bytes.to_vec(),
            retries: 0,
            deadline: 0,
        };
        if self.state == State::Normal && self.unacked.len() < (self.peer.max_outgoing as usize) {
            self.transmit_new(packet, now_ms);
        } else if self.queued.len() < self.config.maximum_queued_packets {
            self.queued.push_back(packet);
            self.set_writable(false);
        } else {
            self.die(Some("outbound queue limit"));
            return Err(Error::Limit);
        }
        if self.state == State::Dead {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }
    fn process(&mut self, packet: LinkPacket, now_ms: u64) {
        if packet.control & RESET != 0 {
            self.die(Some("peer reset"));
            return;
        }
        if packet.control & SYN != 0
            && self.state == State::Negotiating
            && let Some(payload) = &packet.payload
        {
            match Synchronization::decode(payload) {
                Ok(sync) if sync.usable() => {
                    self.peer = sync;
                    self.peer_received = true;
                    self.received = packet.sequence;
                    self.last_acked_received = self.received;
                    self.send_ack();
                }
                _ => {
                    self.die(Some("invalid peer synchronization limits"));
                    return;
                }
            }
        }
        if self.state == State::Dead {
            return;
        }
        if packet.control & ACK != 0 {
            self.accumulated_acks += 1;
            if self.state == State::Negotiating
                && self.peer_received
                && packet.acknowledgement == self.sent
            {
                if self.queued.iter().any(|p| {
                    p.payload.len() + 10 > self.peer.max_length as usize
                        || !self.peer.sessions.iter().any(|s| s.id == p.session)
                }) {
                    self.die(Some("peer cannot carry queued payload"));
                    return;
                }
                self.state = State::Normal;
                self.syn_deadline = None;
                self.set_writable(true);
            }
            // Only an ACK for an actually outstanding sequence may advance the send window.
            if let Some(index) = self
                .unacked
                .iter()
                .position(|p| p.sequence == packet.acknowledgement)
            {
                self.unacked.drain(..=index);
            }
            self.flush_queued(now_ms);
        }
        if self.state != State::Normal {
            return;
        }
        if packet.control & EAK != 0
            && let Some(missing) = &packet.payload
        {
            let indices: Vec<_> = self
                .unacked
                .iter()
                .enumerate()
                .filter_map(|(i, p)| missing.contains(&p.sequence).then_some(i))
                .collect();
            for index in indices {
                self.retransmit(index, now_ms);
                if self.state == State::Dead {
                    return;
                }
            }
        }
        if packet.control & !ACK == 0
            && let Some(payload) = packet.payload
        {
            self.receive_data(
                Pending {
                    sequence: packet.sequence,
                    session: packet.session_id,
                    payload,
                    retries: 0,
                    deadline: 0,
                },
                now_ms,
            );
        }
        if self.peer.max_acknowledgements > 0
            && self.accumulated_acks >= self.peer.max_acknowledgements as u16
        {
            self.send_ack();
        }
    }
    fn receive_data(&mut self, packet: Pending, now_ms: u64) {
        let distance = packet.sequence.wrapping_sub(self.received) as u16;
        if distance == 0 || distance > self.peer.max_outgoing as u16 + 10 {
            self.send_ack();
            return;
        }
        if self
            .out_of_order
            .iter()
            .any(|p| p.sequence == packet.sequence)
        {
            return;
        }
        if self.out_of_order.len() >= self.config.maximum_out_of_order_packets {
            self.die(Some("out-of-order limit"));
            return;
        }
        self.out_of_order.push(packet);
        if distance > 1 {
            if distance >= self.peer.max_outgoing as u16 {
                let missing = (1..distance)
                    .map(|i| self.received.wrapping_add(i as u8))
                    .collect();
                self.ack_deadline = None;
                self.write_packet(EAK, self.sent, 0, Some(missing));
            }
            return;
        }
        while let Some(index) = self
            .out_of_order
            .iter()
            .position(|p| p.sequence == self.received.wrapping_add(1))
        {
            let packet = self.out_of_order.remove(index);
            self.received = packet.sequence;
            if packet.session == CONTROL_SESSION {
                self.enqueue(Event::Control(packet.payload));
            } else {
                self.enqueue(Event::Session {
                    session_id: packet.session,
                    bytes: packet.payload,
                });
            }
            if self.state == State::Dead {
                return;
            }
        }
        if self.peer.max_acknowledgements == 0 {
            return;
        }
        let window = self
            .peer
            .max_outgoing
            .saturating_sub(self.config.max_outgoing_delta)
            .max(1);
        if self.received.wrapping_sub(self.last_acked_received) >= window {
            self.send_ack();
        } else {
            self.ack_deadline =
                Some(now_ms.saturating_add(self.peer.acknowledgement_timeout_ms as u64));
        }
    }
    fn transmit_new(&mut self, mut packet: Pending, now_ms: u64) {
        self.sent = self.sent.wrapping_add(1);
        packet.sequence = self.sent;
        packet.deadline = now_ms.saturating_add(self.peer.retransmission_timeout_ms as u64);
        self.ack_deadline = None;
        self.write_packet(
            ACK,
            packet.sequence,
            packet.session,
            Some(packet.payload.clone()),
        );
        self.last_acked_received = self.received;
        if self.state != State::Dead && self.peer.max_retransmissions > 0 {
            self.unacked.push_back(packet);
        }
        self.set_writable(self.unacked.len() < self.peer.max_outgoing as usize);
    }
    fn flush_queued(&mut self, now_ms: u64) {
        while self.state == State::Normal && self.unacked.len() < self.peer.max_outgoing as usize {
            let Some(packet) = self.queued.pop_front() else {
                break;
            };
            self.transmit_new(packet, now_ms);
        }
        if self.state == State::Normal {
            self.set_writable(self.unacked.len() < self.peer.max_outgoing as usize);
        }
    }
    fn retransmit(&mut self, index: usize, now_ms: u64) {
        let packet = &mut self.unacked[index];
        packet.retries = packet.retries.saturating_add(1);
        if packet.retries >= self.peer.max_retransmissions {
            self.die(Some("packet not acknowledged"));
            return;
        }
        packet.deadline = now_ms.saturating_add(self.peer.retransmission_timeout_ms as u64);
        let p = packet.clone();
        self.write_packet(ACK, p.sequence, p.session, Some(p.payload));
    }
    pub fn advance_time(&mut self, now_ms: u64) {
        if self.state == State::Dead {
            return;
        }
        if self.marker_deadline.is_some_and(|d| d <= now_ms) {
            self.marker_deadline = None;
            if self.state == State::Detecting {
                self.append_output(&MARKER);
                if self.state != State::Dead {
                    self.marker_deadline = Some(now_ms.saturating_add(1000));
                }
            }
        }
        if self.syn_deadline.is_some_and(|d| d <= now_ms) {
            self.syn_deadline = None;
            if self.state == State::Negotiating {
                self.send_syn();
                if self.state != State::Dead {
                    self.syn_deadline = Some(now_ms.saturating_add(500));
                }
            }
        }
        if self.ack_deadline.is_some_and(|d| d <= now_ms) {
            self.ack_deadline = None;
            if self.state == State::Normal {
                self.send_ack();
            }
        }
        let due: Vec<_> = self
            .unacked
            .iter()
            .enumerate()
            .filter_map(|(i, p)| (p.deadline <= now_ms).then_some(i))
            .collect();
        for index in due {
            if self.state != State::Normal {
                break;
            }
            self.retransmit(index, now_ms);
        }
    }
    fn send_syn(&mut self) {
        self.write_packet(
            SYN,
            self.sent,
            0,
            Some(
                self.config
                    .synchronization()
                    .encode()
                    .expect("fixed bounded SYN"),
            ),
        );
    }
    fn send_ack(&mut self) {
        self.ack_deadline = None;
        self.last_acked_received = self.received;
        self.write_packet(ACK, self.sent, 0, None);
    }
    fn write_packet(
        &mut self,
        control: u8,
        sequence: u8,
        session_id: u8,
        payload: Option<Vec<u8>>,
    ) {
        if self.state == State::Dead {
            return;
        }
        self.accumulated_acks = 0;
        let packet = LinkPacket {
            control,
            sequence,
            acknowledgement: self.received,
            session_id,
            payload,
        };
        match packet.encode() {
            Ok(bytes) => self.append_output(&bytes),
            Err(_) => self.die(Some("outbound packet too large")),
        }
    }
    fn append_output(&mut self, bytes: &[u8]) {
        if self.state == State::Dead {
            return;
        }
        if bytes.len()
            > self
                .config
                .maximum_pending_output_bytes
                .saturating_sub(self.output.len())
        {
            self.die(Some("pending output limit"));
        } else {
            self.output.extend(bytes);
        }
    }
    fn enqueue(&mut self, event: Event) {
        if self.state == State::Dead {
            return;
        }
        if self.events.len() >= self.config.maximum_pending_events {
            self.die(Some("pending event limit"));
        } else {
            self.events.push_back(event);
        }
    }
    fn set_writable(&mut self, value: bool) {
        if self.state == State::Dead || self.writable == value {
            return;
        }
        self.writable = value;
        self.enqueue(Event::Writable(value));
    }
    fn die(&mut self, reason: Option<&str>) {
        if self.state == State::Dead {
            return;
        }
        self.state = State::Dead;
        self.writable = false;
        self.marker_deadline = None;
        self.syn_deadline = None;
        self.ack_deadline = None;
        self.unacked.clear();
        self.queued.clear();
        self.out_of_order.clear();
        self.decoder = PacketDecoder::default();
        self.marker_received.clear();
        self.output.clear();
        self.events.clear();
        self.events
            .push_back(Event::Dead(reason.map(str::to_owned)));
    }
}
