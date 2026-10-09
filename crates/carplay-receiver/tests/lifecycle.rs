// SPDX-License-Identifier: GPL-3.0-only
//! Real sockets, synthetic identity, no mDNS, Bluetooth or phone access.
use carplay_auth::{AuthError, AuthProvider};
use carplay_core::{
    config::ReceiverConfig,
    media::{MediaEvent, MediaSink},
};
use carplay_receiver::{ReceiverEvent, ReceiverHandle, ReceiverOptions, start};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

struct SyntheticAuth;
impl AuthProvider for SyntheticAuth {
    fn protocol_major(&self) -> u8 {
        3
    }
    fn certificate(&self) -> carplay_auth::Result<Vec<u8>> {
        Ok(b"synthetic".to_vec())
    }
    fn sign_challenge(&self, _: &[u8]) -> carplay_auth::Result<Vec<u8>> {
        Err(AuthError::Authentication)
    }
}
struct NoMedia;
impl MediaSink for NoMedia {
    fn send(&self, _: MediaEvent) -> Result<(), String> {
        Ok(())
    }
}
fn options(address: SocketAddr, directory: &Path) -> ReceiverOptions {
    ReceiverOptions {
        bind: address,
        config: ReceiverConfig::default(),
        bluetooth_address: "02:00:00:00:00:01".into(),
        state_dir: directory.into(),
        advertise: false,
    }
}
fn receiver(address: SocketAddr, directory: &Path) -> ReceiverHandle {
    start(
        options(address, directory),
        Arc::new(SyntheticAuth),
        Arc::new(NoMedia),
    )
    .unwrap()
}
fn await_event(receiver: &ReceiverHandle, matches: impl Fn(&ReceiverEvent) -> bool) {
    let until = Instant::now() + Duration::from_secs(3);
    loop {
        let event = receiver
            .events
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .expect("receiver lifecycle event missing");
        if matches(&event) {
            break;
        }
    }
}
fn peer(address: SocketAddr) -> TcpStream {
    let peer = TcpStream::connect_timeout(&address, Duration::from_secs(1)).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    peer
}
fn assert_closed(mut peer: TcpStream) {
    let result = peer.read(&mut [0]);
    assert!(
        matches!(result, Ok(0))
            || result.is_err_and(|e| matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )),
        "the previous accepted socket remains open"
    );
}

#[test]
fn cancellation_joins_idle_control_and_rebinds_exact_address_ipv4_and_ipv6() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let directory = tempfile::tempdir().unwrap();
        let mut server = receiver(address.parse().unwrap(), directory.path());
        let address = server.address;
        for _ in 0..5 {
            let peer = peer(address);
            await_event(&server, |e| matches!(e, ReceiverEvent::TcpAccepted));
            let started = Instant::now();
            server.stop();
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_closed(peer);
            server = receiver(address, directory.path());
        }
        server.stop();
    }
}

#[test]
fn disconnected_peer_and_invalid_request_allow_immediate_next_connection() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let directory = tempfile::tempdir().unwrap();
        let mut server = receiver(address.parse().unwrap(), directory.path());
        for invalid in [false, true, false] {
            let mut peer = peer(server.address);
            await_event(&server, |e| matches!(e, ReceiverEvent::TcpAccepted));
            if invalid {
                peer.write_all(b"INVALID\r\n\r\n").unwrap();
                await_event(&server, |e| matches!(e, ReceiverEvent::Error(_)));
                assert_closed(peer);
            } else {
                peer.write_all(b"GET /info RTSP/1.0\r\nCSeq: 1\r\n\r\n")
                    .unwrap();
                let mut response = [0; 64];
                let n = peer.read(&mut response).unwrap();
                assert!(
                    std::str::from_utf8(&response[..n])
                        .unwrap()
                        .contains("200 OK")
                );
                drop(peer);
            }
            await_event(&server, |e| matches!(e, ReceiverEvent::Disconnected));
        }
        let address = server.address;
        server.stop();
        receiver(address, directory.path()).stop();
    }
}

#[test]
fn startup_failure_after_binding_drops_listener() {
    let directory = tempfile::tempdir().unwrap();
    let temporary = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = temporary.local_addr().unwrap();
    drop(temporary);
    let mut invalid = options(address, directory.path());
    invalid.advertise = true; // Loopback discovery is rejected after TCP bind.
    assert!(start(invalid, Arc::new(SyntheticAuth), Arc::new(NoMedia)).is_err());
    receiver(address, directory.path()).stop();
}
