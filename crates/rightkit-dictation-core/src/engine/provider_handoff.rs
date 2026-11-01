//! BYOK provider-key hand-off from the app (owner of the OS credential store) to the engine
//! sidecar, on `rightkit-credentials` `handoff` (rightkit#886).
//!
//! The frame, the snapshot-replacement rule and the acknowledgment are the owner's
//! (`Message`, `Snapshot`, `expect_ack`); nothing here defines a layout. The sidecar's stdin and
//! stdout belong to the job-queue JSON-RPC channel (`rightkit-job-queue` owns the pipes and
//! exposes no raw pre-hello write), so the owner's frame rides that same private channel as one
//! hex string inside the `set_provider_keys` request, and the owner's Ack comes back hex-encoded
//! in the reply. It never reaches argv or the environment, and every buffer holding it is wiped.

use std::io::Cursor;
use std::process::Command;

use rightkit_credentials::handoff::{
    expect_ack, read_message, receive_snapshot_delivery, write_message, ErrorCode, HandoffError,
    Limits, Message, Snapshot, SnapshotDelivery,
};
use rightkit_credentials::Secret;
use zeroize::Zeroizing;

use super::secret::SecretString;

/// Logical names of the provider slots (`[a-z0-9._-]`, as the owner requires).
pub const GROQ: &str = "groq";
pub const NVIDIA: &str = "nvidia";
pub const OPENROUTER: &str = "openrouter";
/// Every name the sidecar may receive; a snapshot always reports exactly these.
pub const PROVIDER_KEY_NAMES: [&str; 3] = [GROQ, NVIDIA, OPENROUTER];

/// Ambient environment copies of the same secrets (plus the retired Cerebras and NIM aliases)
/// that must never reach the sidecar through inheritance.
pub const AMBIENT_KEY_ENV: [&str; 5] = [
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "NVIDIA_API_KEY",
    "NIM_API_KEY",
    "OPENROUTER_API_KEY",
];

/// Remove ambient provider-key variables from a sidecar command (owner `scrub_inherited_env`).
pub fn scrub_sidecar_env(command: &mut Command) {
    rightkit_credentials::handoff::scrub_inherited_env(command, &AMBIENT_KEY_ENV);
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_encode(bytes: &[u8]) -> SecretString {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    SecretString::new(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_decode(text: &str) -> Result<Zeroizing<Vec<u8>>, HandoffError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(HandoffError::Malformed("hex carrier has an odd length"));
    }
    let mut out = Zeroizing::new(Vec::with_capacity(bytes.len() / 2));
    for pair in bytes.as_chunks::<2>().0 {
        match (hex_nibble(pair[0]), hex_nibble(pair[1])) {
            (Some(high), Some(low)) => out.push((high << 4) | low),
            _ => return Err(HandoffError::Malformed("hex carrier has a non-hex byte")),
        }
    }
    Ok(out)
}

/// What the owner (the app) knows about one provider slot when it builds a snapshot.
pub enum KeySlot {
    /// A non-blank key was read from the OS store.
    Present(SecretString),
    /// The store authoritatively reports no key.
    Absent,
    /// The store could not be read: the sidecar must fail closed and wipe its copy.
    ReadFailed,
}

/// Spawner side: the owner's snapshot of the three provider slots, as one hex frame. The slots'
/// secrets are copied into zeroized `Secret`s and the originals wiped as `slots` drops.
pub fn encode_slots_frame(
    id: u64,
    groq: KeySlot,
    nvidia: KeySlot,
    openrouter: KeySlot,
) -> Result<SecretString, HandoffError> {
    let mut snapshot = Snapshot::new();
    for (name, slot) in [(GROQ, groq), (NVIDIA, nvidia), (OPENROUTER, openrouter)] {
        match slot {
            KeySlot::Present(key) => {
                snapshot.insert_present(name, Secret::from_string(key.expose().to_owned()))?
            }
            KeySlot::Absent => snapshot.insert_absent(name)?,
            KeySlot::ReadFailed => snapshot.insert_failed(name, ErrorCode::StoreError)?,
        }
    }
    encode_snapshot_frame(id, snapshot)
}

/// Spawner side: the owner's snapshot Reply (a full replacement) for correlation id `id`,
/// encoded for the `set_provider_keys` request. The snapshot is consumed and wiped.
pub fn encode_snapshot_frame(id: u64, snapshot: Snapshot) -> Result<SecretString, HandoffError> {
    let message = Message::reply_snapshot(id, snapshot);
    // Sized up front so the buffer never reallocates (a realloc would strand an unwiped copy).
    let mut frame = Zeroizing::new(Vec::with_capacity(4096));
    write_message(&mut *frame, &message)?;
    drop(message);
    Ok(hex_encode(&frame))
}

/// Sidecar side: read exactly one snapshot Reply from the carrier. Anything left after the
/// frame is refused. Apply it with `Snapshot::replace_into`, then answer with [`encode_ack`].
pub fn decode_snapshot_frame(frame: &SecretString) -> Result<SnapshotDelivery, HandoffError> {
    let bytes = hex_decode(frame.expose())?;
    let mut cursor = Cursor::new(&bytes[..]);
    let delivery = receive_snapshot_delivery(&mut cursor, &Limits::default())?;
    if cursor.position() as usize != bytes.len() {
        return Err(HandoffError::TrailingBytes);
    }
    Ok(delivery)
}

/// Sidecar side: the owner's Ack for delivery `id`, hex-encoded. Carries no secret.
pub fn encode_ack(id: u64) -> Result<String, HandoffError> {
    let mut frame = Vec::new();
    write_message(&mut frame, &Message::ack(id))?;
    Ok(hex_encode(&frame).expose().to_owned())
}

/// Spawner side: accept the engine's reply only if it is the Ack for `id` (owner `expect_ack`).
pub fn check_ack(carrier: &str, id: u64) -> Result<(), HandoffError> {
    let bytes = hex_decode(carrier)?;
    let mut cursor = Cursor::new(&bytes[..]);
    let message = read_message(&mut cursor, &Limits::default())?;
    if cursor.position() as usize != bytes.len() {
        return Err(HandoffError::TrailingBytes);
    }
    expect_ack(message, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_credentials::handoff::{Entry, SecretBundle};

    fn snapshot(groq: Option<&str>, nvidia_failed: bool) -> Snapshot {
        let mut snapshot = Snapshot::new();
        match groq {
            Some(key) => snapshot
                .insert_present(GROQ, Secret::from_string(key))
                .unwrap(),
            None => snapshot.insert_absent(GROQ).unwrap(),
        }
        if nvidia_failed {
            snapshot
                .insert_failed(NVIDIA, ErrorCode::StoreError)
                .unwrap();
        } else {
            snapshot.insert_absent(NVIDIA).unwrap();
        }
        snapshot.insert_absent(OPENROUTER).unwrap();
        snapshot
    }

    #[test]
    fn snapshot_round_trips_and_replaces_the_whole_set() {
        let frame = encode_snapshot_frame(7, snapshot(Some("test-groq-key"), false)).unwrap();
        let SnapshotDelivery { id, snapshot } = decode_snapshot_frame(&frame).unwrap();
        assert_eq!(id, 7);
        let mut current = SecretBundle::new();
        current
            .insert(OPENROUTER, Secret::from_string("old-openrouter-key"))
            .unwrap();
        assert!(snapshot.replace_into(&mut current).is_empty());
        assert_eq!(
            current.get(GROQ).unwrap().expose_str().unwrap(),
            "test-groq-key"
        );
        assert!(current.get(OPENROUTER).is_none(), "an Absent name is wiped");
    }

    #[test]
    fn a_failed_read_wipes_the_old_value_and_is_reported() {
        let frame = encode_snapshot_frame(1, snapshot(None, true)).unwrap();
        let delivery = decode_snapshot_frame(&frame).unwrap();
        assert!(matches!(
            delivery.snapshot.get(NVIDIA),
            Some(Entry::ReadFailed(_))
        ));
        let mut current = SecretBundle::new();
        current
            .insert(NVIDIA, Secret::from_string("old-nvidia-key"))
            .unwrap();
        let failed = delivery.snapshot.replace_into(&mut current);
        assert_eq!(failed.len(), 1);
        assert!(current.is_empty());
    }

    #[test]
    fn slots_become_present_absent_and_failed_entries() {
        let frame = encode_slots_frame(
            3,
            KeySlot::Present(SecretString::new("test-groq-key".into())),
            KeySlot::ReadFailed,
            KeySlot::Absent,
        )
        .unwrap();
        let delivery = decode_snapshot_frame(&frame).unwrap();
        assert!(matches!(
            delivery.snapshot.get(GROQ),
            Some(Entry::Present(_))
        ));
        assert!(matches!(
            delivery.snapshot.get(NVIDIA),
            Some(Entry::ReadFailed(_))
        ));
        assert!(matches!(
            delivery.snapshot.get(OPENROUTER),
            Some(Entry::Absent)
        ));
    }

    #[test]
    fn the_carrier_never_shows_the_key_in_debug() {
        let frame = encode_snapshot_frame(1, snapshot(Some("test-groq-key"), false)).unwrap();
        assert_eq!(format!("{frame:?}"), "<redacted>");
        let delivery = decode_snapshot_frame(&frame).unwrap();
        assert!(!format!("{delivery:?}").contains("test-groq-key"));
    }

    #[test]
    fn ack_must_match_the_delivery_id() {
        let ack = encode_ack(9).unwrap();
        assert!(check_ack(&ack, 9).is_ok());
        assert!(matches!(
            check_ack(&ack, 10),
            Err(HandoffError::IdMismatch {
                expected: 10,
                found: 9
            })
        ));
        assert!(check_ack("zz", 9).is_err());
    }

    #[test]
    fn malformed_or_trailing_carriers_are_refused() {
        let frame = encode_snapshot_frame(1, snapshot(None, false)).unwrap();
        let padded = SecretString::new(format!("{}00", frame.expose()));
        assert!(matches!(
            decode_snapshot_frame(&padded),
            Err(HandoffError::TrailingBytes)
        ));
        assert!(decode_snapshot_frame(&SecretString::new("abc".into())).is_err());
        assert!(decode_snapshot_frame(&SecretString::new(String::new())).is_err());
    }

    #[test]
    fn scrub_removes_every_ambient_key_variable() {
        let mut command = Command::new("true");
        scrub_sidecar_env(&mut command);
        for name in AMBIENT_KEY_ENV {
            assert!(
                command.get_envs().any(|(n, v)| n == name && v.is_none()),
                "{name}"
            );
        }
    }
}
