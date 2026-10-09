// SPDX-License-Identifier: GPL-3.0-only
//! Strict pairing TLV8 codec, ported from DiPlay `airplay/Tlv8Codec.kt`.
use crate::{AuthError, Result};
use std::collections::BTreeMap;
pub const MAX_PAIRING_BODY: usize = 64 * 1024;

pub fn encode(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut previous = None;
    for &(kind, value) in items {
        if previous == Some(kind) {
            output.extend_from_slice(&[255, 0]);
        }
        if value.is_empty() {
            output.extend_from_slice(&[kind, 0]);
        }
        for fragment in value.chunks(255) {
            output.extend_from_slice(&[kind, fragment.len() as u8]);
            output.extend_from_slice(fragment);
        }
        previous = Some(kind);
    }
    output
}

/// Pairing messages have one value per type. Only contiguous 255-byte TLV
/// fragments may repeat; ambiguous duplicates and truncated fields are rejected.
pub fn decode(input: &[u8]) -> Result<BTreeMap<u8, Vec<u8>>> {
    if input.len() > MAX_PAIRING_BODY {
        return Err(AuthError::InvalidInput("TLV8 size limit"));
    }
    let mut output: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut position = 0;
    let mut previous = None;
    while position < input.len() {
        if input.len() - position < 2 {
            return Err(AuthError::InvalidInput("truncated TLV8 header"));
        }
        let kind = input[position];
        let length = input[position + 1] as usize;
        position += 2;
        if input.len() - position < length {
            return Err(AuthError::InvalidInput("truncated TLV8 value"));
        }
        if kind == 255 {
            if length != 0 {
                return Err(AuthError::InvalidInput("invalid TLV8 separator"));
            }
        } else if previous == Some((kind, 255)) {
            output
                .get_mut(&kind)
                .ok_or(AuthError::InvalidInput("invalid TLV8 fragment"))?
                .extend_from_slice(&input[position..position + length]);
        } else {
            if output.contains_key(&kind) {
                return Err(AuthError::InvalidInput("duplicate TLV8 field"));
            }
            output.insert(kind, input[position..position + length].to_vec());
        }
        position += length;
        previous = Some((kind, length));
    }
    Ok(output)
}
