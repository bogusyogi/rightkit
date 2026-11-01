//! IPC -> key store round trip: a wire `set_provider_keys` frame carrying the owner's hand-off
//! snapshot (rightkit#886), validated exactly as the sidecar validates it, populates the store
//! the provider roster reads from; a snapshot with no keys, or with a failed read, wipes it.
//!
//! The tests share the process-global store, so they serialize on a lock.

use rightkit_credentials::handoff::{ErrorCode, Snapshot};
use rightkit_credentials::Secret;
use rightkit_dictation_core::engine::provider_handoff::{
    decode_snapshot_frame, encode_snapshot_frame, GROQ, NVIDIA, OPENROUTER,
};
use rightkit_dictation_core::engine::{
    validate_engine_frame, EngineFrame, EnginePayload, EngineSchemaName,
};
use rightkit_dictation_text_pipeline::provider_keys::{self, ProviderKey};
use std::sync::Mutex;

static STORE_LOCK: Mutex<()> = Mutex::new(());

fn wire_frame_of(snapshot: Snapshot) -> serde_json::Value {
    let frame = EngineFrame::base(
        EngineSchemaName::SetProviderKeys,
        "set_provider_keys:1",
        None,
        "trace",
        Some(EnginePayload::SetProviderKeys {
            frame: encode_snapshot_frame(1, snapshot).unwrap(),
        }),
        None,
    );
    // Through the real wire text, as the sidecar's read loop sees it.
    serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap()
}

fn wire_frame(
    groq: Option<&str>,
    nvidia: Option<&str>,
    openrouter: Option<&str>,
) -> serde_json::Value {
    wire_frame_of(provider_keys::snapshot_of(groq, nvidia, openrouter))
}

fn apply(wire: &serde_json::Value) -> Vec<(String, ErrorCode)> {
    let frame = validate_engine_frame(wire).expect("valid frame");
    match frame.payload {
        Some(EnginePayload::SetProviderKeys { frame }) => {
            let delivery = decode_snapshot_frame(&frame).expect("owner frame");
            provider_keys::apply(delivery.snapshot)
        }
        other => panic!("unexpected payload {other:?}"),
    }
}

#[test]
fn set_provider_keys_populates_then_none_clears() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    apply(&wire_frame(
        Some("test-groq-key"),
        Some("test-nvidia-key"),
        Some("test-openrouter-key"),
    ));
    assert_eq!(
        provider_keys::get(ProviderKey::Groq).as_deref(),
        Some("test-groq-key")
    );
    assert_eq!(
        provider_keys::get(ProviderKey::Nvidia).as_deref(),
        Some("test-nvidia-key")
    );
    assert_eq!(
        provider_keys::get(ProviderKey::OpenRouter).as_deref(),
        Some("test-openrouter-key")
    );

    // Full replacement: omitted providers are cleared too.
    apply(&wire_frame(Some("test-groq-key-2"), None, None));
    assert_eq!(
        provider_keys::get(ProviderKey::Groq).as_deref(),
        Some("test-groq-key-2")
    );
    assert_eq!(provider_keys::get(ProviderKey::Nvidia), None);
    assert_eq!(provider_keys::get(ProviderKey::OpenRouter), None);

    apply(&wire_frame(None, None, None));
    assert_eq!(provider_keys::get(ProviderKey::Groq), None);
}

#[test]
fn a_removed_key_is_wiped_in_the_running_engine() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    apply(&wire_frame(
        Some("test-groq-key"),
        Some("test-nvidia-key"),
        None,
    ));
    assert!(provider_keys::get(ProviderKey::Nvidia).is_some());
    // The user removes the NVIDIA key: the next snapshot reports it Absent.
    apply(&wire_frame(Some("test-groq-key"), None, None));
    assert_eq!(provider_keys::get(ProviderKey::Nvidia), None);
    assert!(provider_keys::get(ProviderKey::Groq).is_some());
    provider_keys::clear();
}

#[test]
fn a_failed_owner_read_wipes_the_old_key_and_is_reported() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    apply(&wire_frame(Some("test-groq-key"), None, None));
    let mut snapshot = Snapshot::new();
    snapshot.insert_failed(GROQ, ErrorCode::StoreError).unwrap();
    snapshot
        .insert_present(NVIDIA, Secret::from_string("test-nvidia-key"))
        .unwrap();
    snapshot.insert_absent(OPENROUTER).unwrap();
    let failed = apply(&wire_frame_of(snapshot));
    assert_eq!(failed, vec![(GROQ.to_string(), ErrorCode::StoreError)]);
    assert_eq!(provider_keys::get(ProviderKey::Groq), None, "fail closed");
    assert!(provider_keys::get(ProviderKey::Nvidia).is_some());
    provider_keys::clear();
}

#[test]
fn blank_keys_are_treated_as_absent() {
    let _guard = STORE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    apply(&wire_frame(Some("   "), None, None));
    assert_eq!(provider_keys::get(ProviderKey::Groq), None);
    provider_keys::clear();
}
