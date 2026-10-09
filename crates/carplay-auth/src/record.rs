// SPDX-License-Identifier: GPL-3.0-only
//! Port of DiPlay `airplay/ControlCipher.kt`. Receive state tolerates TCP
//! fragmentation/coalescing. Any invalid frame poisons the stream permanently.
use crate::{AuthError, Result, crypto};
use zeroize::Zeroizing;
pub const MAX_WRITE_PAYLOAD: usize = 0x4000;
const MAX_RECEIVE_PAYLOAD: usize = u16::MAX as usize;

pub struct ControlWriter {
    key: Zeroizing<[u8; 32]>,
    counter: u64,
    exhausted: bool,
}
impl ControlWriter {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
            counter: 0,
            exhausted: false,
        }
    }
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let frames = plaintext.len().div_ceil(MAX_WRITE_PAYLOAD).max(1);
        if self.exhausted || (frames - 1) as u64 > u64::MAX - self.counter {
            return Err(AuthError::CounterExhausted);
        }
        let mut output = Vec::new();
        if plaintext.is_empty() {
            self.frame(&[], &mut output)?;
        }
        for chunk in plaintext.chunks(MAX_WRITE_PAYLOAD) {
            self.frame(chunk, &mut output)?;
        }
        Ok(output)
    }
    fn frame(&mut self, plaintext: &[u8], output: &mut Vec<u8>) -> Result<()> {
        let header = (plaintext.len() as u16).to_le_bytes();
        let sealed = crypto::chacha_seal(
            &self.key,
            &crypto::nonce64(self.counter),
            plaintext,
            &header,
        )?;
        self.exhausted = self.counter == u64::MAX;
        if !self.exhausted {
            self.counter += 1;
        }
        output.extend_from_slice(&header);
        output.extend_from_slice(&sealed);
        Ok(())
    }
}

pub struct ControlReader {
    key: Zeroizing<[u8; 32]>,
    counter: u64,
    buffer: Vec<u8>,
    poisoned: bool,
    exhausted: bool,
}
impl ControlReader {
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: Zeroizing::new(key),
            counter: 0,
            buffer: Vec::new(),
            poisoned: false,
            exhausted: false,
        }
    }
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }
    /// Call at transport EOF: an incomplete authenticated frame is an error.
    pub fn finish(&mut self) -> Result<()> {
        if self.poisoned || !self.buffer.is_empty() {
            self.poisoned = true;
            self.buffer.clear();
            return Err(AuthError::Authentication);
        }
        Ok(())
    }
    pub fn decrypt(&mut self, mut bytes: &[u8]) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(AuthError::Authentication);
        }
        let result = self.decode(&mut bytes);
        if result.is_err() {
            self.poisoned = true;
            self.buffer.clear();
        }
        result
    }
    fn decode(&mut self, bytes: &mut &[u8]) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        while !bytes.is_empty() {
            let target = if self.buffer.len() < 2 {
                2
            } else {
                usize::from(u16::from_le_bytes([self.buffer[0], self.buffer[1]])) + 18
            };
            debug_assert!(target <= MAX_RECEIVE_PAYLOAD + 18);
            let take = (target - self.buffer.len()).min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            *bytes = &bytes[take..];
            if self.buffer.len() < 2 {
                continue;
            }
            let length = usize::from(u16::from_le_bytes([self.buffer[0], self.buffer[1]]));
            if self.buffer.len() != length + 18 {
                continue;
            }
            if self.exhausted {
                return Err(AuthError::CounterExhausted);
            }
            let clear = crypto::chacha_open(
                &self.key,
                &crypto::nonce64(self.counter),
                &self.buffer[2..],
                &self.buffer[..2],
            )?;
            output.extend_from_slice(&clear);
            self.exhausted = self.counter == u64::MAX;
            if !self.exhausted {
                self.counter += 1;
            }
            self.buffer.clear();
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_counter_exhaustion_cannot_wrap_or_partially_encrypt() {
        let key = [0x77; 32];
        let mut writer = ControlWriter::new(key);
        writer.counter = u64::MAX;
        assert!(matches!(
            writer.encrypt(&vec![0; MAX_WRITE_PAYLOAD + 1]),
            Err(AuthError::CounterExhausted)
        ));
        let last = writer.encrypt(b"last").unwrap();
        assert!(matches!(
            writer.encrypt(b"overflow"),
            Err(AuthError::CounterExhausted)
        ));
        let mut reader = ControlReader::new(key);
        reader.counter = u64::MAX;
        assert_eq!(reader.decrypt(&last).unwrap(), b"last");
        assert!(matches!(
            reader.decrypt(&last),
            Err(AuthError::CounterExhausted)
        ));
    }

    #[test]
    fn eof_rejects_partial_header_payload_or_tag() {
        let frame = ControlWriter::new([0x19; 32]).encrypt(b"body").unwrap();
        for cut in [1, 2, 5, frame.len() - 1] {
            let mut reader = ControlReader::new([0x19; 32]);
            assert!(reader.decrypt(&frame[..cut]).unwrap().is_empty());
            assert!(reader.finish().is_err());
            assert!(reader.decrypt(&frame[cut..]).is_err());
        }
        let mut reader = ControlReader::new([0x19; 32]);
        reader.decrypt(&frame).unwrap();
        assert!(reader.finish().is_ok());
    }
}
