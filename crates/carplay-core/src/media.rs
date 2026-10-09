// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay airplay/{ScreenStream,AudioStream,MicrophonePacketizer}.kt.
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub const SCREEN_HEADER_LEN: usize = 128;
pub const MAX_VIDEO_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid media framing")]
    Framing,
    #[error("unsupported audio format: {0:#x}")]
    Format(u64),
    #[error("media authentication failed")]
    Authentication,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioCodec {
    Lpcm,
    AacLc,
    Opus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub codec: AudioCodec,
    pub rate: u32,
    pub channels: u8,
}

impl AudioFormat {
    pub fn from_bits(bits: u64) -> Result<Self, Error> {
        if bits.count_ones() != 1 {
            return Err(Error::Format(bits));
        }
        let (codec, rate, channels) = match bits {
            0x4 => (AudioCodec::Lpcm, 8000, 1),
            0x8 => (AudioCodec::Lpcm, 8000, 2),
            0x10 => (AudioCodec::Lpcm, 16000, 1),
            0x20 => (AudioCodec::Lpcm, 16000, 2),
            0x40 => (AudioCodec::Lpcm, 24000, 1),
            0x80 => (AudioCodec::Lpcm, 24000, 2),
            0x100 => (AudioCodec::Lpcm, 32000, 1),
            0x200 => (AudioCodec::Lpcm, 32000, 2),
            0x400 => (AudioCodec::Lpcm, 44100, 1),
            0x800 => (AudioCodec::Lpcm, 44100, 2),
            0x4000 => (AudioCodec::Lpcm, 48000, 1),
            0x8000 => (AudioCodec::Lpcm, 48000, 2),
            0x400000 => (AudioCodec::AacLc, 44100, 2),
            0x800000 => (AudioCodec::AacLc, 48000, 2),
            0x10000000 | 0x20000000 | 0x40000000 => (AudioCodec::Opus, 48000, 1),
            _ => return Err(Error::Format(bits)),
        };
        Ok(Self {
            codec,
            rate,
            channels,
        })
    }
}

#[derive(Clone, Debug)]
pub enum MediaEvent {
    VideoConfig {
        stream: u16,
        codec: VideoCodec,
        data: Vec<u8>,
    },
    VideoFrame {
        stream: u16,
        data: Vec<u8>,
        sender_ns: u64,
    },
    /// Prepare playback before exposing the stream's UDP ports in SETUP.
    /// MediaSink::send must finish preparation before returning success.
    AudioConfig {
        stream: u16,
        audio_type: String,
        format: AudioFormat,
    },
    Audio {
        stream: u16,
        audio_type: String,
        format: AudioFormat,
        timestamp: u32,
        data: Vec<u8>,
    },
    Stop {
        stream: u16,
    },
}

pub trait MediaSink: Send + Sync {
    /// Must use bounded buffering. A decoder failure is observable by the receiver.
    /// AudioConfig and Stop are completion barriers, not queued-only notifications.
    fn send(&self, event: MediaEvent) -> Result<(), String>;
}

/// One encoded capture packet. PCM is interleaved S16BE; Opus is a raw access
/// unit at 48 kHz. The receiver owns RTP clocks, encryption and network I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    pub format: AudioFormat,
    pub samples_per_packet: u32,
    pub bitrate: Option<u32>,
}

pub trait CaptureSink: Send + Sync {
    /// Must be bounded and return failures without logging captured bytes.
    fn packet(&self, bytes: Vec<u8>) -> Result<(), String>;
    fn failed(&self, reason: String);
}

pub trait CaptureSession: Send {
    /// Stop capture and release its native device. Implementations must also
    /// stop on Drop and must bound worker shutdown.
    fn stop(&mut self);
}

pub trait CaptureFactory: Send + Sync {
    /// Capability probing must not activate an input device.
    fn supports(&self, config: &CaptureConfig) -> bool;
    /// Only the receiver's enabled, authenticated media session calls start.
    fn start(
        &self,
        config: CaptureConfig,
        output: std::sync::Arc<dyn CaptureSink>,
    ) -> Result<Box<dyn CaptureSession>, String>;
}

pub fn screen_header(header: &[u8]) -> Result<(usize, u8, u64), Error> {
    if header.len() != SCREEN_HEADER_LEN {
        return Err(Error::Framing);
    }
    let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    if length > MAX_VIDEO_BODY {
        return Err(Error::Framing);
    }
    let stamp = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let ns = (stamp >> 32) * 1_000_000_000 + (((stamp & 0xffffffff) * 1_000_000_000) >> 32);
    Ok((length, header[4], ns))
}

pub fn annex_b(payload: &[u8]) -> Result<Vec<u8>, Error> {
    if payload.starts_with(&[0, 0, 0, 1]) || payload.starts_with(&[0, 0, 1]) {
        return Ok(payload.to_vec());
    }
    let mut output = Vec::with_capacity(payload.len());
    let mut offset = 0usize;
    while offset < payload.len() {
        let size = payload.get(offset..offset + 4).ok_or(Error::Framing)?;
        let length = u32::from_be_bytes(size.try_into().unwrap()) as usize;
        offset += 4;
        if length == 0 {
            return Err(Error::Framing);
        }
        let end = offset.checked_add(length).ok_or(Error::Framing)?;
        let nalu = payload.get(offset..end).ok_or(Error::Framing)?;
        output.extend_from_slice(&[0, 0, 0, 1]);
        output.extend_from_slice(nalu);
        offset = end;
    }
    Ok(output)
}

pub fn codec_config(payload: &[u8]) -> Result<(VideoCodec, Vec<u8>), Error> {
    // Bare records have no box header. Detect them before searching the screen
    // format description: NAL data itself can contain a FourCC byte sequence.
    if payload.first() == Some(&1) {
        if payload.len() >= 9 && payload[5] & 0x1f > 0 && payload[8] & 0x1f == 7 {
            return Ok((VideoCodec::H264, payload.to_vec()));
        }
        if payload.len() >= 23 {
            return Ok((VideoCodec::H265, payload.to_vec()));
        }
        return Err(Error::Framing);
    }
    for (i, code) in payload.windows(4).enumerate().skip(4) {
        if code == b"avcC" || code == b"hvcC" {
            // Screen descriptions may wrap the box in a sample entry. Its
            // declared size includes the header, not adjacent metadata boxes.
            let start = i - 4;
            let size = u32::from_be_bytes(payload[start..i].try_into().unwrap());
            let (size, header) = match size {
                0 => (payload.len() - start, 8),
                1 => {
                    let bytes = payload.get(i + 4..i + 12).ok_or(Error::Framing)?;
                    let size = usize::try_from(u64::from_be_bytes(bytes.try_into().unwrap()))
                        .map_err(|_| Error::Framing)?;
                    (size, 16)
                }
                size => (size as usize, 8),
            };
            if size <= header {
                return Err(Error::Framing);
            }
            let end = start.checked_add(size).ok_or(Error::Framing)?;
            let data = payload.get(start + header..end).ok_or(Error::Framing)?;
            if data.first() != Some(&1) {
                return Err(Error::Framing);
            }
            return Ok((
                if code == b"avcC" {
                    VideoCodec::H264
                } else {
                    VideoCodec::H265
                },
                data.to_vec(),
            ));
        }
    }
    Err(Error::Framing)
}

pub fn rtp_payload(packet: &[u8]) -> Result<(u16, u32, &[u8]), Error> {
    if packet.len() < 12 || packet[0] != 0x80 {
        return Err(Error::Framing);
    }
    Ok((
        u16::from_be_bytes(packet[2..4].try_into().unwrap()),
        u32::from_be_bytes(packet[4..8].try_into().unwrap()),
        &packet[12..],
    ))
}

/// Window rejects duplicates but tolerates out-of-order delivery and sequence wrap.
#[derive(Default)]
pub struct ReplayWindow {
    highest: Option<u64>,
    seen: u64,
}
impl ReplayWindow {
    pub fn accept(&mut self, counter: u64) -> bool {
        let Some(highest) = self.highest else {
            self.highest = Some(counter);
            self.seen = 1;
            return true;
        };
        if counter > highest {
            let shift = counter - highest;
            self.seen = if shift >= 64 {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.highest = Some(counter);
            true
        } else {
            let behind = highest - counter;
            if behind >= 64 || self.seen & (1 << behind) != 0 {
                false
            } else {
                self.seen |= 1 << behind;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codec_boxes_exclude_adjacent_metadata_and_validate_declared_bounds() {
        for (code, codec) in [(b"hvcC", VideoCodec::H265), (b"avcC", VideoCodec::H264)] {
            let data = [1, 2, 3, 4];
            let mut box_data = 12u32.to_be_bytes().to_vec();
            box_data.extend_from_slice(code);
            box_data.extend_from_slice(&data);
            let mut wrapped = vec![0; 86]; // visual sample-entry prefix
            wrapped.extend_from_slice(&box_data);
            wrapped.extend_from_slice(&8u32.to_be_bytes());
            wrapped.extend_from_slice(b"free");
            assert_eq!(codec_config(&wrapped).unwrap(), (codec, data.to_vec()));
            for size in [0, 4, 8, 13, u32::MAX] {
                let mut invalid = box_data.clone();
                invalid[..4].copy_from_slice(&size.to_be_bytes());
                if size == 0 {
                    assert_eq!(codec_config(&invalid).unwrap().1, data);
                } else {
                    assert!(codec_config(&invalid).is_err());
                }
            }
            let mut extended = 1u32.to_be_bytes().to_vec();
            extended.extend_from_slice(code);
            extended.extend_from_slice(&20u64.to_be_bytes());
            extended.extend_from_slice(&data);
            extended.extend_from_slice(b"metadata");
            assert_eq!(codec_config(&extended).unwrap().1, data);
            assert!(codec_config(&extended[..15]).is_err());
        }
    }

    #[test]
    fn raw_hevc_record_does_not_scan_parameter_sets_for_box_names() {
        let mut record = vec![0; 23];
        record[0] = 1;
        record.extend_from_slice(b"hvcC");
        assert_eq!(codec_config(&record).unwrap(), (VideoCodec::H265, record));
    }
    #[test]
    fn malformed_nal_lengths_never_escape_bounds() {
        assert_eq!(
            annex_b(&[0, 0, 0, 2, 0x65, 0xaa, 0, 0, 0, 1, 0x41]).unwrap(),
            [0, 0, 0, 1, 0x65, 0xaa, 0, 0, 0, 1, 0x41]
        );
        assert!(annex_b(&[255, 255, 255, 255]).is_err());
        assert!(annex_b(&[0, 0, 0, 2, 0x65]).is_err());
    }
    #[test]
    fn replay_window_handles_reordering_and_eviction() {
        let mut w = ReplayWindow::default();
        assert!(w.accept(70));
        assert!(w.accept(68));
        assert!(!w.accept(68));
        assert!(!w.accept(6));
        assert!(w.accept(200));
        assert!(!w.accept(70));
    }
    #[test]
    fn unknown_audio_does_not_silently_become_pcm() {
        assert!(AudioFormat::from_bits(0).is_err());
        assert!(AudioFormat::from_bits(0xc000).is_err());
        assert_eq!(AudioFormat::from_bits(0x800000).unwrap().rate, 48000);
    }
}
