// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9, shared/src/main/java/com/shilapi/xcertplay/transport/Ntb16Codec.kt.
//! NTB16 codec. The USB/NCM control plane and OS network interface are separate concerns.
//! Malformed NDP chains/entries are rejected instead of silently yielding a partial Ethernet frame list.
use crate::{Error, Result};
pub const MAX_DATAGRAM: usize = 65_507;
fn u16le(b: &[u8], i: usize) -> usize {
    u16::from_le_bytes([b[i], b[i + 1]]) as usize
}
fn put16(b: &mut [u8], i: usize, n: usize) {
    b[i..i + 2].copy_from_slice(&(n as u16).to_le_bytes());
}
pub fn encode(frame: &[u8], sequence: u16) -> Result<Vec<u8>> {
    if frame.is_empty() || frame.len() > MAX_DATAGRAM {
        return Err(Error::Limit);
    }
    let size = 28 + frame.len();
    let mut out = vec![0; size + usize::from(size.is_multiple_of(512))];
    out[..4].copy_from_slice(b"NCMH");
    put16(&mut out, 4, 12);
    put16(&mut out, 6, sequence as usize);
    put16(&mut out, 8, size);
    put16(&mut out, 10, 12);
    out[12..16].copy_from_slice(b"NCM0");
    put16(&mut out, 16, 16);
    put16(&mut out, 20, 28);
    put16(&mut out, 22, frame.len());
    out[28..size].copy_from_slice(frame);
    Ok(out)
}
pub fn decode(block: &[u8]) -> Result<Vec<&[u8]>> {
    if block.len() < 12 {
        return Err(Error::Incomplete);
    }
    if &block[..4] != b"NCMH" || u16le(block, 4) != 12 {
        return Err(Error::Invalid("NTH16 header"));
    }
    let size = u16le(block, 8);
    if size < 12 || size > block.len() {
        return Err(Error::Invalid("NTB16 block length"));
    }
    let block = &block[..size];
    let mut ndp = u16le(block, 10);
    let mut seen = std::collections::HashSet::new();
    let mut frames = Vec::new();
    while ndp != 0 {
        if !seen.insert(ndp) {
            return Err(Error::Invalid("cyclic NDP16 chain"));
        }
        if ndp < 12 || ndp + 12 > size || &block[ndp..ndp + 4] != b"NCM0" {
            return Err(Error::Invalid("NDP16 header or CRC mode"));
        }
        let length = u16le(block, ndp + 4);
        if length < 12 || !length.is_multiple_of(4) || ndp + length > size {
            return Err(Error::Invalid("NDP16 table length"));
        }
        let mut terminated = false;
        for entry in (ndp + 8..ndp + length).step_by(4) {
            let index = u16le(block, entry);
            let length = u16le(block, entry + 2);
            if index == 0 && length == 0 {
                terminated = true;
                break;
            }
            if index < 12 || length == 0 || index + length > size {
                return Err(Error::Invalid("NDP16 datagram bounds"));
            }
            frames.push(&block[index..index + length]);
        }
        if !terminated {
            return Err(Error::Invalid("missing NDP16 terminator"));
        }
        ndp = u16le(block, ndp + 6);
    }
    Ok(frames)
}
