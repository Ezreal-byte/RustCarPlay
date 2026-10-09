// SPDX-License-Identifier: GPL-3.0-only
// Derived from DiPlay 9e244d9:
// shared/src/test/java/com/shilapi/xcertplay/transport/Iap2FileTransferReceiverTest.kt
use carplay_protocol::file_transfer::{Artwork, FileTransferReceiver, MAXIMUM_ARTWORK_BYTES};

fn setup(id: u8, size: u64, kind: u16) -> Vec<u8> {
    let mut bytes = vec![id, 4];
    bytes.extend_from_slice(&size.to_be_bytes());
    bytes.extend_from_slice(&kind.to_be_bytes());
    bytes
}
#[test]
fn source_artwork_split_across_first_and_last_datagrams() {
    let mut receiver = FileTransferReceiver::default();
    assert_eq!(receiver.accept(&setup(0x81, 5, 2)).replies, [vec![0x81, 1]]);
    assert_eq!(receiver.accept(&[0x81, 0x80, 1, 2, 3]).completed, None);
    let outcome = receiver.accept(&[0x81, 0x40, 4, 5]);
    assert_eq!(outcome.replies, [vec![0x81, 5]]);
    assert_eq!(
        outcome.completed,
        Some(Artwork {
            id: 0x81,
            bytes: vec![1, 2, 3, 4, 5]
        })
    );
    assert_eq!(receiver.buffered_bytes(), 0);
}
#[test]
fn source_oversized_unsupported_and_truncated_transfers() {
    let mut receiver = FileTransferReceiver::with_maximum_bytes(4).unwrap();
    let oversized = receiver.accept(&setup(1, 5, 2));
    assert_eq!(oversized.replies, [vec![1, 2]]);
    assert_eq!(oversized.completed.unwrap().bytes, []);
    let unsupported = receiver.accept(&setup(2, 3, 7));
    assert_eq!(unsupported.replies, [vec![2, 2]]);
    assert_eq!(unsupported.completed, None);
    assert_eq!(receiver.accept(&[4, 4]).completed, None);
    receiver.accept(&setup(3, 4, 2));
    let truncated = receiver.accept(&[3, 0x40, 1, 2]);
    assert_eq!(truncated.replies, [vec![3, 2]]);
    assert_eq!(
        truncated.completed.unwrap(),
        Artwork {
            id: 3,
            bytes: vec![]
        }
    );
}
#[test]
fn source_sender_cancel_clears_only_pending_artwork() {
    let mut receiver = FileTransferReceiver::default();
    receiver.accept(&setup(5, 4, 2));
    let cancelled = receiver.accept(&[5, 2]);
    assert_eq!(cancelled.replies.len(), 0);
    assert_eq!(
        cancelled.completed.unwrap(),
        Artwork {
            id: 5,
            bytes: vec![]
        }
    );
    assert_eq!(receiver.accept(&[5, 2]).completed, None);
}
#[test]
fn source_zero_size_artwork_completes_immediately() {
    let outcome = FileTransferReceiver::default().accept(&setup(0x80, 0, 2));
    assert_eq!(outcome.replies, [vec![0x80, 1], vec![0x80, 5]]);
    assert_eq!(
        outcome.completed.unwrap(),
        Artwork {
            id: 0x80,
            bytes: vec![]
        }
    );
}
#[test]
fn bounds_evict_with_cancel_and_reject_u64_overflow() {
    let mut receiver = FileTransferReceiver::default();
    for id in 0..4 {
        receiver.accept(&setup(id, 2, 2));
        receiver.accept(&[id, 0x80, id]);
    }
    assert_eq!(receiver.buffered_bytes(), 4);
    assert_eq!(
        receiver.accept(&setup(4, 1, 2)).replies,
        [vec![0, 2], vec![4, 1]]
    );
    assert_eq!(receiver.accept(&[0, 0x40, 1]).completed, None);
    assert_eq!(
        receiver.accept(&setup(5, u64::MAX, 2)).replies,
        [vec![5, 2]]
    );
    receiver.clear();
    assert_eq!(receiver.buffered_bytes(), 0);
    assert!(FileTransferReceiver::with_maximum_bytes(0).is_err());
    assert!(FileTransferReceiver::with_maximum_bytes(MAXIMUM_ARTWORK_BYTES + 1).is_err());
}
#[test]
fn duplicate_setup_overrun_and_completion_without_last() {
    let mut receiver = FileTransferReceiver::default();
    receiver.accept(&setup(1, 9, 2));
    receiver.accept(&[1, 0x80, 1, 2]);
    receiver.accept(&setup(1, 1, 2));
    let complete = receiver.accept(&[1, 0, 3]);
    assert_eq!(complete.completed.unwrap().bytes, [3]);
    receiver.accept(&setup(2, 1, 2));
    assert_eq!(receiver.accept(&[2, 0x40, 1, 2]).replies, [vec![2, 2]]);
    for data in [vec![], vec![1], vec![9, 0], vec![9, 5]] {
        assert_eq!(receiver.accept(&data).completed, None);
    }
    let debug = format!(
        "{:?}",
        Artwork {
            id: 1,
            bytes: b"sensitive pixels".to_vec()
        }
    );
    assert!(!debug.contains("sensitive"));
}
