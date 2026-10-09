// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay airplay/MicrophonePacketizer.kt and CarPlayMediaEngine.kt.
use crate::server::ReceiverEvent;
use anyhow::{Result, ensure};
use carplay_auth::crypto;
use carplay_core::media::{
    AudioCodec, AudioFormat, CaptureConfig, CaptureFactory, CaptureSession, CaptureSink,
};
use std::{
    net::{SocketAddr, UdpSocket},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
    },
    time::Duration,
};

#[derive(Clone, Copy)]
pub(crate) struct Spec {
    pub capture: CaptureConfig,
    pub rtp_samples: u32,
    pub port: u16,
}

pub(crate) fn capture_config(bits: u64, frames: u32) -> Option<(CaptureConfig, u32)> {
    let format = AudioFormat::from_bits(bits).ok()?;
    if format.channels != 1 {
        return None;
    }
    match format.codec {
        AudioCodec::Lpcm => {
            let millis = if frames == 0 {
                20
            } else {
                ((u64::from(frames) * 1000 + u64::from(format.rate) / 2) / u64::from(format.rate))
                    .clamp(5, 60) as u32
            };
            let samples = format.rate * millis / 1000;
            Some((
                CaptureConfig {
                    format,
                    samples_per_packet: samples,
                    bitrate: None,
                },
                samples,
            ))
        }
        AudioCodec::Opus => {
            let (bitrate, clock) = match bits {
                0x40000000 => (96_000, 48_000),
                0x20000000 => (64_000, 24_000),
                0x10000000 => (48_000, 48_000),
                _ => return None,
            };
            Some((
                CaptureConfig {
                    format,
                    samples_per_packet: 960,
                    bitrate: Some(bitrate),
                },
                clock / 50,
            ))
        }
        AudioCodec::AacLc => None,
    }
}

pub(crate) fn negotiate(
    kind: u16,
    audio_type: &str,
    data: &plist::Dictionary,
    enabled: bool,
    factory: Option<&Arc<dyn CaptureFactory>>,
) -> std::result::Result<Option<Spec>, u16> {
    let port = match data.get("dataPort") {
        None => return Ok(None),
        Some(value) => value.as_unsigned_integer().ok_or(400u16)?,
    };
    if port == 0 {
        return Ok(None);
    }
    let port = u16::try_from(port).map_err(|_| 400u16)?;
    if !enabled || kind != 100 || !matches!(audio_type, "telephony" | "speechrecognition") {
        return Err(501);
    }
    let bits = data
        .get("audioFormat")
        .and_then(plist::Value::as_unsigned_integer)
        .ok_or(400u16)?;
    let frames = match data.get("framesPerPacket") {
        None => 0,
        Some(value) => {
            u32::try_from(value.as_unsigned_integer().ok_or(400u16)?).map_err(|_| 400u16)?
        }
    };
    let (capture, rtp_samples) = capture_config(bits, frames).ok_or(501u16)?;
    if !factory.is_some_and(|factory| factory.supports(&capture)) {
        return Err(501);
    }
    Ok(Some(Spec {
        capture,
        rtp_samples,
        port,
    }))
}

struct Packetizer {
    key: zeroize::Zeroizing<[u8; 32]>,
    sequence: u16,
    timestamp: u32,
    nonce: Option<u64>,
    rtp_samples: u32,
    expected_pcm: Option<usize>,
}
impl Packetizer {
    fn new(key: [u8; 32], spec: Spec) -> Self {
        Self {
            key: zeroize::Zeroizing::new(key),
            sequence: 0,
            timestamp: 0,
            nonce: Some(0),
            rtp_samples: spec.rtp_samples,
            expected_pcm: (spec.capture.format.codec == AudioCodec::Lpcm).then_some(
                spec.capture.samples_per_packet as usize
                    * usize::from(spec.capture.format.channels)
                    * 2,
            ),
        }
    }
    fn seal(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 65_471,
            "invalid microphone payload length"
        );
        if let Some(expected) = self.expected_pcm {
            ensure!(
                bytes.len() == expected,
                "microphone PCM packet has wrong length"
            );
        } else {
            ensure!(
                bytes.len() <= 1275,
                "Opus microphone packet exceeds one frame"
            );
        }
        let counter = self
            .nonce
            .ok_or_else(|| anyhow::anyhow!("microphone nonce exhausted"))?;
        let mut header = [0u8; 12];
        header[0] = 0x80;
        header[1] = 100;
        header[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        header[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        let encrypted =
            crypto::chacha_seal(&self.key, &crypto::nonce64(counter), bytes, &header[4..])?;
        let mut packet = Vec::with_capacity(12 + encrypted.len() + 8);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&encrypted);
        packet.extend_from_slice(&counter.to_le_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(self.rtp_samples);
        self.nonce = counter.checked_add(1);
        Ok(packet)
    }
}

struct Uplink {
    socket: UdpSocket,
    packetizer: Mutex<Packetizer>,
    cancel: Arc<AtomicBool>,
    failure: Arc<AtomicBool>,
    events: SyncSender<ReceiverEvent>,
}
impl CaptureSink for Uplink {
    fn packet(&self, bytes: Vec<u8>) -> std::result::Result<(), String> {
        if self.cancel.load(Ordering::Acquire) {
            return Err("microphone cancelled".into());
        }
        let packet = self
            .packetizer
            .lock()
            .map_err(|_| "microphone lock poisoned".to_string())?
            .seal(&bytes)
            .map_err(|e| e.to_string())?;
        self.socket
            .send(&packet)
            .map_err(|e| format!("microphone UDP send: {e}"))?;
        Ok(())
    }
    fn failed(&self, reason: String) {
        if !self.cancel.load(Ordering::Acquire) {
            let _ = self.events.try_send(ReceiverEvent::Error(format!(
                "microphone capture: {reason}"
            )));
            self.failure.store(true, Ordering::Release);
        }
    }
}

pub(crate) struct Pending {
    pub spec: Spec,
    pub factory: Arc<dyn CaptureFactory>,
    pub key: zeroize::Zeroizing<[u8; 32]>,
    pub local: SocketAddr,
    pub peer: SocketAddr,
    pub cancel: Arc<AtomicBool>,
    pub failure: Arc<AtomicBool>,
    pub events: SyncSender<ReceiverEvent>,
}
impl Pending {
    pub fn start(self) -> Result<Box<dyn CaptureSession>> {
        let mut local = self.local;
        local.set_port(0);
        let mut peer = self.peer;
        peer.set_port(self.spec.port);
        let socket = UdpSocket::bind(local)?;
        socket.connect(peer)?;
        socket.set_write_timeout(Some(Duration::from_millis(100)))?;
        let output = Arc::new(Uplink {
            socket,
            packetizer: Mutex::new(Packetizer::new(*self.key, self.spec)),
            cancel: self.cancel,
            failure: self.failure,
            events: self.events,
        });
        self.factory
            .start(self.spec.capture, output)
            .map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(text: &str) -> Vec<u8> {
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
    #[test]
    fn independent_input_key_and_rtp_packet_vectors() {
        // Independently generated with .NET HMACSHA512 HKDF extract/expand and
        // System.Security.Cryptography.ChaCha20Poly1305, using public test bytes.
        let key: [u8; 32] = std::array::from_fn(|n| n as u8);
        let input = crypto::derive_key(
            &key,
            b"DataStream-Salt18446744073709551615",
            b"DataStream-Input-Encryption-Key",
        )
        .unwrap();
        assert_eq!(
            input.as_slice(),
            hex("4115f0c9b013c6fe1f91c5bd67107d6a3e50ffb3df7db649e1ef0cb662e0c259")
        );
        let output = crypto::derive_key(
            &key,
            b"DataStream-Salt18446744073709551615",
            b"DataStream-Output-Encryption-Key",
        )
        .unwrap();
        assert_ne!(*input, *output);
        let (mut capture, _) = capture_config(0x10, 0).unwrap();
        capture.samples_per_packet = 2;
        let mut packetizer = Packetizer::new(
            key,
            Spec {
                capture,
                rtp_samples: 3,
                port: 7000,
            },
        );
        packetizer.sequence = u16::MAX;
        packetizer.timestamp = u32::MAX - 1;
        packetizer.nonce = Some(0x0102030405060708);
        assert_eq!(
            packetizer.seal(&[0x12, 0x34, 0xfe, 0xdc]).unwrap(),
            hex(concat!(
                "8064",
                "ffff",
                "fffffffe",
                "00000000",
                "e27d5338e3e7c3d0f8a18413faac45acf5264be0",
                "0807060504030201"
            ))
        );
        assert_eq!(packetizer.sequence, 0);
        assert_eq!(packetizer.timestamp, 1);
        assert_eq!(packetizer.nonce, Some(0x0102030405060709));
    }
    #[test]
    fn opus_capture_clock_and_rtp_clock_are_distinct() {
        for (bits, bitrate, timestamp_step) in [
            (0x10000000, 48_000, 960),
            (0x20000000, 64_000, 480),
            (0x40000000, 96_000, 960),
        ] {
            let (capture, step) = capture_config(bits, 1).unwrap();
            assert_eq!(capture.format.rate, 48_000);
            assert_eq!(capture.samples_per_packet, 960);
            assert_eq!(capture.bitrate, Some(bitrate));
            assert_eq!(step, timestamp_step);
        }
        assert!(capture_config(0x800000, 0).is_none());
        assert!(capture_config(0x20, 0).is_none());
        let (capture, _) = capture_config(0x10, 80).unwrap();
        assert_eq!(capture.samples_per_packet, 80);
    }
    #[test]
    fn nonce_never_wraps_and_invalid_pcm_does_not_consume_it() {
        let (capture, rtp_samples) = capture_config(0x10, 0).unwrap();
        let mut packetizer = Packetizer::new(
            [0; 32],
            Spec {
                capture,
                rtp_samples,
                port: 7000,
            },
        );
        let payload = vec![0; capture.samples_per_packet as usize * 2];
        assert!(packetizer.seal(&[1]).is_err());
        assert_eq!(packetizer.nonce, Some(0));
        packetizer.nonce = Some(u64::MAX);
        packetizer.seal(&payload).unwrap();
        assert!(packetizer.seal(&payload).is_err());
    }
}
