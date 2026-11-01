use rightkit_device_link_types::{
    DevicePlatform, DeviceRef, EffectClass, GuestPass, MemberRole, PairedDevice, PairedState,
    Registry, TokenGrant, GUEST_IDLE_MS,
};

fn device(id: &str) -> DeviceRef {
    DeviceRef {
        schema_version: 1,
        device_id: id.to_string(),
        platform: DevicePlatform::Linux,
        display_name: Some("box".to_string()),
    }
}

fn paired(id: &str) -> PairedDevice {
    serde_json::from_value(serde_json::json!({
        "schema_version": 2,
        "pairing_id": "p1",
        "device": device(id),
        "public_key": "00",
        "paired_at_ms": 1,
        "revision": 1,
        "state": { "state": "active" },
        "grants": ["reversible"],
        "grants_expire_at_ms": null,
        "manage_peers": false
    }))
    .expect("a version 1 style record parses with the newer fields defaulted")
}

#[test]
fn old_records_default_the_newer_fields() {
    let record = paired("dev_a");
    assert!(record.is_active());
    assert_eq!(record.member_role(), MemberRole::Unclassified);
    assert!(record.peer_tls_pin.is_none() && record.guest.is_none() && !record.admit);
    assert_eq!(record.grants, vec![EffectClass::Reversible]);
}

#[test]
fn registry_round_trips_and_finds_devices() {
    let mut registry = Registry::default();
    registry.devices.push(paired("dev_a"));
    let text = serde_json::to_string(&registry).expect("serialize");
    let back: Registry = serde_json::from_str(&text).expect("parse");
    assert_eq!(back, registry);
    assert!(back.get("dev_a").is_some() && back.get("dev_b").is_none());
}

#[test]
fn revoked_is_not_active() {
    let mut record = paired("dev_a");
    record.state = PairedState::Revoked {
        at_ms: 5,
        by: "local".to_string(),
    };
    assert!(!record.is_active() && !record.is_unrevoked());
}

#[test]
fn guest_pass_needs_approval_and_a_live_lease() {
    let mut pass = GuestPass {
        minted_by: "m".to_string(),
        approved_ms: None,
        lease_expires_ms: 1_000_000,
        idle_ms: GUEST_IDLE_MS,
        last_seen_ms: None,
        check_code: GuestPass::check_code("pairing", "key"),
        reported_session: None,
    };
    assert!(!pass.admits(10), "unapproved is not admitted");
    pass.approved_ms = Some(5);
    assert!(pass.admits(10));
    assert!(!pass.admits(1_000_000), "lease end is exclusive");
    assert_eq!(pass.check_code, GuestPass::check_code("pairing", "key"));
}

#[test]
fn token_grant_line_is_stable() {
    let grant = TokenGrant::new("dev_a", "p", &[7u8; 32], 100);
    assert!(!grant.can_admit && grant.expires_at_ms > grant.issued_at_ms);
    assert!(grant
        .line()
        .starts_with("pulse-circle-grant-v2\nadmitter:dev_a\n"));
}
