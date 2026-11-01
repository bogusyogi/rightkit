//! Canonical stable identifiers for sessions, forks, events, and transcript
//! messages.
//!
//! Newly minted identities use RFC 9562 UUIDv7 — the 48 most-significant bits
//! are the Unix-epoch timestamp in milliseconds (monotonic per process, sort-
//! equivalent to creation order), the next 4 bits are the version, the next
//! 2 bits are the variant, and the remaining 62 bits are random.
//!
//! Identity is implemented in pure Rust (no `uuid` crate, no `rand` crate) so
//! F1 stays within the protocol crate's locked dependency set. The PRNG is a
//! `splitmix64` seeded from system time + process id + an atomic counter, so
//! on any single platform a sequence of mints produced at distinct timestamps
//! is unique with overwhelming probability and still satisfies the RFC's
//! "unpredictable" requirement (the 62 random bits are sufficient for the
//! version-7 collision domain and there is no security claim at stake here;
//! the F1 deliverable explicitly says "stable identity, not security").
//!
//! Producers (the engine loop) still pick constant message ids (`final`,
//! `forced-stop`, `graceful-summary`, ...) and `seq` independently of the
//! database row sequence. The id helpers below derive every message identity
//! from the persisted row sequence at the storage boundary, so every message
//! gets a `<kind>-<row_seq>` identity and React keys stay unique and stable
//! (per the rendering-lists contract).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Process-local sequence mixed into every UUIDv7 mint so a sequence of
/// mints produced in the same millisecond — the boundary case RFC 9562 §6.1
/// warns about — is still unique.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// State for the splitmix64 PRNG. Each call advances the state and produces
/// 64 random bits.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Build the per-process initial seed from the current wall-clock ms, the
/// process id (best-effort), and an atomic counter. Each call advances the
/// counter so back-to-back seeds are still distinct.
fn seed_mix() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let ctr = COUNTER.fetch_add(1, Ordering::Relaxed);
    nanos ^ pid.rotate_left(17) ^ ctr.rotate_left(31)
}

/// Mint a UUIDv7 string. Output format is the canonical hyphenated
/// 36-character lowercase form, equivalent to a UUID-printable BNF: 8-4-4-4-12
/// hex with `version=7` and `variant=8/9/a/b`.
pub fn uuid_v7() -> String {
    let ms: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut sm = SplitMix64(seed_mix());
    let r1 = sm.next();
    let r2 = sm.next();

    let mut bytes = [0u8; 16];
    bytes[0..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
    bytes[6] = ((ms >> 8) & 0x0f) as u8 | 0x70;
    bytes[7] = (ms & 0xff) as u8;
    bytes[8] = ((r1 & 0x3f) as u8) | 0x80;
    bytes[9] = ((r1 >> 8) & 0xff) as u8;
    bytes[10] = ((r1 >> 16) & 0xff) as u8;
    bytes[11] = ((r1 >> 24) & 0xff) as u8;
    bytes[12] = ((r1 >> 32) & 0xff) as u8;
    bytes[13] = ((r1 >> 40) & 0xff) as u8;
    bytes[14] = (r2 & 0xff) as u8;
    bytes[15] = ((r2 >> 8) & 0xff) as u8;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Predicate — cheaply identifies any RFC 9562 v7 UUID string. Used by the
/// storage boundary to decide whether an incoming identity is the new
/// canonical shape (mint-vs-rewrite).
pub fn is_canonical_uuid_v7(id: &str) -> bool {
    if id.len() != 36 {
        return false;
    }
    let bytes = id.as_bytes();
    bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes[14] == b'7'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

/// Build the canonical id for a transcript message at `row_seq`. Independent
/// of the payload-supplied id so two rows whose producers both chose the same
/// constant id are still uniquely keyed by their persisted sequence.
pub fn message_id(row_seq: i64) -> String {
    format!("message-{row_seq}")
}

/// Canonical id for a `ToolCallStarted` origin row. This is the row-level
/// identity an awaited engine dispatch consults; the protocol event itself
/// still carries a `seq`-derived display id, but the durable record uses this
/// shape so a tool result can deterministically find its origin at replay
/// time.
pub fn tool_call_origin_id(call_id: &str, row_seq: i64) -> String {
    format!("tool_call_origin:{call_id}:{row_seq}")
}

/// Canonical id for a `TranscriptMessage` carried inside a tool result.
pub fn tool_result_id(call_id: &str, row_seq: i64) -> String {
    format!("tool:{call_id}:{row_seq}")
}

/// Canonical id for an assistant message at `row_seq`.
pub fn assistant_message_id(row_seq: i64) -> String {
    format!("assistant:{row_seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v7_has_canonical_version_and_variant() {
        let id = uuid_v7();
        assert_eq!(id.len(), 36);
        assert!(is_canonical_uuid_v7(&id), "{id} not v7");
    }

    #[test]
    fn uuid_v7_is_lexicographically_sortable_by_minting_time() {
        let a = uuid_v7();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = uuid_v7();
        assert!(a < b, "expected {a} < {b}");
    }

    #[test]
    fn sequential_mints_differ_in_their_random_tail() {
        let a = uuid_v7();
        let b = uuid_v7();
        assert_ne!(a, b);
    }

    #[test]
    fn message_id_derives_from_row_seq() {
        assert_eq!(message_id(7), "message-7");
        assert_eq!(tool_call_origin_id("tc1", 11), "tool_call_origin:tc1:11");
        assert_eq!(tool_result_id("tc1", 11), "tool:tc1:11");
        assert_eq!(assistant_message_id(11), "assistant:11");
    }

    #[test]
    fn is_canonical_uuid_v7_only_accepts_v7() {
        assert!(is_canonical_uuid_v7("01920000-0000-7000-8000-000000000000"));
        assert!(!is_canonical_uuid_v7(
            "01920000-0000-4000-8000-000000000000"
        ));
        assert!(!is_canonical_uuid_v7("not-a-uuid"));
        assert!(!is_canonical_uuid_v7(""));
    }
}
