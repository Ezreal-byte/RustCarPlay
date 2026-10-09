// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9 shared/src/main/java/com/shilapi/xcertplay/network/CarPlayBonjour.kt.
//! Browse the PHONE's _carplay-ctrl service and request connection to this receiver.
//! Selection must match the chosen phone's TXT Bluetooth/device ID or an explicitly selected IP.
//! TXT IDs are a discovery hint, not cryptographic authentication; AirPlay pairing still authenticates the phone.
use mdns_sd::{IfKind, ScopedIp, ServiceDaemon, ServiceEvent};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, SocketAddrV6, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const SERVICE_TYPE: &str = "_carplay-ctrl._tcp.local.";
const MAX_HEADER: usize = 16 * 1024;
const MAX_TRACKED_ENDPOINTS: usize = 64;
const POLL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub struct Options {
    pub receiver_address: SocketAddr,
    /// Must be the same device ID advertised by AirPlay and supplied in 0x4301.
    pub receiver_device_id: String,
    pub source_version: String,
    pub target_bluetooth: Option<[u8; 6]>,
    /// Explicit user selection takes priority over unavailable/private TXT identifiers.
    pub target_ip: Option<IpAddr>,
    /// Total time limit per attempt (connect, write, response headers).
    pub timeout: Duration,
    pub max_attempts: u8,
    pub retry_delay: Duration,
}
impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field(
                "target_bluetooth_selected",
                &self.target_bluetooth.is_some(),
            )
            .field("target_ip_selected", &self.target_ip.is_some())
            .field("timeout", &self.timeout)
            .field("max_attempts", &self.max_attempts)
            .finish_non_exhaustive()
    }
}
impl Options {
    pub fn new(
        receiver_address: SocketAddr,
        local_bluetooth: [u8; 6],
        source_version: impl Into<String>,
        target_bluetooth: [u8; 6],
    ) -> Self {
        Self {
            receiver_address,
            receiver_device_id: local_bluetooth.iter().map(|b| format!("{b:02X}")).collect(),
            source_version: source_version.into(),
            target_bluetooth: Some(target_bluetooth),
            target_ip: None,
            timeout: Duration::from_secs(3),
            max_attempts: 5,
            retry_delay: Duration::from_secs(1),
        }
    }
    fn validate(&self) -> Result<()> {
        if !valid_ip(self.receiver_address.ip())
            || self.receiver_address.ip().is_loopback()
            || self.receiver_address.port() == 0
        {
            return Err(Error::Configuration(
                "select a concrete receiver LAN address and bound port",
            ));
        }
        if self.target_ip.is_some_and(|ip| {
            !valid_ip(ip)
                || ip.is_loopback()
                || ip == self.receiver_address.ip()
                || ip.is_ipv4() != self.receiver_address.is_ipv4()
        }) {
            return Err(Error::Configuration(
                "selected phone IP must be distinct and reachable through the selected receiver address family",
            ));
        }
        if !(Duration::from_millis(100)..=Duration::from_secs(30)).contains(&self.timeout)
            || self.max_attempts == 0
            || self.max_attempts > 10
            || self.retry_delay > Duration::from_secs(60)
        {
            return Err(Error::Configuration(
                "invalid discovery timeout or retry limit",
            ));
        }
        if parse_mac(&self.receiver_device_id).is_none()
            || self.source_version.is_empty()
            || self.source_version.len() > 64
            || !self
                .source_version
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.')
        {
            return Err(Error::Configuration(
                "invalid receiver device ID or source version",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid discovery configuration: {0}")]
    Configuration(&'static str),
    #[error("mDNS operation failed")]
    Mdns,
    #[error("control probe I/O failed ({0:?})")]
    Io(io::ErrorKind),
    #[error("control probe timed out")]
    TimedOut,
    #[error("control probe cancelled")]
    Cancelled,
    #[error("invalid or oversized HTTP response headers")]
    Response,
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value.kind())
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchBy {
    BluetoothId,
    SelectedIp,
}
pub enum Event {
    Browsing,
    /// No trusted selection can be made from the service TXT. Show an explicit phone-IP selector.
    PhoneSelectionRequired {
        candidate_addresses: Vec<IpAddr>,
    },
    Matched {
        by: MatchBy,
        ipv6: bool,
    },
    ProbeStarted {
        attempt: u8,
    },
    ProbeResponse {
        attempt: u8,
        status: u16,
    },
    ProbeFailed {
        attempt: u8,
        reason: Failure,
    },
    RetryLimitReached,
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Io(io::ErrorKind),
    TimedOut,
    MalformedResponse,
    MdnsStopped,
}
impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Browsing => f.write_str("Browsing"),
            Self::Stopped => f.write_str("Stopped"),
            Self::RetryLimitReached => f.write_str("RetryLimitReached"),
            Self::PhoneSelectionRequired {
                candidate_addresses,
            } => f
                .debug_struct("PhoneSelectionRequired")
                .field("candidate_count", &candidate_addresses.len())
                .finish(),
            Self::Matched { by, ipv6 } => f
                .debug_struct("Matched")
                .field("by", by)
                .field("ipv6", ipv6)
                .finish(),
            Self::ProbeStarted { attempt } => f
                .debug_struct("ProbeStarted")
                .field("attempt", attempt)
                .finish(),
            Self::ProbeResponse { attempt, status } => f
                .debug_struct("ProbeResponse")
                .field("attempt", attempt)
                .field("status", status)
                .finish(),
            Self::ProbeFailed { attempt, reason } => f
                .debug_struct("ProbeFailed")
                .field("attempt", attempt)
                .field("reason", reason)
                .finish(),
        }
    }
}

pub struct Handle {
    pub events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    retry: Arc<AtomicBool>,
    active: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
    daemon: ServiceDaemon,
}
impl Handle {
    /// Restart bounded probes for already matched endpoints after a new bootstrap attempt.
    /// Do not call while the current AirPlay session is active.
    pub fn retry(&self) {
        self.retry.store(true, Ordering::Release);
    }
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Ok(active) = self.active.lock()
            && let Some(stream) = active.as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Ok(receiver) = self.daemon.shutdown() {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn start(options: Options) -> Result<Handle> {
    options.validate()?;
    let daemon = ServiceDaemon::new().map_err(|_| Error::Mdns)?;
    if daemon
        .disable_interface(IfKind::All)
        .and_then(|_| daemon.enable_interface(IfKind::Addr(options.receiver_address.ip())))
        .is_err()
    {
        let _ = daemon.shutdown();
        return Err(Error::Mdns);
    }
    let browser = match daemon.browse(SERVICE_TYPE) {
        Ok(browser) => browser,
        Err(_) => {
            let _ = daemon.shutdown();
            return Err(Error::Mdns);
        }
    };
    let (tx, events) = mpsc::sync_channel(64);
    let cancel = Arc::new(AtomicBool::new(false));
    let retry = Arc::new(AtomicBool::new(false));
    let active = Arc::new(Mutex::new(None));
    let stopped = cancel.clone();
    let socket = active.clone();
    let retries = retry.clone();
    let thread = match thread::Builder::new()
        .name("carplay-phone-discovery".into())
        .spawn(move || worker(options, browser, stopped, retries, socket, tx))
    {
        Ok(thread) => thread,
        Err(error) => {
            let _ = daemon.stop_browse(SERVICE_TYPE);
            let _ = daemon.shutdown();
            return Err(error.into());
        }
    };
    Ok(Handle {
        events,
        cancel,
        retry,
        active,
        thread: Some(thread),
        daemon,
    })
}
fn emit(tx: &SyncSender<Event>, event: Event) {
    let _ = tx.try_send(event);
}
struct Job {
    attempts: u8,
    next: Option<Instant>,
}
fn worker(
    options: Options,
    browser: mdns_sd::Receiver<ServiceEvent>,
    cancel: Arc<AtomicBool>,
    retry: Arc<AtomicBool>,
    active: Arc<Mutex<Option<TcpStream>>>,
    tx: SyncSender<Event>,
) {
    emit(&tx, Event::Browsing);
    let mut jobs: BTreeMap<SocketAddr, Job> = BTreeMap::new();
    let mut selection_notified = false;
    let mut connected = false;
    let started = Instant::now();
    while !cancel.load(Ordering::Acquire) {
        if retry.swap(false, Ordering::AcqRel) {
            connected = false;
            for job in jobs.values_mut() {
                job.attempts = 0;
                job.next = Some(Instant::now());
            }
        }
        if let Ok(event) = browser.recv_timeout(POLL) {
            match event {
                ServiceEvent::ServiceResolved(service) => {
                    let addresses: Vec<_> = service
                        .get_addresses()
                        .iter()
                        .filter_map(|ip| socket_address(ip, service.get_port()))
                        .take(32)
                        .collect();
                    let selected = select(
                        &options,
                        &addresses,
                        service.get_property_val_str("id"),
                        service.get_property_val_str("deviceid"),
                    );
                    match selected {
                        Selection::Match(addresses, by) => {
                            for address in addresses {
                                if jobs.len() < MAX_TRACKED_ENDPOINTS
                                    && !jobs.contains_key(&address)
                                {
                                    jobs.insert(
                                        address,
                                        Job {
                                            attempts: 0,
                                            next: (!connected).then(Instant::now),
                                        },
                                    );
                                    emit(
                                        &tx,
                                        Event::Matched {
                                            by,
                                            ipv6: address.is_ipv6(),
                                        },
                                    );
                                }
                            }
                        }
                        Selection::NeedsIp => {
                            if !selection_notified {
                                selection_notified = true;
                                emit(
                                    &tx,
                                    Event::PhoneSelectionRequired {
                                        candidate_addresses: addresses
                                            .iter()
                                            .filter(|a| {
                                                valid_ip(a.ip())
                                                    && !a.ip().is_loopback()
                                                    && a.is_ipv4()
                                                        == options.receiver_address.is_ipv4()
                                            })
                                            .map(SocketAddr::ip)
                                            .collect(),
                                    },
                                );
                            }
                        }
                        Selection::Ignore => {}
                    }
                }
                ServiceEvent::SearchStopped(_) => {
                    if !cancel.load(Ordering::Acquire) {
                        emit(
                            &tx,
                            Event::ProbeFailed {
                                attempt: 0,
                                reason: Failure::MdnsStopped,
                            },
                        );
                    }
                    break;
                }
                _ => {}
            }
        } else if browser.is_disconnected() {
            break;
        }
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if jobs.is_empty() && !selection_notified && started.elapsed() >= Duration::from_secs(10) {
            // Some iPhones omit or privatize the TXT identifier. Never guess
            // which mismatching service belongs to the selected Bluetooth peer.
            selection_notified = true;
            emit(
                &tx,
                Event::PhoneSelectionRequired {
                    candidate_addresses: Vec::new(),
                },
            );
        }
        let due = jobs.iter().find_map(|(address, job)| {
            job.next
                .filter(|time| *time <= Instant::now())
                .map(|_| *address)
        });
        if let Some(address) = due {
            let job = jobs.get_mut(&address).expect("selected job exists");
            job.attempts += 1;
            job.next = None;
            emit(
                &tx,
                Event::ProbeStarted {
                    attempt: job.attempts,
                },
            );
            let succeeded = match probe(&options, address, &cancel, &active) {
                Ok(status) => {
                    emit(
                        &tx,
                        Event::ProbeResponse {
                            attempt: job.attempts,
                            status,
                        },
                    );
                    (200..300).contains(&status)
                }
                Err(Error::Cancelled) => break,
                Err(error) => {
                    let reason = match error {
                        Error::Io(kind) => Failure::Io(kind),
                        Error::TimedOut => Failure::TimedOut,
                        _ => Failure::MalformedResponse,
                    };
                    emit(
                        &tx,
                        Event::ProbeFailed {
                            attempt: job.attempts,
                            reason,
                        },
                    );
                    false
                }
            };
            if !succeeded {
                if job.attempts < options.max_attempts {
                    job.next = Some(Instant::now() + options.retry_delay);
                } else {
                    emit(&tx, Event::RetryLimitReached);
                }
            } else {
                // Multiple addresses can name the same phone. Once it accepts,
                // never send another /connect until the application requests retry.
                connected = true;
                for job in jobs.values_mut() {
                    job.next = None;
                }
            }
        }
    }
    emit(&tx, Event::Stopped);
}
fn valid_ip(ip: IpAddr) -> bool {
    !ip.is_unspecified() && !ip.is_multicast() && !matches!(ip,IpAddr::V4(v4)if v4.is_broadcast())
}
fn parse_mac(value: &str) -> Option<[u8; 6]> {
    let compact =
        if value.len() == 17 && (value.as_bytes()[2] == b':' || value.as_bytes()[2] == b'-') {
            let separator = value.as_bytes()[2];
            if ![2, 5, 8, 11, 14]
                .iter()
                .all(|&i| value.as_bytes()[i] == separator)
            {
                return None;
            }
            value
                .bytes()
                .filter(|b| *b != separator)
                .collect::<Vec<_>>()
        } else if value.len() == 12 {
            value.as_bytes().to_vec()
        } else {
            return None;
        };
    if compact.len() != 12 || !compact.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let hex = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b.to_ascii_lowercase() - b'a' + 10
        }
    };
    Some(std::array::from_fn(|i| {
        hex(compact[2 * i]) * 16 + hex(compact[2 * i + 1])
    }))
}
enum Selection {
    Match(Vec<SocketAddr>, MatchBy),
    NeedsIp,
    Ignore,
}
fn select(
    options: &Options,
    addresses: &[SocketAddr],
    id: Option<&str>,
    device_id: Option<&str>,
) -> Selection {
    let addresses: Vec<_> = addresses
        .iter()
        .copied()
        .filter(|a| {
            a.port() != 0
                && valid_ip(a.ip())
                && !a.ip().is_loopback()
                && a.ip() != options.receiver_address.ip()
                && a.is_ipv4() == options.receiver_address.is_ipv4()
        })
        .collect();
    if let Some(target) = options.target_ip {
        let addresses: Vec<_> = addresses.into_iter().filter(|a| a.ip() == target).collect();
        return if addresses.is_empty() {
            Selection::Ignore
        } else {
            Selection::Match(addresses, MatchBy::SelectedIp)
        };
    }
    let Some(target) = options.target_bluetooth else {
        return Selection::NeedsIp;
    };
    let ids: Vec<_> = [id, device_id]
        .into_iter()
        .flatten()
        .filter_map(parse_mac)
        .collect();
    if ids.is_empty() {
        return Selection::NeedsIp;
    }
    if ids.contains(&target) && !addresses.is_empty() {
        Selection::Match(addresses, MatchBy::BluetoothId)
    } else {
        Selection::Ignore
    }
}
fn socket_address(address: &ScopedIp, port: u16) -> Option<SocketAddr> {
    match address {
        ScopedIp::V4(v4) => Some(SocketAddr::new(IpAddr::V4(*v4.addr()), port)),
        ScopedIp::V6(v6) => {
            let scope = v6.scope_id().index;
            if v6.addr().is_unicast_link_local() && scope == 0 {
                return None;
            }
            Some(SocketAddr::V6(SocketAddrV6::new(
                *v6.addr(),
                port,
                0,
                scope,
            )))
        }
        _ => None,
    }
}
/// Exact DiPlay connect request. Peer IP literals prevent hostname/header injection.
pub fn connect_request(
    peer: SocketAddr,
    source_version: &str,
    receiver_device_id: &str,
) -> Result<Vec<u8>> {
    let id =
        parse_mac(receiver_device_id).ok_or(Error::Configuration("invalid receiver device ID"))?;
    if peer.port() == 0
        || source_version.is_empty()
        || source_version.len() > 64
        || !source_version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(Error::Configuration(
            "invalid probe endpoint/source version",
        ));
    }
    let host = match peer.ip() {
        IpAddr::V4(ip) => format!("{ip}:{}", peer.port()),
        IpAddr::V6(ip) => format!("[{ip}]:{}", peer.port()),
    };
    let id: String = id.iter().map(|b| format!("{b:02X}")).collect();
    Ok(format!("GET /ctrl-int/1/connect HTTP/1.1\r\nHost: {host}\r\nUser-Agent: AirPlay/{source_version}\r\nAirPlay-Receiver-Device-ID: {id}\r\nConnection: close\r\n\r\n").into_bytes())
}
struct SocketGuard<'a> {
    stream: TcpStream,
    active: &'a Mutex<Option<TcpStream>>,
}
impl Drop for SocketGuard<'_> {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Ok(mut slot) = self.active.lock() {
            *slot = None;
        }
    }
}
fn probe(
    options: &Options,
    peer: SocketAddr,
    cancel: &AtomicBool,
    active: &Mutex<Option<TcpStream>>,
) -> Result<u16> {
    let deadline = Instant::now() + options.timeout;
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let socket = Socket::new(Domain::for_address(peer), Type::STREAM, Some(Protocol::TCP))?;
    let mut source = options.receiver_address;
    source.set_port(0);
    socket.bind(&source.into())?;
    socket.connect_timeout(&peer.into(), options.timeout.min(Duration::from_secs(1)))?;
    let stream: TcpStream = socket.into();
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(POLL))?;
    if let Ok(mut slot) = active.lock() {
        *slot = Some(stream.try_clone()?);
    }
    let mut owned = SocketGuard { stream, active };
    let request = connect_request(peer, &options.source_version, &options.receiver_device_id)?;
    let mut written = 0;
    while written < request.len() {
        check_deadline(cancel, deadline)?;
        match owned.stream.write(&request[written..]) {
            Ok(0) => return Err(Error::Io(io::ErrorKind::WriteZero)),
            Ok(n) => written += n,
            Err(e) if retryable(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    read_response(&mut owned.stream, cancel, deadline)
}
fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
fn check_deadline(cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(Error::TimedOut)
    } else {
        Ok(())
    }
}
fn read_response(reader: &mut impl Read, cancel: &AtomicBool, deadline: Instant) -> Result<u16> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        check_deadline(cancel, deadline)?;
        if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
            return response_status(&bytes[..end]);
        }
        if bytes.len() >= MAX_HEADER {
            return Err(Error::Response);
        }
        let limit = chunk.len().min(MAX_HEADER - bytes.len());
        match reader.read(&mut chunk[..limit]) {
            Ok(0) => return Err(Error::Response),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if retryable(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
}
fn response_status(bytes: &[u8]) -> Result<u16> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Response)?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().ok_or(Error::Response)?.splitn(3, ' ');
    if !matches!(first.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
        return Err(Error::Response);
    }
    let status = first.next().ok_or(Error::Response)?;
    if status.len() != 3 || !status.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Response);
    }
    if first
        .next()
        .is_none_or(|reason| reason.chars().any(char::is_control))
    {
        return Err(Error::Response);
    }
    let status: u16 = status.parse().map_err(|_| Error::Response)?;
    if !(100..600).contains(&status) {
        return Err(Error::Response);
    }
    for line in lines {
        let (key, value) = line.split_once(':').ok_or(Error::Response)?;
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.chars().any(|c| c.is_control() && c != '\t')
        {
            return Err(Error::Response);
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options::new(
            "192.168.1.2:7000".parse().unwrap(),
            [2, 0, 0, 0, 0, 1],
            "950.7.1",
            [2, 0, 0, 0, 0, 2],
        )
    }
    #[test]
    fn selected_phone_matches_txt_only_and_missing_identity_requires_explicit_ip() {
        let addresses = [
            "192.168.1.3:1234".parse().unwrap(),
            "192.168.1.4:1234".parse().unwrap(),
        ];
        let mut options = options();
        assert!(matches!(
            select(&options, &addresses, Some("02:00:00:00:00:02"), None),
            Selection::Match(_, MatchBy::BluetoothId)
        ));
        assert!(matches!(
            select(&options, &addresses, None, Some("020000000002")),
            Selection::Match(_, MatchBy::BluetoothId)
        ));
        assert!(matches!(
            select(&options, &addresses, Some("02:00:00:00:00:03"), None),
            Selection::Ignore
        ));
        assert!(matches!(
            select(&options, &addresses, None, None),
            Selection::NeedsIp
        ));
        options.target_ip = Some("192.168.1.4".parse().unwrap());
        let Selection::Match(found, MatchBy::SelectedIp) = select(&options, &addresses, None, None)
        else {
            panic!()
        };
        assert_eq!(found, vec![addresses[1]]);
        assert!(matches!(
            select(&options, &addresses[..1], None, None),
            Selection::Ignore
        ));
    }
    #[test]
    fn excludes_other_interfaces_self_multicast_and_loopback() {
        let addresses = [
            "192.168.1.2:1234",
            "127.0.0.1:1234",
            "224.0.0.1:1234",
            "0.0.0.0:1234",
            "[fe80::1%4]:1234",
        ]
        .map(|a| a.parse().unwrap());
        assert!(matches!(
            select(&options(), &addresses, Some("020000000002"), None),
            Selection::Ignore
        ));
    }
    #[test]
    fn source_probe_request_matches_upstream_and_strips_ipv6_scope_from_host() {
        let value = connect_request(
            "[fe80::1%4]:1234".parse().unwrap(),
            "950.7.1",
            "02:00:00:00:00:01",
        )
        .unwrap();
        assert_eq!(value,b"GET /ctrl-int/1/connect HTTP/1.1\r\nHost: [fe80::1]:1234\r\nUser-Agent: AirPlay/950.7.1\r\nAirPlay-Receiver-Device-ID: 020000000001\r\nConnection: close\r\n\r\n");
        assert!(
            connect_request(
                "192.168.1.3:12".parse().unwrap(),
                "1\r\nX: y",
                "020000000001"
            )
            .is_err()
        );
        assert!(connect_request("192.168.1.3:12".parse().unwrap(), "1", "not-an-address").is_err());
    }
    #[test]
    fn response_headers_are_bounded_validated_and_cancellation_aware() {
        let parse = |wire: &[u8]| {
            read_response(
                &mut io::Cursor::new(wire),
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(1),
            )
        };
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap(),
            200
        );
        assert_eq!(parse(b"HTTP/1.1 403 Forbidden\r\n\r\n").unwrap(), 403);
        for value in [
            &b"HTTP/1.1 200 OK\r\n"[..],
            b"garbage\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nBadHeader\r\n\r\n",
            b"HTTP/1.1 999 impossible\r\n\r\n",
        ] {
            assert!(parse(value).is_err());
        }
        assert!(parse(&vec![b'a'; MAX_HEADER + 10]).is_err());
        assert!(matches!(
            read_response(
                &mut io::empty(),
                &AtomicBool::new(true),
                Instant::now() + POLL
            ),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            read_response(&mut io::empty(), &AtomicBool::new(false), Instant::now()),
            Err(Error::TimedOut)
        ));
    }
    #[test]
    fn real_loopback_probe_binds_source_writes_request_reads_reply_and_closes() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut socket, peer) = server.accept().unwrap();
            assert!(peer.ip().is_loopback());
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 256];
            while !request.windows(4).any(|b| b == b"\r\n\r\n") {
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
            }
            assert!(request.starts_with(b"GET /ctrl-int/1/connect HTTP/1.1\r\n"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            assert_eq!(socket.read(&mut buf).unwrap(), 0);
        });
        let mut options = options();
        options.receiver_address = "127.0.0.1:7000".parse().unwrap();
        let active = Mutex::new(None);
        assert_eq!(
            probe(&options, address, &AtomicBool::new(false), &active).unwrap(),
            200
        );
        assert!(active.lock().unwrap().is_none());
        worker.join().unwrap();
    }
    #[test]
    fn invalid_selection_is_rejected_before_network_access_and_debug_is_redacted() {
        let mut value = options();
        value.receiver_address = "0.0.0.0:0".parse().unwrap();
        assert!(start(value).is_err());
        assert!(!format!("{:?}", options()).contains("192.168"));
        assert!(
            !format!(
                "{:?}",
                Event::PhoneSelectionRequired {
                    candidate_addresses: vec!["192.168.1.9".parse().unwrap()]
                }
            )
            .contains("192.168")
        );
    }
    #[test]
    fn stalled_native_peer_times_out_or_cancels_and_is_closed() {
        for cancel_early in [false, true] {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = server.local_addr().unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            let stopped = cancel.clone();
            let worker = thread::spawn(move || {
                let (mut socket, _) = server.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buf = [0; 256];
                assert!(socket.read(&mut buf).unwrap() > 0);
                if cancel_early {
                    stopped.store(true, Ordering::Release);
                }
                while socket.read(&mut buf).unwrap() > 0 {}
            });
            let mut options = options();
            options.receiver_address = "127.0.0.1:7000".parse().unwrap();
            options.timeout = if cancel_early {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(30)
            };
            let active = Mutex::new(None);
            let result = probe(&options, address, &cancel, &active);
            if cancel_early {
                assert!(matches!(result, Err(Error::Cancelled)));
            } else {
                assert!(matches!(result, Err(Error::TimedOut)));
            }
            assert!(active.lock().unwrap().is_none());
            worker.join().unwrap();
        }
    }
}
