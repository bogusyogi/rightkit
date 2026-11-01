//! In-memory store for the user's BYOK LLM provider keys.
//!
//! The shell hands the keys over as the owner's `rightkit-credentials` hand-off snapshot
//! (rightkit#886) on the private stdin IPC channel (`SetProviderKeys`) instead of the sidecar's
//! process environment, so they never appear in the environment block. The store is the
//! owner's `SecretBundle` (values zeroized on drop) and is only ever changed through
//! `Snapshot::replace_into`: a full replacement in which every name that is Absent, ReadFailed
//! or not mentioned is wiped. Release builds read keys ONLY from here; debug builds
//! additionally fall back to the legacy environment variables (see `l3_cleanup::providers`).

use rightkit_credentials::handoff::{
    ErrorCode, HandoffError, SecretBundle, Snapshot, SnapshotDelivery,
};
use rightkit_credentials::Secret;
use rightkit_dictation_core::engine::provider_handoff::{
    decode_snapshot_frame, encode_ack, GROQ, NVIDIA, OPENROUTER,
};
use rightkit_dictation_core::engine::SecretString;
use std::sync::RwLock;

static STORE: RwLock<Option<SecretBundle>> = RwLock::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKey {
    Groq,
    Nvidia,
    OpenRouter,
}

impl ProviderKey {
    fn name(self) -> &'static str {
        match self {
            Self::Groq => GROQ,
            Self::Nvidia => NVIDIA,
            Self::OpenRouter => OPENROUTER,
        }
    }
}

/// Apply a delivered snapshot as a full replacement. The previous secrets are dropped (and
/// wiped). Returns the names whose read failed on the owner's side (their old values are wiped
/// too, fail closed) so the caller can report them; never a value.
pub fn apply(snapshot: Snapshot) -> Vec<(String, ErrorCode)> {
    let mut guard = STORE.write().unwrap_or_else(|p| p.into_inner());
    let bundle = guard.get_or_insert_with(SecretBundle::new);
    snapshot.replace_into(bundle)
}

/// Sidecar side of one `SetProviderKeys` delivery: decode the owner's frame, replace the whole
/// set, and return the owner's Ack (hex) to send back with the names whose read failed. A frame
/// that does not decode leaves the store untouched and returns a typed error without any value.
pub fn apply_frame(
    frame: &SecretString,
) -> Result<(String, Vec<(String, ErrorCode)>), HandoffError> {
    let SnapshotDelivery { id, snapshot } = decode_snapshot_frame(frame)?;
    let failed = apply(snapshot);
    Ok((encode_ack(id)?, failed))
}

/// A snapshot of optional keys, one entry per provider slot (Present or Absent).
pub fn snapshot_of(groq: Option<&str>, nvidia: Option<&str>, openrouter: Option<&str>) -> Snapshot {
    let mut snapshot = Snapshot::new();
    for (name, key) in [(GROQ, groq), (NVIDIA, nvidia), (OPENROUTER, openrouter)] {
        let outcome = match key {
            Some(key) => snapshot.insert_present(name, Secret::from_string(key)),
            None => snapshot.insert_absent(name),
        };
        outcome.expect("provider slot names are valid and unique");
    }
    snapshot
}

/// Drop and wipe every stored key.
pub fn clear() {
    apply(snapshot_of(None, None, None));
}

/// The trimmed, non-blank key for a provider, if one was delivered.
pub fn get(which: ProviderKey) -> Option<String> {
    let guard = STORE.read().unwrap_or_else(|p| p.into_inner());
    let secret = guard.as_ref()?.get(which.name())?;
    let key = secret.expose_str().ok()?.trim();
    (!key.is_empty()).then(|| key.to_string())
}
