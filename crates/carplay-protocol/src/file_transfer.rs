// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9:
// shared/src/main/java/com/shilapi/xcertplay/transport/Iap2FileTransferReceiver.kt
//! Bounded iAP2 v2 NowPlayingArtworkData datagrams (file-transfer session type 1).
//! Feed complete link-session datagrams. Reply bytes must keep their datagram boundary.
use crate::{Error, Result};
use std::{collections::VecDeque, fmt};

pub const MAXIMUM_ARTWORK_BYTES: usize = 2 * 1024 * 1024;
pub const MAXIMUM_PENDING_TRANSFERS: usize = 4;

#[derive(Clone, PartialEq, Eq)]
pub struct Artwork {
    pub id: u8,
    /// An empty completion clears the previous artwork, including a refused cover.
    pub bytes: Vec<u8>,
}
impl fmt::Debug for Artwork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Artwork")
            .field("id", &self.id)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub replies: Vec<Vec<u8>>,
    pub completed: Option<Artwork>,
}
struct Pending {
    id: u8,
    expected: usize,
    bytes: Vec<u8>,
}
pub struct FileTransferReceiver {
    maximum_bytes: usize,
    pending: VecDeque<Pending>,
}
impl Default for FileTransferReceiver {
    fn default() -> Self {
        Self {
            maximum_bytes: MAXIMUM_ARTWORK_BYTES,
            pending: VecDeque::new(),
        }
    }
}
impl FileTransferReceiver {
    pub fn with_maximum_bytes(maximum_bytes: usize) -> Result<Self> {
        if maximum_bytes == 0 || maximum_bytes > MAXIMUM_ARTWORK_BYTES {
            return Err(Error::Limit);
        }
        Ok(Self {
            maximum_bytes,
            pending: VecDeque::new(),
        })
    }
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn buffered_bytes(&self) -> usize {
        self.pending.iter().map(|p| p.bytes.len()).sum()
    }
    pub fn accept(&mut self, datagram: &[u8]) -> Outcome {
        let [id, flags, ..] = *datagram else {
            return Outcome::default();
        };
        match flags & 0x0f {
            4 => self.setup(id, datagram),
            0 => self.data(id, flags, &datagram[2..]),
            2 => {
                if self.remove(id).is_some() {
                    Outcome {
                        completed: Some(Artwork { id, bytes: vec![] }),
                        ..Default::default()
                    }
                } else {
                    Outcome::default()
                }
            }
            _ => Outcome::default(),
        }
    }
    fn remove(&mut self, id: u8) -> Option<Pending> {
        let position = self.pending.iter().position(|p| p.id == id)?;
        self.pending.remove(position)
    }
    fn setup(&mut self, id: u8, datagram: &[u8]) -> Outcome {
        self.remove(id);
        if datagram.len() < 12 || datagram[10..12] != [0, 2] {
            return Outcome {
                replies: vec![vec![id, 2]],
                ..Default::default()
            };
        }
        let size = u64::from_be_bytes(datagram[2..10].try_into().expect("checked length"));
        if size > self.maximum_bytes as u64 {
            return rejected(id);
        }
        if size == 0 {
            return Outcome {
                replies: vec![vec![id, 1], vec![id, 5]],
                completed: Some(Artwork { id, bytes: vec![] }),
            };
        }
        let mut replies = Vec::new();
        if self.pending.len() >= MAXIMUM_PENDING_TRANSFERS {
            // The reference evicts its oldest transfer. Also cancel it explicitly
            // so the sender is not left awaiting a completion that cannot arrive.
            if let Some(old) = self.pending.pop_front() {
                replies.push(vec![old.id, 2]);
            }
        }
        self.pending.push_back(Pending {
            id,
            expected: size as usize,
            bytes: Vec::new(),
        });
        replies.push(vec![id, 1]);
        Outcome {
            replies,
            completed: None,
        }
    }
    fn data(&mut self, id: u8, flags: u8, bytes: &[u8]) -> Outcome {
        let Some(index) = self.pending.iter().position(|p| p.id == id) else {
            return Outcome::default();
        };
        let pending = &mut self.pending[index];
        if bytes.len() > pending.expected - pending.bytes.len() {
            self.pending.remove(index);
            return rejected(id);
        }
        pending.bytes.extend_from_slice(bytes);
        if flags & 0x40 == 0 && pending.bytes.len() < pending.expected {
            return Outcome::default();
        }
        let pending = self.pending.remove(index).expect("known pending transfer");
        if pending.bytes.len() != pending.expected {
            return rejected(id);
        }
        Outcome {
            replies: vec![vec![id, 5]],
            completed: Some(Artwork {
                id,
                bytes: pending.bytes,
            }),
        }
    }
}
fn rejected(id: u8) -> Outcome {
    Outcome {
        replies: vec![vec![id, 2]],
        completed: Some(Artwork { id, bytes: vec![] }),
    }
}
