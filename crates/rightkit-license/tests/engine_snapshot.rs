#![cfg(feature = "client-testing")]
//! The owner-to-engine entitlement snapshot: the owner's client builds it from the stored record,
//! it crosses a real handoff frame, and an engine that holds no credential store and has no way to
//! be told "you are Pro" verifies it again (signature, app, device, expiry, denylist, ledger).

mod client_support;

use std::sync::atomic::Ordering::SeqCst;

use client_support::*;
use rightkit_credentials::handoff::{
    expect_reply, read_message, write_message, Entry, ErrorCode, HandoffError, Limits, Message,
    Mode, Snapshot,
};
use rightkit_credentials::Secret;
use rightkit_license::{
    ClockRefusal, ClockState, ClockStore, EngineEntitlementError, EngineEntitlements,
    EngineEntitlementsConfig, EngineSnapshotCell, FileClockStore, OwnerLedgerReader, RevokedIds,
    SecretString, SnapshotError, CLOCK_FRESH, CLOCK_INITIALIZED, SNAPSHOT_CLOCK, SNAPSHOT_DEVICE,
    SNAPSHOT_NAMES, SNAPSHOT_TOKEN,
};

fn key() -> SecretString {
    SecretString::new(KEY)
}

/// The engine is "another process" to the owner that wrote the ledger, so it must see wall time strictly past
/// the owner's mark (a restart in the same second must not freeze expiry). Wait for that second to pass.
fn settle(f: &Fixture) {
    let ledger = FileClockStore::new(f.clock_path());
    for _ in 0..60 {
        let mark = ledger.load().unwrap().map_or(0, |state| state.highest_seen);
        if now() > mark {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the wall clock never passed the owner's ledger mark");
}

/// An engine over `cell` and the shared licence directory, with the test signer's key and no verdict cache
/// (so every check reads the cell again).
fn engine_with(
    f: &Fixture,
    cell: &EngineSnapshotCell,
    configure: impl FnOnce(EngineEntitlementsConfig) -> EngineEntitlementsConfig,
) -> EngineEntitlements<rightkit_license::AntiRollbackClock<OwnerLedgerReader>> {
    let config = configure(EngineEntitlementsConfig::from_snapshot(
        common::pubkey(),
        APP,
        cell,
    ))
    .with_cache_ttl_secs(0);
    EngineEntitlements::from_snapshot(config, f.license_dir()).unwrap()
}

fn engine(
    f: &Fixture,
    cell: &EngineSnapshotCell,
) -> EngineEntitlements<rightkit_license::AntiRollbackClock<OwnerLedgerReader>> {
    engine_with(f, cell, |config| config)
}

fn present(snapshot: &Snapshot, name: &str) -> String {
    match snapshot.get(name) {
        Some(Entry::Present(secret)) => secret.expose_str().unwrap().to_owned(),
        other => panic!("{name} should be present, found {other:?}"),
    }
}

fn snapshot_with(token: Option<&str>, device: Option<&str>, clock: Option<&str>) -> Snapshot {
    let mut snapshot = Snapshot::new();
    for (name, value) in [
        (SNAPSHOT_TOKEN, token),
        (SNAPSHOT_DEVICE, device),
        (SNAPSHOT_CLOCK, clock),
    ] {
        match value {
            Some(text) => snapshot
                .insert_present(name, Secret::from_string(text))
                .unwrap(),
            None => snapshot.insert_absent(name).unwrap(),
        }
    }
    snapshot
}

fn unreadable(error: EngineEntitlementError) -> String {
    match error {
        EngineEntitlementError::Unreadable(text) => text,
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[test]
fn the_snapshot_has_exactly_the_three_names_and_the_owner_binding() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let snapshot = client.engine_snapshot();
    assert_eq!(snapshot.len(), 3);
    assert_eq!(
        snapshot.names().collect::<Vec<_>>(),
        {
            let mut names = SNAPSHOT_NAMES.to_vec();
            names.sort_unstable();
            names
        },
        "the names are valid handoff names, distinct, and all three are in the snapshot"
    );
    assert!(
        present(&snapshot, SNAPSHOT_TOKEN).contains('.'),
        "a signed token"
    );
    assert_eq!(present(&snapshot, SNAPSHOT_DEVICE), DEVICE);
    assert!([CLOCK_INITIALIZED, CLOCK_FRESH].contains(&present(&snapshot, SNAPSHOT_CLOCK).as_str()));
}

#[test]
fn engine_snapshot_roundtrip() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);

    // Over a real handoff frame, as the owner writes it to the engine's stdin.
    let mut wire = Vec::new();
    write_message(
        &mut wire,
        &Message::reply_snapshot(9, client.engine_snapshot()),
    )
    .unwrap();
    let received = read_message(&mut wire.as_slice(), &Limits::default()).unwrap();
    let snapshot = expect_reply(received, 9, Mode::Snapshot, &SNAPSHOT_NAMES).unwrap();

    // The engine side has a cell and a directory, no Store: remove the owner's item to prove it.
    let cell = EngineSnapshotCell::new();
    cell.apply(snapshot).unwrap();
    f.backend.remove(ACCOUNT);
    let verified = engine(&f, &cell).require_pro().unwrap();
    assert_eq!(verified.id, LICENSE_ID);
    assert_eq!(verified.app, APP);
}

#[test]
fn a_lease_token_verifies_under_the_device_hash_the_owner_sends() {
    let f = fixture();
    let client = f.client();
    client.lease_activate(key(), Some("Work Mac")).unwrap();
    settle(&f);
    let snapshot = client.engine_snapshot();
    assert_eq!(
        present(&snapshot, SNAPSHOT_DEVICE),
        DEVICE_HASH,
        "a lease binds the hash"
    );
    let cell = EngineSnapshotCell::new();
    cell.apply(snapshot).unwrap();
    assert_eq!(engine(&f, &cell).require_pro().unwrap().id, LICENSE_ID);
}

#[test]
fn the_engine_refuses_before_any_delivery() {
    let f = fixture();
    let cell = EngineSnapshotCell::new();
    assert!(unreadable(engine(&f, &cell).require_pro().unwrap_err()).contains("not delivered"));
}

#[test]
fn a_deactivation_reaches_the_engine_with_the_next_delivery_even_through_the_verdict_cache() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    // The default cache TTL: a verdict is cached after the first check.
    let config = EngineEntitlementsConfig::from_snapshot(common::pubkey(), APP, &cell);
    let cached = EngineEntitlements::from_snapshot(config, f.license_dir()).unwrap();
    cached.require_pro().unwrap();
    cached.require_pro().unwrap();

    client.deactivate().unwrap();
    cell.apply(client.engine_snapshot()).unwrap();
    assert_eq!(
        cached.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
}

#[test]
fn an_owner_that_cannot_read_the_licence_makes_the_engine_refuse_it_never_free_and_never_stale() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    assert!(engine(&f, &cell).require_pro().is_ok());

    f.backend.deny_get.store(true, SeqCst);
    let failed = client.engine_snapshot();
    assert!(matches!(
        failed.get(SNAPSHOT_TOKEN),
        Some(Entry::ReadFailed(_))
    ));
    assert_eq!(cell.apply(failed), Err(SnapshotError::OwnerReadFailed));
    let text = unreadable(engine(&f, &cell).require_pro().unwrap_err());
    assert!(text.contains("could not read"), "{text}");
}

#[test]
fn absent_revoked_and_unsigned_records_are_a_free_engine() {
    let f = fixture();
    let client = f.client();
    // Nothing stored.
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    assert_eq!(
        engine(&f, &cell).require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
    // A client with licensing disabled has no licence product: the same.
    let disabled = rightkit_license::LicenseClient::new(
        rightkit_license::LicenseSetup::Disabled,
        std::sync::Arc::new(FixedDevice),
    )
    .unwrap();
    assert!(matches!(
        disabled.engine_snapshot().get(SNAPSHOT_TOKEN),
        Some(Entry::Absent)
    ));
}

#[test]
fn a_malformed_snapshot_keeps_nothing_and_says_so() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let token = present(&client.engine_snapshot(), SNAPSHOT_TOKEN);
    let cell = EngineSnapshotCell::new();
    // A token with no device binding, an empty one, whitespace only, and a token that is not text.
    for bad in [
        snapshot_with(Some(&token), None, Some(CLOCK_FRESH)),
        snapshot_with(Some(&token), Some("  "), Some(CLOCK_FRESH)),
        snapshot_with(Some("   "), Some(DEVICE), Some(CLOCK_FRESH)),
    ] {
        assert_eq!(cell.apply(bad), Err(SnapshotError::Malformed));
        assert!(unreadable(engine(&f, &cell).require_pro().unwrap_err()).contains("malformed"));
    }
    let mut binary = Snapshot::new();
    binary
        .insert_present(SNAPSHOT_TOKEN, Secret::from_bytes(vec![0xff, 0xfe]))
        .unwrap();
    binary
        .insert_present(SNAPSHOT_DEVICE, Secret::from_string(DEVICE))
        .unwrap();
    assert_eq!(cell.apply(binary), Err(SnapshotError::Malformed));
    // An oversized token is refused unread.
    let huge = "a".repeat(rightkit_license::MAX_TOKEN_BYTES as usize + 1);
    assert_eq!(
        cell.apply(snapshot_with(Some(&huge), Some(DEVICE), None)),
        Err(SnapshotError::Malformed)
    );
}

#[test]
fn a_token_for_another_device_or_app_or_key_does_not_verify() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let token = present(&client.engine_snapshot(), SNAPSHOT_TOKEN);
    let cell = EngineSnapshotCell::new();
    // The owner's word about the device is checked against the token's own binding.
    cell.apply(snapshot_with(
        Some(&token),
        Some("some-other-device"),
        Some(CLOCK_FRESH),
    ))
    .unwrap();
    assert!(matches!(
        engine(&f, &cell).require_pro().unwrap_err(),
        EngineEntitlementError::Invalid(_)
    ));
    cell.apply(snapshot_with(Some(&token), Some(DEVICE), Some(CLOCK_FRESH)))
        .unwrap();
    assert!(engine(&f, &cell).require_pro().is_ok());
    // Another app's key, and another app name.
    let wrong_key = EngineEntitlements::from_snapshot(
        EngineEntitlementsConfig::from_snapshot(common::other_pubkey(), APP, &cell),
        f.license_dir(),
    )
    .unwrap();
    assert!(matches!(
        wrong_key.require_pro().unwrap_err(),
        EngineEntitlementError::Invalid(_)
    ));
    let wrong_app = EngineEntitlements::from_snapshot(
        EngineEntitlementsConfig::from_snapshot(common::pubkey(), "otherapp", &cell),
        f.license_dir(),
    )
    .unwrap();
    assert!(matches!(
        wrong_app.require_pro().unwrap_err(),
        EngineEntitlementError::Invalid(_)
    ));
    // The suite key is not the test signer's key, so the suite configuration refuses this token.
    let suite = EngineEntitlements::suite_snapshot(APP, &cell, f.license_dir()).unwrap();
    assert!(matches!(
        suite.require_pro().unwrap_err(),
        EngineEntitlementError::Invalid(_)
    ));
}

#[test]
fn snapshot_refuses_rolled_back_clock() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    assert!(engine(&f, &cell).require_pro().is_ok());

    // Another process saw a time far in the future: the wall clock is now behind the ledger.
    FileClockStore::new(f.clock_path())
        .save(&ClockState {
            highest_seen: now() + 10_000,
            trusted_process: "owner-process".into(),
            last_server_iat: 0,
        })
        .unwrap();
    match engine(&f, &cell).require_pro().unwrap_err() {
        EngineEntitlementError::Clock(ClockRefusal::Rollback { highest_seen }) => {
            assert!(highest_seen > now())
        }
        other => panic!("expected a rollback refusal, got {other:?}"),
    }
}

#[test]
fn a_ledger_the_owner_initialised_cannot_be_deleted_to_reset_rollback_protection() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let token = present(&client.engine_snapshot(), SNAPSHOT_TOKEN);
    let cell = EngineSnapshotCell::new();
    // The owner says the ledger was initialised, and there is none.
    let _ = std::fs::remove_file(f.clock_path());
    cell.apply(snapshot_with(
        Some(&token),
        Some(DEVICE),
        Some(CLOCK_INITIALIZED),
    ))
    .unwrap();
    assert_eq!(
        engine(&f, &cell).require_pro().unwrap_err(),
        EngineEntitlementError::Clock(ClockRefusal::Missing)
    );
    // A fresh owner has no ledger yet; the engine starts it.
    cell.apply(snapshot_with(Some(&token), Some(DEVICE), Some(CLOCK_FRESH)))
        .unwrap();
    assert!(engine(&f, &cell).require_pro().is_ok());
    // A flag that is missing or unknown is read as initialised, the stricter reading.
    let _ = std::fs::remove_file(f.clock_path());
    cell.apply(snapshot_with(Some(&token), Some(DEVICE), None))
        .unwrap();
    assert!(matches!(
        engine(&f, &cell).require_pro().unwrap_err(),
        EngineEntitlementError::Clock(ClockRefusal::Missing)
    ));
}

#[test]
fn snapshot_honours_revoked_ids() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    assert!(engine(&f, &cell).require_pro().is_ok());

    let revoked = engine_with(&f, &cell, |config| {
        config.with_revoked_ids(RevokedIds::new().with(LICENSE_ID))
    });
    assert_eq!(
        revoked.require_pro().unwrap_err(),
        EngineEntitlementError::Revoked(LICENSE_ID.to_owned())
    );
    // An id that is not this licence changes nothing.
    let other = engine_with(&f, &cell, |config| {
        config.with_revoked_ids(RevokedIds::new().with("lic_someone_else"))
    });
    assert_eq!(other.require_pro().unwrap().id, LICENSE_ID);
}

#[test]
fn the_owner_answers_only_the_three_names_in_snapshot_mode() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);

    let ask = |id, mode, names: &[&str]| {
        client.answer_engine_request(&Message::request(id, mode, names).unwrap())
    };

    // Any order of exactly the three names is answered, with the request's id.
    let reply = ask(
        7,
        Mode::Snapshot,
        &[SNAPSHOT_CLOCK, SNAPSHOT_TOKEN, SNAPSHOT_DEVICE],
    );
    let snapshot = expect_reply(reply, 7, Mode::Snapshot, &SNAPSHOT_NAMES).unwrap();
    assert_eq!(present(&snapshot, SNAPSHOT_DEVICE), DEVICE);

    let code = |message: Message| match message {
        Message::Error { id, code, name } => (id, code, name),
        other => panic!("expected an error, found {other:?}"),
    };
    // The licence record's own account name, or any other name, is never released.
    assert_eq!(
        code(ask(
            8,
            Mode::Snapshot,
            &[SNAPSHOT_TOKEN, SNAPSHOT_DEVICE, ACCOUNT]
        )),
        (8, ErrorCode::NotAllowed, Some(ACCOUNT.to_owned()))
    );
    // Strict mode cannot carry Absent or failed entries, a subset or a repeat is not the snapshot.
    assert_eq!(
        code(ask(9, Mode::Strict, &SNAPSHOT_NAMES)).1,
        ErrorCode::Invalid
    );
    assert_eq!(
        code(ask(10, Mode::Snapshot, &[SNAPSHOT_TOKEN])).1,
        ErrorCode::Invalid
    );
    assert_eq!(
        code(ask(11, Mode::Snapshot, &[SNAPSHOT_TOKEN, SNAPSHOT_DEVICE])).1,
        ErrorCode::Invalid
    );
    // A message that is not a request.
    assert_eq!(
        code(client.answer_engine_request(&Message::ack(12))),
        (12, ErrorCode::Invalid, None)
    );
    // The engine's side refuses a reply that answers another request.
    let stale = ask(13, Mode::Snapshot, &SNAPSHOT_NAMES);
    assert!(matches!(
        expect_reply(stale, 14, Mode::Snapshot, &SNAPSHOT_NAMES),
        Err(HandoffError::IdMismatch { .. })
    ));
}

#[test]
fn nothing_the_snapshot_carries_prints() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let snapshot = client.engine_snapshot();
    let token = present(&snapshot, SNAPSHOT_TOKEN);
    let cell = EngineSnapshotCell::new();
    for printed in [format!("{snapshot:?}"), format!("{cell:?}")] {
        assert!(
            !printed.contains(&token) && !printed.contains(DEVICE),
            "{printed}"
        );
    }
    cell.apply(snapshot).unwrap();
    let printed = format!("{cell:?}");
    assert!(
        !printed.contains(&token) && !printed.contains(DEVICE),
        "{printed}"
    );
}

#[test]
fn the_engine_only_reads_the_owners_ledger() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    settle(&f);
    let cell = EngineSnapshotCell::new();
    cell.apply(client.engine_snapshot()).unwrap();
    let before = std::fs::read(f.clock_path()).unwrap();
    let engine = engine(&f, &cell);
    for _ in 0..3 {
        engine.require_pro().unwrap();
    }
    assert_eq!(
        std::fs::read(f.clock_path()).unwrap(),
        before,
        "the owner is the ledger's one writer"
    );
    // With no ledger at all (a fresh owner) the engine creates none either.
    std::fs::remove_file(f.clock_path()).unwrap();
    let token = present(&client.engine_snapshot(), SNAPSHOT_TOKEN);
    cell.apply(snapshot_with(Some(&token), Some(DEVICE), Some(CLOCK_FRESH)))
        .unwrap();
    engine.require_pro().unwrap();
    assert!(!f.clock_path().exists());
}
