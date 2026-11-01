#![cfg(feature = "devicelink")]

use rightkit_device_link_types::{
    CheckKey, DevicePlatform, EndorsementBatch, OwnerSig, PairedDevice, Redemption,
};
use rightkit_mesh::devicelink::circle::{EndorsementBatchExt, RedemptionExt, circle_pairing_id};
use rightkit_mesh::devicelink::policy::authorize;
use rightkit_mesh::devicelink::{DeviceIdentity, LinkError, RegistryStore, device_id_for_key};

fn temp_registry(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-mesh-devicelink-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("registry.json")
}

fn record(identity: &DeviceIdentity) -> PairedDevice {
    serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "pairing_id": "00112233445566778899aabbccddeeff",
        "device": identity.device(),
        "public_key": identity.public_key_hex(),
        "paired_at_ms": 1,
        "revision": 1,
        "state": { "state": "active" },
        "grants": [],
        "grants_expire_at_ms": null,
        "manage_peers": false
    }))
    .expect("record")
}

#[test]
fn identity_is_derived_from_the_seed() {
    let a = DeviceIdentity::from_seed(&[1u8; 32], DevicePlatform::Linux, None).expect("a");
    let again = DeviceIdentity::from_seed(&[1u8; 32], DevicePlatform::Linux, None).expect("a2");
    let b =
        DeviceIdentity::from_seed(&[2u8; 32], DevicePlatform::Mac, Some("b".into())).expect("b");
    assert_eq!(a.device_id(), again.device_id());
    assert_ne!(a.device_id(), b.device_id());
    assert!(a.device_id().starts_with("dev_") && a.device_id().len() == 36);
    assert_eq!(a.device_id(), device_id_for_key(&a.public_key()));
}

#[test]
fn the_registry_file_keeps_records_and_bumps_its_revision() {
    let path = temp_registry("store");
    let store = RegistryStore::new(path.clone());
    assert!(
        store
            .load()
            .expect("missing file is empty")
            .devices
            .is_empty()
    );
    let id = DeviceIdentity::from_seed(&[3u8; 32], DevicePlatform::Ios, None).expect("id");
    let rec = record(&id);
    store
        .update(|registry| {
            registry.devices.push(rec.clone());
            Ok(())
        })
        .expect("update");
    let refused: Result<(), LinkError> = store.update(|registry| {
        registry.devices.clear();
        Err(LinkError::UnknownDevice)
    });
    assert!(refused.is_err());
    let loaded = store.load().expect("load");
    assert_eq!(loaded.revision, 1, "a refused change writes nothing");
    assert!(loaded.get(id.device_id()).is_some());
    let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
}

#[test]
fn circle_pairing_ids_are_symmetric() {
    let ab = circle_pairing_id("dev_a", "dev_b");
    assert_eq!(ab, circle_pairing_id("dev_b", "dev_a"));
    assert_eq!(ab.len(), 32);
    assert_ne!(ab, circle_pairing_id("dev_a", "dev_c"));
}

#[test]
fn an_empty_endorsement_batch_is_malformed() {
    let batch = EndorsementBatch {
        bodies: Vec::new(),
        grants: Vec::new(),
        sig: OwnerSig {
            check: CheckKey {
                device: "mac-owner-check".into(),
                public_key: "00".into(),
                binding: None,
            },
            challenge: String::new(),
            client: String::new(),
            key_id: String::new(),
            nonce: String::new(),
            issued: 0,
            expires: 0,
            signature: String::new(),
        },
    };
    assert!(matches!(batch.verify(10), Err(LinkError::Malformed(_))));
}

#[test]
fn a_redemption_signs_a_fixed_layout() {
    let mut redemption = Redemption {
        pairing_id: "p".into(),
        joiner_device_id: "j".into(),
        joiner_public_key: "k".into(),
        redeemed_at_ms: 5,
        member_class: "member".into(),
        lease_expires_at_ms: 0,
        signature: String::new(),
    };
    let first = redemption.signing_input();
    redemption.redeemed_at_ms = 6;
    assert_ne!(first, redemption.signing_input());
    assert!(!redemption.is_guest());
}

#[test]
fn chat_policy_needs_an_owner_proof_or_a_reversible_grant() {
    let id = DeviceIdentity::from_seed(&[4u8; 32], DevicePlatform::Linux, None).expect("id");
    let mut rec = record(&id);
    assert!(authorize(&rec, 10).is_err(), "pairing alone grants nothing");
    rec.grants = vec![rightkit_device_link_types::EffectClass::Reversible];
    assert_eq!(authorize(&rec, 10), Ok("grant"));
    rec.grants_expire_at_ms = Some(5);
    assert!(authorize(&rec, 10).is_err(), "an expired grant is refused");
    rec.admit = true;
    assert_eq!(authorize(&rec, 10), Ok("owner"));
}
