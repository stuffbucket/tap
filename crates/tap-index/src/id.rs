//! Stable segment ids.

use tap_model::PaneId;

/// FNV-1a, 64-bit. Chosen over `std`'s hasher because ids are stored (in
/// edit lists, as `SegId`) and must not change between builds or releases.
fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Separate the parts so ("ab", "c") and ("a", "bc") differ.
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The id of a segment: a hash of its pane, its kind and its first stream
/// byte, as 16 hex digits. The kind is included because a command, its first
/// paragraph and an idle gap can start at the same byte; idle gaps, which
/// cover no bytes, also include their start time.
pub fn segment_id(pane: &PaneId, kind: &str, start_byte: u64, start_us: Option<u64>) -> String {
    let byte = start_byte.to_le_bytes();
    let time = start_us.map(u64::to_le_bytes);
    let mut parts: Vec<&[u8]> = vec![pane.as_str().as_bytes(), kind.as_bytes(), &byte];
    if let Some(time) = &time {
        parts.push(time);
    }
    format!("{:016x}", fnv1a(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_fixed_and_distinguish_their_parts() {
        let pane = PaneId::new("main").unwrap();
        // Pinned: ids are stored in edit lists, so this value must never change.
        assert_eq!(segment_id(&pane, "command", 42, None), "65852b12c91f7a96");
        assert_ne!(
            segment_id(&pane, "command", 42, None),
            segment_id(&pane, "paragraph", 42, None)
        );
        assert_ne!(
            segment_id(&pane, "idle", 42, Some(1)),
            segment_id(&pane, "idle", 42, Some(2))
        );
    }
}
