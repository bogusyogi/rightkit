mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use common::{other_pubkey, pubkey, signing_key, Claims, APP, DEVICE};
use rightkit_license::{
    EngineEntitlementError as E, EngineEntitlements, EngineEntitlementsConfig, VerifyError,
    MAX_CACHE_TTL_SECS,
};

const NOW: i64 = 1_800_000_000;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "rightkit-license-engine-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Self(d)
    }
    fn write(&self, token: &str) {
        fs::write(self.0.join("license.token"), token).unwrap();
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn engine(dir: &Dir, device: &'static str, now: Arc<AtomicI64>) -> EngineEntitlements {
    engine_key(dir, pubkey(), device, now)
}

fn engine_key(
    dir: &Dir,
    key: [u8; 32],
    device: &'static str,
    now: Arc<AtomicI64>,
) -> EngineEntitlements {
    let cfg =
        EngineEntitlementsConfig::new(key, APP, dir.0.clone(), move || Ok(device.to_string()));
    EngineEntitlements::new(cfg).with_wall_clock(move || Ok(now.load(Ordering::SeqCst)))
}

fn clock() -> Arc<AtomicI64> {
    Arc::new(AtomicI64::new(NOW))
}

#[test]
fn valid_pro_licence_passes() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000).token());
    let e = engine(&d, DEVICE, clock());
    assert!(e.require_pro().is_ok());
    assert!(e.current().unwrap().is_pro());
}

#[test]
fn missing_licence_is_refused() {
    let d = Dir::new();
    assert_eq!(engine(&d, DEVICE, clock()).require_pro(), Err(E::NoLicense));
}

#[test]
fn tampered_token_is_refused() {
    let d = Dir::new();
    let mut c = Claims::term(NOW - 100, NOW + 10_000);
    let genuine = {
        c.tier = "plus";
        c.token()
    };
    // Swap the payload for a pro payload but keep the plus signature.
    c.tier = "pro";
    let pro_payload = c.token();
    let (pro_body, _) = pro_payload.split_once('.').unwrap();
    let (_, plus_sig) = genuine.split_once('.').unwrap();
    d.write(&format!("{pro_body}.{plus_sig}"));
    assert_eq!(
        engine(&d, DEVICE, clock()).require_pro(),
        Err(E::Invalid(VerifyError::BadSignature))
    );
}

#[test]
fn wrong_signer_is_refused() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000).token());
    let e = engine_key(&d, other_pubkey(), DEVICE, clock());
    assert_eq!(e.require_pro(), Err(E::Invalid(VerifyError::BadSignature)));
    let _ = signing_key();
}

#[test]
fn expired_token_is_refused() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 10_000, NOW - 1).token());
    assert!(matches!(
        engine(&d, DEVICE, clock()).require_pro(),
        Err(E::Invalid(VerifyError::Expired { .. }))
    ));
}

#[test]
fn other_device_is_refused() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000).token());
    assert_eq!(
        engine(&d, "some-other-device", clock()).require_pro(),
        Err(E::Invalid(VerifyError::DeviceMismatch))
    );
}

#[test]
fn other_app_is_refused() {
    let d = Dir::new();
    let mut c = Claims::term(NOW - 100, NOW + 10_000);
    c.app = "scraperight";
    d.write(&c.token());
    assert!(matches!(
        engine(&d, DEVICE, clock()).require_pro(),
        Err(E::Invalid(VerifyError::AppMismatch { .. }))
    ));
}

#[test]
fn device_source_failure_is_refused() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000).token());
    let cfg =
        EngineEntitlementsConfig::new(pubkey(), APP, d.0.clone(), || Err("no keychain".into()));
    let e = EngineEntitlements::new(cfg).with_wall_clock(|| Ok(NOW));
    assert!(matches!(e.require_pro(), Err(E::DeviceUnavailable(_))));
}

#[test]
fn free_tier_and_missing_feature_are_not_entitled() {
    let d = Dir::new();
    let mut c = Claims::term(NOW - 100, NOW + 10_000);
    c.tier = "plus";
    d.write(&c.token());
    let e = engine(&d, DEVICE, clock());
    assert_eq!(e.require_pro(), Err(E::NotEntitled));
    // The test token carries no features.
    assert_eq!(e.require("batch"), Err(E::NotEntitled));
    assert_eq!(e.require("  "), Err(E::NotEntitled));
}

#[test]
fn pushed_flag_snapshot_and_env_are_ignored() {
    let d = Dir::new();
    // Everything a caller or window could push, with no signed token on disk.
    fs::write(d.0.join("settings.json"), r#"{"is_pro":true,"tier":"pro"}"#).unwrap();
    fs::write(d.0.join("entitlements.json"), r#"{"pro":true}"#).unwrap();
    std::env::set_var("HEARDRIGHT_ENGINE_TEST_MODE", "1");
    std::env::set_var("RIGHTKIT_FORCE_PRO", "1");
    let e = engine(&d, DEVICE, clock());
    e.invalidate(); // a "licence changed" hint never grants
    assert_eq!(e.require_pro(), Err(E::NoLicense));
    // A garbage token file next to the pushed flag is also refused.
    d.write("pro");
    assert!(e.require_pro().is_err());
    std::env::remove_var("HEARDRIGHT_ENGINE_TEST_MODE");
    std::env::remove_var("RIGHTKIT_FORCE_PRO");
}

#[test]
fn cache_is_bounded_and_expiry_is_rechecked() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 100).token());
    let now = clock();
    let e = engine(&d, DEVICE, now.clone());
    assert!(e.require_pro().is_ok());

    // Licence file removed: still served from cache inside the TTL...
    fs::remove_file(d.0.join("license.token")).unwrap();
    now.store(NOW + 10, Ordering::SeqCst);
    assert!(e.require_pro().is_ok());
    // ...until invalidated...
    e.invalidate();
    assert_eq!(e.require_pro(), Err(E::NoLicense));

    // ...or until the TTL passes.
    d.write(&Claims::term(NOW - 100, NOW + 100_000).token());
    now.store(NOW, Ordering::SeqCst);
    assert!(e.require_pro().is_ok());
    fs::remove_file(d.0.join("license.token")).unwrap();
    now.store(NOW + MAX_CACHE_TTL_SECS + 1, Ordering::SeqCst);
    assert_eq!(e.require_pro(), Err(E::NoLicense));
}

#[test]
fn cached_entitlement_still_expires() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 30).token());
    let now = clock();
    let e = engine(&d, DEVICE, now.clone());
    assert!(e.require_pro().is_ok());
    now.store(NOW + 31, Ordering::SeqCst);
    assert!(matches!(
        e.require_pro(),
        Err(E::Invalid(VerifyError::Expired { .. }))
    ));
}

#[test]
fn ttl_is_clamped_and_path_escape_ignored() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000_000).token());
    let now = clock();
    let n2 = now.clone();
    let cfg = EngineEntitlementsConfig::new(pubkey(), APP, d.0.clone(), || Ok(DEVICE.into()))
        .with_cache_ttl_secs(i64::MAX)
        .with_token_file("../outside");
    let e = EngineEntitlements::new(cfg).with_wall_clock(move || Ok(n2.load(Ordering::SeqCst)));
    assert!(e.require_pro().is_ok()); // fell back to license.token inside the data root
    fs::remove_file(d.0.join("license.token")).unwrap();
    now.store(NOW + MAX_CACHE_TTL_SECS + 1, Ordering::SeqCst);
    assert_eq!(e.require_pro(), Err(E::NoLicense));
}

fn credential_engine(
    dir: &Dir,
    reader: impl Fn() -> Result<Option<String>, String> + Send + Sync + 'static,
) -> EngineEntitlements {
    let cfg = EngineEntitlementsConfig::new(pubkey(), APP, dir.0.clone(), || Ok(DEVICE.into()))
        .with_credential_reader(reader);
    EngineEntitlements::new(cfg).with_wall_clock(|| Ok(NOW))
}

#[test]
fn credential_source_verifies_without_token_file() {
    let d = Dir::new(); // no license.token written: no file bridge needed
    let token = Claims::term(NOW - 100, NOW + 10_000).token();
    let e = credential_engine(&d, move || Ok(Some(token.clone())));
    assert!(e.require_pro().is_ok());
}

#[test]
fn credential_source_missing_is_no_license_and_ignores_token_file() {
    let d = Dir::new();
    d.write(&Claims::term(NOW - 100, NOW + 10_000).token());
    let e = credential_engine(&d, || Ok(None));
    assert_eq!(e.require_pro(), Err(E::NoLicense));
}

#[test]
fn credential_source_error_is_unreadable_not_no_license() {
    let d = Dir::new();
    let e = credential_engine(&d, || Err("keychain locked".into()));
    assert!(matches!(e.require_pro(), Err(E::Unreadable(_))));
}

#[test]
fn credential_source_still_verifies_signature_device_expiry_and_tier() {
    let d = Dir::new();
    // Expired token from the credential source is still refused on expiry.
    let expired = Claims::term(NOW - 10_000, NOW - 1).token();
    let e = credential_engine(&d, move || Ok(Some(expired.clone())));
    assert!(matches!(
        e.require_pro(),
        Err(E::Invalid(VerifyError::Expired { .. }))
    ));
    // Plus from the credential source is authentic but not Pro (no plus->pro).
    let mut c = Claims::term(NOW - 100, NOW + 10_000);
    c.tier = "plus";
    let plus = c.token();
    let e = credential_engine(&d, move || Ok(Some(plus.clone())));
    assert_eq!(e.require_pro(), Err(E::NotEntitled));
    // Oversized credential payload never reaches verification.
    let e = credential_engine(&d, || Ok(Some("a".repeat(20_000))));
    assert!(matches!(e.require_pro(), Err(E::Unreadable(_))));
}

#[test]
fn oversized_or_empty_token_file_is_refused() {
    let d = Dir::new();
    d.write("   \n");
    assert!(matches!(
        engine(&d, DEVICE, clock()).require_pro(),
        Err(E::Unreadable(_))
    ));
    d.write(&"a".repeat(20_000));
    assert!(matches!(
        engine(&d, DEVICE, clock()).require_pro(),
        Err(E::Unreadable(_))
    ));
}
