// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9, shared/src/main/java/com/shilapi/xcertplay/transport/UsbMuxFrameBuffer.kt.
//! Device-side USBMUX framing (not the distinct desktop usbmuxd socket protocol).
//! Captured iOS reply trailer handling is deliberately limited to a validated four-byte boundary.
use crate::{Error, Result};
use std::collections::VecDeque;
pub const HEADER: usize = 16;
pub const MAX_FRAME: usize = 65_536;
pub const VERSION: u32 = 0;
pub const DIAGNOSTIC: u32 = 1;
pub const TCP: u32 = 6;
pub const REPLY_MAGIC: u32 = 0xfaceface;
fn u32be(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn u16be(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub protocol: u32,
    pub word8: u32,
    pub sequence: u16,
    pub acknowledgement: u16,
    pub payload: Vec<u8>,
}
impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.len() > MAX_FRAME - HEADER {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::with_capacity(HEADER + self.payload.len());
        bytes.extend(self.protocol.to_be_bytes());
        bytes.extend(((HEADER + self.payload.len()) as u32).to_be_bytes());
        bytes.extend(self.word8.to_be_bytes());
        bytes.extend(self.sequence.to_be_bytes());
        bytes.extend(self.acknowledgement.to_be_bytes());
        bytes.extend(&self.payload);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER {
            return Err(Error::Incomplete);
        }
        if bytes.len() > MAX_FRAME || u32be(bytes, 4) as usize != bytes.len() {
            return Err(Error::Invalid("USBMUX length"));
        }
        Ok(Self {
            protocol: u32be(bytes, 0),
            word8: u32be(bytes, 8),
            sequence: u16be(bytes, 12),
            acknowledgement: u16be(bytes, 14),
            payload: bytes[HEADER..].to_vec(),
        })
    }
    fn optional_padding(&self) -> bool {
        (self.protocol == VERSION && self.payload.len() == 4 && self.word8 == 2)
            || (self.word8 == REPLY_MAGIC
                && match self.protocol {
                    DIAGNOSTIC => diagnostic(&self.payload),
                    TCP => tcp(&self.payload, self.payload.len()),
                    _ => false,
                })
    }
}
fn diagnostic(bytes: &[u8]) -> bool {
    (2..=1024).contains(&bytes.len())
        && bytes[0] == 4
        && bytes[1..].iter().all(|b| (0x20..=0x7e).contains(b))
}
fn tcp(bytes: &[u8], payload_length: usize) -> bool {
    bytes.len() >= 20
        && (20..=payload_length).contains(&((bytes[12] as usize >> 4) * 4))
        && u16be(bytes, 0) != 0
        && u16be(bytes, 2) != 0
}
#[derive(Debug, Default)]
pub struct Decoder {
    bytes: VecDeque<u8>,
    optional_padding: bool,
}
impl Decoder {
    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        // One frame plus its optional trailer. Consumers must drain complete frames between pushes.
        if bytes.len() > (MAX_FRAME + 4).saturating_sub(self.bytes.len()) {
            return Err(Error::Limit);
        }
        self.bytes.extend(bytes);
        Ok(())
    }
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        if self.bytes.len() < HEADER {
            return Ok(None);
        }
        let bytes = self.bytes.make_contiguous();
        let mut length = u32be(bytes, 4) as usize;
        if !(HEADER..=MAX_FRAME).contains(&length) {
            if !self.optional_padding || u32be(bytes, 0) == TCP {
                return Err(Error::Invalid("USBMUX frame length"));
            }
            if bytes.len() < HEADER + 4 {
                return Ok(None);
            }
            let protocol = u32be(bytes, 4);
            let candidate_length = u32be(bytes, 8) as usize;
            if u32be(bytes, 12) != REPLY_MAGIC {
                return Err(Error::Invalid("USBMUX padding boundary"));
            }
            let valid = match protocol {
                TCP if (HEADER + 20..=MAX_FRAME).contains(&candidate_length) => {
                    if bytes.len() < 4 + HEADER + 20 {
                        return Ok(None);
                    }
                    tcp(&bytes[4 + HEADER..], candidate_length - HEADER)
                }
                DIAGNOSTIC if (HEADER + 2..=HEADER + 1024).contains(&candidate_length) => {
                    if bytes.len() < 4 + candidate_length {
                        return Ok(None);
                    }
                    diagnostic(&bytes[4 + HEADER..4 + candidate_length])
                }
                _ => false,
            };
            if !valid {
                return Err(Error::Invalid("USBMUX padding target"));
            }
            self.bytes.drain(..4);
            self.optional_padding = false;
            length = candidate_length;
        }
        if self.bytes.len() < length {
            return Ok(None);
        }
        let bytes: Vec<_> = self.bytes.drain(..length).collect();
        let frame = Frame::decode(&bytes)?;
        self.optional_padding = frame.optional_padding();
        Ok(Some(frame))
    }
}
