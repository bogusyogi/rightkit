#![cfg(feature = "client-testing")]
mod client_support;
use client_support::*;
use rightkit_license::{
    ClockState, ClockStore, NativeCredential, NativeEntitlementSource, NativeSourceError,
    SecretString,
};
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

fn signed(source: &impl NativeEntitlementSource) -> rightkit_license::NativeSignedCredential {
    match source.read().unwrap() {
        NativeCredential::Signed(value) => value,
        _ => panic!("expected signed native record"),
    }
}

#[test]
fn native_source_uses_native_device_and_rejects_stale_marker_after_deactivation() {
    let f = fixture();
    let client = f.client();
    let source = client.native_entitlement_source().unwrap();
    assert!(matches!(source.read().unwrap(), NativeCredential::Absent));
    client.activate(SecretString::new(KEY)).unwrap();
    let native = signed(&source);
    assert_eq!(native.device_binding, DEVICE);
    client.deactivate().unwrap();
    assert_eq!(
        source.mark_clock_initialized(&native),
        Err(NativeSourceError::Refused)
    );
    assert!(matches!(source.read().unwrap(), NativeCredential::Absent));
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
}

#[test]
fn native_marker_compares_exact_revision_and_preserves_credentials() {
    let f = fixture();
    let client = f.client();
    client.activate(SecretString::new(KEY)).unwrap();
    let source = client.native_entitlement_source().unwrap();
    let old = signed(&source);
    let before = f.backend.envelope().unwrap();
    // Simulate another cooperating committed record with unchanged token.
    let mut replacement = before.clone();
    replacement["revision"] = serde_json::json!("different-revision");
    f.backend.put_text(ACCOUNT, &replacement.to_string());
    assert_eq!(
        source.mark_clock_initialized(&old),
        Err(NativeSourceError::Refused)
    );
    let current = signed(&source);
    source.mark_clock_initialized(&current).unwrap();
    let after = f.backend.envelope().unwrap();
    assert_eq!(
        after["record"]["signedToken"],
        before["record"]["signedToken"]
    );
    assert_eq!(
        after["record"]["activationToken"],
        before["record"]["activationToken"]
    );
}

#[test]
fn native_denial_or_pending_fence_is_refusal_never_absence() {
    let f = fixture();
    let client = f.client();
    let source = client.native_entitlement_source().unwrap();
    f.backend.deny_get.store(true, SeqCst);
    assert!(matches!(source.read(), Err(NativeSourceError::Refused)));
    f.backend.deny_get.store(false, SeqCst);
    f.leave_marker("unresolved");
    assert!(matches!(source.read(), Err(NativeSourceError::Refused)));
}

#[test]
fn credential_clock_preserves_scalar_codec_and_compares_exact_state() {
    let f = fixture();
    let client =
        f.client_with(|cfg| cfg.with_credential_clock(f.store.clone(), "highest-seen-unix"));
    let clock = client.native_clock_store().unwrap();
    f.backend.put_text("highest-seen-unix", "1234");
    let old = clock.load().unwrap().unwrap();
    assert_eq!(old.highest_seen, 1234);
    let new = ClockState {
        highest_seen: 1235,
        trusted_process: "native".into(),
        last_server_iat: 1000,
    };
    assert!(!clock.compare_and_save(None, &new).unwrap());
    assert!(clock.compare_and_save(Some(&old), &new).unwrap());
    assert_eq!(clock.load().unwrap(), Some(new));
    assert!(!f.clock_path().exists());
    assert!(f
        .backend
        .text("highest-seen-unix")
        .unwrap()
        .starts_with('{'));
}

#[test]
fn timed_out_clock_worker_keeps_guard_until_delayed_backend_finishes() {
    let f = fixture_with(Duration::from_millis(30));
    let client = f.client_with(|cfg| {
        cfg.with_credential_clock(f.store.clone(), "highest-seen-unix")
            .with_persist_wait(Duration::from_millis(20))
    });
    let clock = client.native_clock_store().unwrap();
    f.backend.primary_set_delay_ms.store(300, SeqCst);
    let new = ClockState {
        highest_seen: 100,
        trusted_process: "native".into(),
        last_server_iat: 90,
    };
    assert!(clock.compare_and_save(None, &new).is_err());
    // A second authorizer cannot read a half-written value; it waits for worker settlement.
    assert_eq!(clock.load().unwrap(), Some(new));
}

#[test]
fn legacy_import_binding_mode_is_lease_id_presence_even_without_optional_metadata() {
    let f = fixture();
    let client = f.client();
    let mut claims = common::Claims::term(now() - 100, now() + 3600);
    claims.device = DEVICE_HASH;
    let raw = serde_json::json!({ "status": "active", "signedToken": claims.token(), "leaseId": "legacy-lease" });
    client
        .import_legacy_json(SecretString::new(raw.to_string()))
        .unwrap();
    let native = signed(&client.native_entitlement_source().unwrap());
    assert_eq!(native.device_binding, DEVICE_HASH);
    let other = fixture();
    // Same hashed token without lease-id is forbidden by raw legacy binding mode.
    let raw = serde_json::json!({ "status": "active", "signedToken": claims.token() });
    assert!(other
        .client()
        .import_legacy_json(SecretString::new(raw.to_string()))
        .is_err());
}

// A mutable default keychain with native, per-target pinning.
struct SwitchingState {
    targets: [TestBackend; 2],
    selected: std::sync::atomic::AtomicUsize,
    switch_after_read: std::sync::atomic::AtomicBool,
    gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    entered: std::sync::mpsc::Sender<()>,
}
struct SwitchingStore {
    shared: std::sync::Arc<SwitchingState>,
    fixed: Option<usize>,
}
impl SwitchingStore {
    fn target(&self) -> usize {
        self.fixed
            .unwrap_or_else(|| self.shared.selected.load(SeqCst))
    }
}
impl rightkit_credentials::Backend for SwitchingStore {
    fn name(&self) -> &'static str {
        "native-switching-keychain"
    }
    fn durable(&self) -> bool {
        true
    }
    fn exclusion_identity(&self) -> Option<String> {
        self.fixed.map(|i| {
            format!(
                "native-switch:{:p}:{i}",
                std::sync::Arc::as_ptr(&self.shared)
            )
        })
    }
    fn pinned(
        &self,
    ) -> rightkit_credentials::Result<Option<std::sync::Arc<dyn rightkit_credentials::Backend>>>
    {
        Ok(if self.fixed.is_some() {
            None
        } else {
            Some(std::sync::Arc::new(Self {
                shared: self.shared.clone(),
                fixed: Some(self.target()),
            }))
        })
    }
    fn get(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<Option<rightkit_credentials::Secret>> {
        let result = rightkit_credentials::Backend::get(
            &self.shared.targets[self.target()],
            ns,
            account,
            mode,
        );
        if account == ACCOUNT && self.shared.switch_after_read.swap(false, SeqCst) {
            self.shared.selected.store(1, SeqCst);
        }
        result
    }
    fn exists(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<bool> {
        rightkit_credentials::Backend::exists(
            &self.shared.targets[self.target()],
            ns,
            account,
            mode,
        )
    }
    fn set(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        value: &rightkit_credentials::Secret,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<()> {
        if account == "highest-seen-unix" {
            let gate = self.shared.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                self.shared.entered.send(()).unwrap();
                gate.recv().unwrap();
            }
        }
        rightkit_credentials::Backend::set(
            &self.shared.targets[self.target()],
            ns,
            account,
            value,
            mode,
        )
    }
    fn add_if_absent(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        value: &rightkit_credentials::Secret,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<bool> {
        rightkit_credentials::Backend::add_if_absent(
            &self.shared.targets[self.target()],
            ns,
            account,
            value,
            mode,
        )
    }
    fn delete(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<bool> {
        if account == "legacy-record" {
            if let Some(gate) = self.shared.gate.lock().unwrap().take() {
                self.shared.entered.send(()).unwrap();
                gate.recv().unwrap();
            }
        }
        rightkit_credentials::Backend::delete(
            &self.shared.targets[self.target()],
            ns,
            account,
            mode,
        )
    }
    fn delete_if_equals(
        &self,
        ns: &rightkit_credentials::Namespace,
        account: &str,
        value: &rightkit_credentials::Secret,
        mode: rightkit_credentials::Interaction,
    ) -> rightkit_credentials::Result<rightkit_credentials::ConditionalDelete> {
        rightkit_credentials::Backend::delete_if_equals(
            &self.shared.targets[self.target()],
            ns,
            account,
            value,
            mode,
        )
    }
}

#[test]
fn switched_default_cannot_release_clock_guard_before_old_target_worker_finishes() {
    use rightkit_credentials::{Config, Namespace, Store};
    use std::sync::{mpsc, Arc, Mutex};
    let f = fixture();
    let (release, gate) = mpsc::channel();
    let (entered, started) = mpsc::channel();
    let shared = Arc::new(SwitchingState {
        targets: [TestBackend::default(), TestBackend::default()],
        selected: std::sync::atomic::AtomicUsize::new(0),
        switch_after_read: std::sync::atomic::AtomicBool::new(false),
        gate: Mutex::new(Some(gate)),
        entered,
    });
    let store = Store::open(
        Config::new(Namespace::new("native-switching").unwrap())
            .backend(Arc::new(SwitchingStore {
                shared: shared.clone(),
                fixed: None,
            }))
            .timeout(Duration::from_millis(30)),
    );
    let client = f.client_with(|cfg| {
        cfg.with_credential_clock(store.clone(), "highest-seen-unix")
            .with_persist_wait(Duration::from_millis(20))
    });
    let clock = client.native_clock_store().unwrap();
    // The reader gets its own generous worker wait: only the retained guard may
    // refuse it before release, and a correct load after release must not time out.
    let reader_client = f.client_with(|cfg| {
        cfg.with_credential_clock(store, "highest-seen-unix")
            .with_persist_wait(Duration::from_secs(5))
    });
    let new = ClockState {
        highest_seen: 100,
        trusted_process: "native".into(),
        last_server_iat: 90,
    };
    assert!(clock.compare_and_save(None, &new).is_err());
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    shared.selected.store(1, SeqCst);
    let other = reader_client.native_clock_store().unwrap();
    let (finished, result) = mpsc::channel();
    let refused = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refused_in_reader = refused.clone();
    let reader = std::thread::spawn(move || {
        // A clock load polls the ledger lock only up to a bounded wait, so
        // retry until the retained guard is released. Each refusal is counted
        // so the test can prove an attempt really ran while the gate was
        // closed; any success then would prove the guard was dropped early.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match other.load() {
                Ok(value) => {
                    finished.send(Ok(value)).unwrap();
                    break;
                }
                Err(rightkit_license::ClientClockError::Busy) => {
                    refused_in_reader.fetch_add(1, SeqCst);
                    if std::time::Instant::now() >= deadline {
                        finished
                            .send(Err(rightkit_license::ClientClockError::Busy))
                            .unwrap();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                // Only the lock-wait refusal proves the retained guard blocked the
                // load; any other error (e.g. a reader-worker timeout) is unrelated,
                // is not counted, and ends the reader so the test fails with it.
                Err(other_error) => {
                    finished.send(Err(other_error)).unwrap();
                    break;
                }
            }
        }
    });
    // Even though B reports no held rotation, A's abandoned native write still owns guard:
    // wait until the reader has provably attempted and been refused at least once.
    let wait_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while refused.load(SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < wait_deadline,
            "reader never attempted a load while the gate was closed"
        );
        if let Ok(early) = result.try_recv() {
            panic!(
                "reader ended before the gate opened (only Busy may refuse): {:?}",
                early.as_ref().err()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Ok(early) = result.try_recv() {
        panic!(
            "reader ended before the gate opened (only Busy may refuse): {:?}",
            early.as_ref().err()
        );
    }
    release.send(()).unwrap();
    let outcome = result.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        outcome.is_ok(),
        "clock load did not succeed after release (only Busy may refuse): {:?}",
        outcome.as_ref().err()
    );
    reader.join().unwrap();
    assert!(shared.targets[0].text("highest-seen-unix").is_some());
    assert!(shared.targets[1].text("highest-seen-unix").is_none());
}

#[test]
fn pending_release_failure_or_panic_preserves_record_and_never_calls_server() {
    let f = fixture();
    let client = f.client();
    client.activate(SecretString::new(KEY)).unwrap();
    let before = f.backend.text(ACCOUNT).unwrap();
    assert!(client.deactivate_with_pending_release(|_| Err(())).is_err());
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    assert!(client
        .deactivate_with_pending_release(|_| panic!("queue failed"))
        .is_err());
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    assert_eq!(f.state.deactivate_calls.load(SeqCst), 0);
    let mut queued = false;
    client
        .deactivate_with_pending_release(|intent| {
            assert_eq!(f.backend.envelope().unwrap()["state"], "active");
            match intent {
                rightkit_license::PendingLicenseRelease::Activation(token) => {
                    assert!(!token.is_blank())
                }
                _ => panic!("expected native activation release"),
            }
            queued = true;
            Ok(())
        })
        .unwrap();
    assert!(queued);
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
}

#[test]
fn marker_compare_commit_and_confirm_stay_on_one_native_target() {
    use rightkit_credentials::{Config, Namespace, Store};
    use rightkit_license::{LicenseClient, LicenseClientConfig, LicenseSetup};
    use std::sync::{mpsc, Arc, Mutex};
    let f = fixture();
    let (entered, _started) = mpsc::channel();
    let shared = Arc::new(SwitchingState {
        targets: [TestBackend::default(), TestBackend::default()],
        selected: std::sync::atomic::AtomicUsize::new(0),
        switch_after_read: std::sync::atomic::AtomicBool::new(false),
        gate: Mutex::new(None),
        entered,
    });
    let token = common::Claims::term(now() - 100, now() + 3600).token();
    shared.targets[0].put_text(
        ACCOUNT,
        &serde_json::json!({"rkv":1,"revision":"active-A","state":"active",
        "record":{"status":"active","signedToken":token,"clockInitialized":false}})
        .to_string(),
    );
    shared.targets[1].put_text(
        ACCOUNT,
        &serde_json::json!({"rkv":1,"revision":"removed-B","state":"tombstone"}).to_string(),
    );
    let store = Store::open(
        Config::new(Namespace::new("marker-switching").unwrap()).backend(Arc::new(
            SwitchingStore {
                shared: shared.clone(),
                fixed: None,
            },
        )),
    );
    let cfg = LicenseClientConfig::new(APP, HOST, "test", common::pubkey(), store, f.license_dir())
        .unwrap();
    let client = LicenseClient::new(LicenseSetup::configured(cfg), Arc::new(FixedDevice)).unwrap();
    let source = client.native_entitlement_source().unwrap();
    let before = signed(&source);
    shared.switch_after_read.store(true, SeqCst);
    source.mark_clock_initialized(&before).unwrap();
    let a: serde_json::Value =
        serde_json::from_str(&shared.targets[0].text(ACCOUNT).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&shared.targets[1].text(ACCOUNT).unwrap()).unwrap();
    assert_eq!(a["record"]["clockInitialized"], true);
    assert_eq!(b["state"], "tombstone");
    assert_eq!(b["revision"], "removed-B");
}

#[test]
fn legacy_cleanup_timeout_retains_operation_guard_until_native_delete_settles() {
    use rightkit_credentials::{Config, Namespace, Store};
    use rightkit_license::LegacySource;
    use std::sync::{mpsc, Arc, Mutex};
    let f = fixture();
    let (release, gate) = mpsc::channel();
    let (entered, started) = mpsc::channel();
    let shared = Arc::new(SwitchingState {
        targets: [TestBackend::default(), TestBackend::default()],
        selected: std::sync::atomic::AtomicUsize::new(0),
        switch_after_read: std::sync::atomic::AtomicBool::new(false),
        gate: Mutex::new(Some(gate)),
        entered,
    });
    shared.targets[0].put_text(
        "legacy-record",
        &serde_json::json!({"status":"active",
        "signedToken":common::Claims::term(now()-100, now()+3600).token()})
        .to_string(),
    );
    let store = Store::open(
        Config::new(Namespace::new("legacy-delete").unwrap())
            .backend(Arc::new(SwitchingStore {
                shared: shared.clone(),
                fixed: None,
            }))
            .timeout(Duration::from_millis(30)),
    );
    let client = f.client_with(|cfg| cfg.with_persist_wait(Duration::from_millis(20)));
    client
        .import_legacy(&LegacySource {
            store,
            account: "legacy-record".into(),
        })
        .unwrap();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    shared.selected.store(1, SeqCst);
    // Own generous worker wait for the reader; the writer client keeps its short one.
    let reader_client = f.client_with(|cfg| cfg.with_persist_wait(Duration::from_secs(5)));
    let other = reader_client.native_entitlement_source().unwrap();
    let (finished, result) = mpsc::channel();
    let refused = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refused_in_reader = refused.clone();
    let reader = std::thread::spawn(move || {
        // A native read waits only a bounded time (250 ms) for the operation
        // lock and then reports Refused, so retry until the retained guard is
        // released. Each refusal is counted so the test can prove an attempt
        // really ran while the gate was closed; any success then would prove
        // the guard was dropped while the timed-out delete was still running.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            match other.read() {
                Ok(value) => {
                    finished.send(Ok(value)).unwrap();
                    break;
                }
                Err(NativeSourceError::Refused) => {
                    refused_in_reader.fetch_add(1, SeqCst);
                    if std::time::Instant::now() >= deadline {
                        finished.send(Err(NativeSourceError::Refused)).unwrap();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                // Only the lock-wait Refused proves the retained guard blocked the
                // read; Unavailable is unrelated, is not counted, and fails the test.
                Err(other_error) => {
                    finished.send(Err(other_error)).unwrap();
                    break;
                }
            }
        }
    });
    // No read may succeed while the delete worker still holds the operation
    // guard: wait until the reader has provably attempted and been refused.
    let wait_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while refused.load(SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < wait_deadline,
            "reader never attempted a read while the gate was closed"
        );
        if let Ok(early) = result.try_recv() {
            panic!(
                "reader ended before the gate opened (only Refused may count): {:?}",
                early.as_ref().err()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Ok(early) = result.try_recv() {
        panic!(
            "reader ended before the gate opened (only Refused may count): {:?}",
            early.as_ref().err()
        );
    }
    release.send(()).unwrap();
    let outcome = result.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        outcome.is_ok(),
        "read did not succeed after release (only Refused may count): {:?}",
        outcome.as_ref().err()
    );
    reader.join().unwrap();
    assert!(shared.targets[0].text("legacy-record").is_none());
}

// ---- bare record upgrade (rightkit#1286) --------------------------------------------------

use rightkit_license::{
    LegacySource, Lifecycle, MessageCode, NativeRefusal, RevisionState, ViewStatus,
};

/// A term token valid now, for the fixture device.
fn good_claims() -> common::Claims {
    let mut claims = common::Claims::term(now() - 100, now() + 3600);
    claims.id = LICENSE_ID;
    claims
}

/// The record an older `@rightkit/license` client stored: no `rkv`, no revision.
fn bare_text(token: &str) -> String {
    format!(
        r#"{{"status":"active","deviceId":"{DEVICE}","activationToken":"{ACTIVATION}","signedToken":"{token}","licenseId":"{LICENSE_ID}"}}"#
    )
}

fn stored_revision(f: &Fixture) -> String {
    f.backend.envelope().unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn bare_same_item_signed_record_upgrades_on_first_native_read() {
    let f = fixture();
    let client = f.client();
    let token = good_claims().token();
    f.backend.put_text(ACCOUNT, &bare_text(&token));
    let source = client.native_entitlement_source().unwrap();
    let native = signed(&source);
    assert_eq!(native.token, token);
    assert_eq!(native.device_binding, DEVICE);
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["rkv"], 1);
    assert_eq!(stored["state"], "active");
    assert_eq!(stored["revision"].as_str().unwrap(), native.revision);
    assert_eq!(stored["record"]["signedToken"], token.as_str());
    assert_eq!(f.backend.primary_sets.load(SeqCst), 1);
    assert_eq!(client.last_native_refusal(), None);
    // A second read finds the envelope: same revision, no further write.
    let again = signed(&source);
    assert_eq!(again.revision, native.revision);
    assert_eq!(f.backend.primary_sets.load(SeqCst), 1);
    assert!(f.backend.text(MARKER).is_none());
}

#[test]
fn bare_record_upgrades_on_status_and_native_reads_the_same_revision() {
    let f = fixture();
    let client = f.client();
    f.backend
        .put_text(ACCOUNT, &bare_text(&good_claims().token()));
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    assert_eq!(view.revision_state, RevisionState::Active);
    let revision = stored_revision(&f);
    let native = signed(&client.native_entitlement_source().unwrap());
    assert_eq!(native.revision, revision);
    assert_eq!(f.backend.primary_sets.load(SeqCst), 1);
}

#[test]
fn bare_snake_case_lease_record_upgrades_with_hashed_binding() {
    let f = fixture();
    let client = f.client();
    let mut claims = good_claims();
    claims.device = DEVICE_HASH;
    let raw = serde_json::json!({
        "status": "active",
        "signed_token": claims.token(),
        "lease_id": "lease-1",
        "lease_generation": 1,
        "lease_expires_at_unix": now() + 3600,
        "lease_grace_seconds": 0,
    });
    f.backend.put_text(ACCOUNT, &raw.to_string());
    let native = signed(&client.native_entitlement_source().unwrap());
    assert_eq!(native.device_binding, DEVICE_HASH);
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["rkv"], 1);
    assert_eq!(stored["record"]["deviceId"], DEVICE_HASH);
    assert_eq!(stored["record"]["lease"]["leaseId"], "lease-1");
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
}

#[test]
fn status_and_native_agree_on_every_record_shape() {
    // (name, item text or None for absent, grants)
    let wrong_app = {
        let mut claims = good_claims();
        claims.app = "someone-else";
        claims.token()
    };
    let wrong_key = good_claims().token_signed_by(&common::other_signing_key());
    let wrong_device = {
        let mut claims = good_claims();
        claims.device = "another-device";
        claims.token()
    };
    let shapes: Vec<(&str, Option<String>, bool)> = vec![
        ("absent", None, false),
        ("corrupt", Some("{not json".to_string()), false),
        ("bare good", Some(bare_text(&good_claims().token())), true),
        ("bare wrong app", Some(bare_text(&wrong_app)), false),
        ("bare wrong key", Some(bare_text(&wrong_key)), false),
        ("bare wrong device", Some(bare_text(&wrong_device)), false),
        (
            "bare revoked",
            Some(r#"{"status":"revoked","deviceId":"dev-1","signedToken":"x"}"#.to_string()),
            false,
        ),
        (
            "bare activation only",
            Some(format!(
                r#"{{"status":"active","deviceId":"{DEVICE}","activationToken":"{ACTIVATION}"}}"#
            )),
            false,
        ),
        (
            "bare invalid status",
            Some(format!(
                r#"{{"status":"nonsense","deviceId":"{DEVICE}","signedToken":"{}"}}"#,
                good_claims().token()
            )),
            false,
        ),
    ];
    for (name, text, grants) in shapes {
        let f = fixture();
        let client = f.client();
        if let Some(text) = &text {
            f.backend.put_text(ACCOUNT, text);
        }
        let view = client.status();
        let status_grants = matches!(view.status, ViewStatus::Active | ViewStatus::Grace);
        let native = client.native_entitlement_source().unwrap().read();
        let native_grants = matches!(native, Ok(NativeCredential::Signed(_)));
        assert_eq!(status_grants, grants, "{name}: status {view:?}");
        assert_eq!(native_grants, grants, "{name}: native");
    }
    // Revisioned shapes: an active envelope, a tombstone and an unresolved marker.
    let f = fixture();
    let client = f.client();
    client.activate(SecretString::new(KEY)).unwrap();
    assert_eq!(client.status().status, ViewStatus::Active);
    assert!(matches!(
        client.native_entitlement_source().unwrap().read(),
        Ok(NativeCredential::Signed(_))
    ));
    f.leave_marker("unresolved");
    assert_ne!(client.status().status, ViewStatus::Active);
    assert!(client.native_entitlement_source().unwrap().read().is_err());
    let f = fixture();
    let client = f.client();
    client.activate(SecretString::new(KEY)).unwrap();
    client.deactivate().unwrap();
    assert_eq!(client.status().status, ViewStatus::None);
    assert!(matches!(
        client.native_entitlement_source().unwrap().read(),
        Ok(NativeCredential::Absent)
    ));
}

#[test]
fn bare_record_failing_authentication_is_never_written_and_refuses_both_paths() {
    let f = fixture();
    let client = f.client();
    let mut claims = good_claims();
    claims.app = "someone-else";
    let bare = bare_text(&claims.token());
    f.backend.put_text(ACCOUNT, &bare);
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Unverifiable, "{view:?}");
    assert_eq!(view.lifecycle, Lifecycle::MigrationRequired);
    assert_eq!(view.message, MessageCode::MigrationRequired);
    assert_eq!(view.revision_state, RevisionState::None);
    assert!(view.tier.is_none() && view.features.is_empty());
    let source = client.native_entitlement_source().unwrap();
    assert!(matches!(source.read(), Err(NativeSourceError::Refused)));
    assert_eq!(
        client.last_native_refusal(),
        Some(NativeRefusal::Unauthenticated)
    );
    // Neither written nor deleted.
    assert_eq!(f.backend.text(ACCOUNT).as_deref(), Some(bare.as_str()));
    assert_eq!(f.backend.primary_sets.load(SeqCst), 0);
    assert!(f.backend.text(MARKER).is_none());
    // Repeated reads stay refused and still write nothing.
    assert!(source.read().is_err());
    assert_eq!(client.status().lifecycle, Lifecycle::MigrationRequired);
    assert_eq!(f.backend.text(ACCOUNT).as_deref(), Some(bare.as_str()));
    assert_eq!(f.backend.primary_sets.load(SeqCst), 0);
}

#[test]
fn expired_bare_record_upgrades_as_expired() {
    let f = fixture();
    let client = f.client();
    let mut claims = common::Claims::term(now() - 7200, now() - 3600);
    claims.id = LICENSE_ID;
    f.backend.put_text(ACCOUNT, &bare_text(&claims.token()));
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Expired, "{view:?}");
    assert_eq!(view.revision_state, RevisionState::Active);
    assert_eq!(f.backend.envelope().unwrap()["rkv"], 1);
    // The native source hands the expired token on; the engine judges its time.
    let native = signed(&client.native_entitlement_source().unwrap());
    assert_eq!(native.revision, stored_revision(&f));
}

#[test]
fn bare_record_in_a_different_item_is_not_auto_imported() {
    let f = fixture();
    let client = f.client();
    let bare = bare_text(&good_claims().token());
    f.backend.put_text("legacy-record", &bare);
    assert_eq!(client.status().status, ViewStatus::None);
    assert!(matches!(
        client.native_entitlement_source().unwrap().read(),
        Ok(NativeCredential::Absent)
    ));
    assert!(f.backend.text(ACCOUNT).is_none());
    assert_eq!(
        f.backend.text("legacy-record").as_deref(),
        Some(bare.as_str())
    );
    assert_eq!(f.backend.primary_sets.load(SeqCst), 0);
}

#[test]
fn existing_envelope_and_tombstone_are_never_rewritten_by_reads() {
    let f = fixture();
    let client = f.client();
    let source = client.native_entitlement_source().unwrap();
    client.activate(SecretString::new(KEY)).unwrap();
    let active = f.backend.text(ACCOUNT).unwrap();
    let writes = f.backend.primary_sets.load(SeqCst);
    for _ in 0..2 {
        assert_eq!(client.status().status, ViewStatus::Active);
        let _ = signed(&source);
    }
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), active);
    assert_eq!(f.backend.primary_sets.load(SeqCst), writes);
    client.deactivate().unwrap();
    let tombstone = f.backend.text(ACCOUNT).unwrap();
    let writes = f.backend.primary_sets.load(SeqCst);
    for _ in 0..2 {
        assert_eq!(client.status().status, ViewStatus::None);
        assert!(matches!(source.read(), Ok(NativeCredential::Absent)));
    }
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), tombstone);
    assert_eq!(f.backend.primary_sets.load(SeqCst), writes);
}

#[test]
fn upgrade_commit_failure_refuses_then_recovers() {
    let f = fixture();
    let client = f.client();
    let bare = bare_text(&good_claims().token());
    f.backend.put_text(ACCOUNT, &bare);
    f.backend.fail_primary_set.store(true, SeqCst);
    let source = client.native_entitlement_source().unwrap();
    assert!(matches!(source.read(), Err(NativeSourceError::Refused)));
    assert!(matches!(
        client.last_native_refusal(),
        Some(NativeRefusal::Unresolved | NativeRefusal::Store)
    ));
    assert_ne!(client.status().status, ViewStatus::Active);
    // Nothing granted, and the stored record is still the bare one.
    assert_eq!(f.backend.text(ACCOUNT).as_deref(), Some(bare.as_str()));
    f.backend.fail_primary_set.store(false, SeqCst);
    let mut recovered = false;
    for _ in 0..3 {
        if client.status().status == ViewStatus::Active {
            recovered = true;
            break;
        }
    }
    assert!(recovered, "status never recovered");
    let native = signed(&source);
    assert_eq!(native.revision, stored_revision(&f));
    assert_eq!(client.last_native_refusal(), None);
}

#[test]
fn concurrent_first_reads_upgrade_exactly_once() {
    let f = fixture();
    let client = f.client();
    f.backend
        .put_text(ACCOUNT, &bare_text(&good_claims().token()));
    let source = client.native_entitlement_source().unwrap();
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    let _ = client.status();
                    source.read()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // A read may lose the 250 ms lock race (refused, never granted); every grant agrees.
    let revision = stored_revision(&f);
    let mut granted = 0;
    for result in results {
        match result {
            Ok(NativeCredential::Signed(value)) => {
                assert_eq!(value.revision, revision);
                granted += 1;
            }
            Err(NativeSourceError::Refused) => {}
            _ => panic!("unexpected native result"),
        }
    }
    assert!(granted >= 1, "no read upgraded the record");
    assert_eq!(signed(&source).revision, revision);
    assert_eq!(f.backend.primary_sets.load(SeqCst), 1, "exactly one write");
}

#[test]
fn import_legacy_same_item_after_auto_upgrade_is_noop() {
    let f = fixture();
    let client = f.client();
    f.backend
        .put_text(ACCOUNT, &bare_text(&good_claims().token()));
    let native = signed(&client.native_entitlement_source().unwrap());
    let stored = f.backend.text(ACCOUNT).unwrap();
    let source = LegacySource {
        store: f.store.clone(),
        account: ACCOUNT.to_string(),
    };
    let view = client.import_legacy(&source).unwrap();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), stored);
    assert_eq!(stored_revision(&f), native.revision);
    assert_eq!(f.backend.primary_sets.load(SeqCst), 1);
}

#[test]
fn native_refusal_codes_carry_no_secret() {
    let all = [
        (NativeRefusal::Busy, "busy"),
        (NativeRefusal::Unresolved, "unresolved"),
        (NativeRefusal::Store, "store"),
        (NativeRefusal::Corrupt, "corrupt"),
        (NativeRefusal::Unauthenticated, "unauthenticated"),
        (NativeRefusal::Device, "device"),
        (NativeRefusal::InvalidRecord, "invalid_record"),
    ];
    for (refusal, code) in all {
        assert_eq!(refusal.as_str(), code);
        assert!(code.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    }
    let f = fixture();
    let client = f.client();
    let source = client.native_entitlement_source().unwrap();
    assert_eq!(client.last_native_refusal(), None);
    let mut claims = good_claims();
    claims.app = "someone-else";
    let token = claims.token();
    f.backend.put_text(ACCOUNT, &bare_text(&token));
    assert!(source.read().is_err());
    f.backend.put_text(ACCOUNT, "{not json");
    assert!(source.read().is_err());
    assert_eq!(client.last_native_refusal(), Some(NativeRefusal::Corrupt));
    f.backend.deny_get.store(true, SeqCst);
    assert!(source.read().is_err());
    assert_eq!(client.last_native_refusal(), Some(NativeRefusal::Store));
    f.backend.deny_get.store(false, SeqCst);
    f.leave_marker("unresolved");
    assert!(source.read().is_err());
    assert_eq!(
        client.last_native_refusal(),
        Some(NativeRefusal::Unresolved)
    );
    f.backend.remove(MARKER);
    f.backend.put_text(ACCOUNT, &bare_text(&token));
    assert!(source.read().is_err());
    let last = client.last_native_refusal().unwrap();
    let shown = format!("{last:?} {last} {}", last.as_str());
    let dir = f.dir.path().to_string_lossy().to_string();
    for secret in [
        token.as_str(),
        DEVICE,
        DEVICE_HASH,
        ACTIVATION,
        LICENSE_ID,
        dir.as_str(),
    ] {
        assert!(!shown.contains(secret), "refusal text leaks {secret}");
    }
    f.assert_no_secret("refusal", &shown);
    // A read that is not refused clears it.
    f.backend.remove(ACCOUNT);
    assert!(matches!(source.read(), Ok(NativeCredential::Absent)));
    assert_eq!(client.last_native_refusal(), None);
}
