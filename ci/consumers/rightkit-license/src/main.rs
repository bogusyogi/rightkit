// rightkit-license 0.2.4 registry consumer, outside the workspace.
//
// Pure and offline. Verifies the published license-v2 vector with verify_token and rejects a wrong
// app, a wrong device, an expired token, a malformed token and a tampered payload. Verifies the
// published lease-envelope-v1 vector with verify_lease_envelope and rejects a changed generation.
// Loads a file clock store from a temp directory (a missing ledger reads as absent). Evaluates
// NativeEngineEntitlements over a native source that reports no credential, which must deny access.
// The public keys and vectors are copied verbatim from the crate's test-vectors/ (public material).
// State: a temp directory removed on exit. No network, no OS keychain, no real user data.
use rightkit_license::{
    verify_lease_envelope, verify_token, ClockStore, DeviceIdError, DeviceIdSource, FileClockStore,
    LicenseClient, LicenseMode, LicenseSetup, Lifecycle, LifecyclePolicy,
    MemoryClockStore, NativeCredential, NativeEngineEntitlements, NativeEntitlementSource,
    NativeSignedCredential, NativeSourceError, ProcessAnchor, VerifyError, ViewStatus,
};
use std::sync::Arc;
use std::path::PathBuf;

type Failure = Box<dyn std::error::Error>;

// test-vectors/license-v2.json publicKeyBase64url, decoded.
const LICENSE_KEY: [u8; 32] = [
    0x6c, 0x8e, 0xdc, 0x05, 0x9a, 0x57, 0x7c, 0xb4, 0xf4, 0x1f, 0x19, 0xcc, 0x00, 0x9b, 0x54, 0xa5,
    0x10, 0xe3, 0x86, 0x57, 0x3d, 0x42, 0xc7, 0xee, 0xa8, 0xdb, 0x80, 0x91, 0x27, 0xeb, 0x50, 0xd3,
];
// test-vectors/license-v2.json token, for app heardright, device dev-1, now 2000.
const LICENSE_TOKEN: &str = "eyJ2IjoyLCJpZCI6ImxpY18xIiwiYXBwIjoiaGVhcmRyaWdodCIsInRpZXIiOiJwcm8iLCJmZWF0dXJlcyI6WyJ3YWtlX3dvcmQiLCJ2b2ljZV9jb21tYW5kcyJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MTAwMCwiZXhwIjo5OTk5OTk5OTk5fQ.JNmWEwG16VehpwJz9SooQIia0k1Ht0c150jca86h7sAnFj9tKuPO8yZhfyR48vx3Yd_03RQyYYtbKm08TCgsBg";

// test-vectors/lease-envelope-v1.json publicKeyBase64url, decoded.
const LEASE_KEY: [u8; 32] = [
    0xea, 0x4a, 0x6c, 0x63, 0xe2, 0x9c, 0x52, 0x0a, 0xbe, 0xf5, 0x50, 0x7b, 0x13, 0x2e, 0xc5, 0xf9,
    0x95, 0x47, 0x76, 0xae, 0xbe, 0xbe, 0x7b, 0x92, 0x42, 0x1e, 0xea, 0x69, 0x14, 0x46, 0xd2, 0x2c,
];
// test-vectors/lease-envelope-v1.json envelope object, compact.
const LEASE_ENVELOPE: &str = r#"{"leaseId":"lease_01HZX","deviceIdHash":"9f2c1a7be04d3f56","pool":"pro","issuedAt":"2026-10-01T00:00:00.000Z","expiresAt":"2026-11-01T00:00:00.000Z","graceSeconds":604800,"generation":3,"entitlement":"ENTITLEMENT.TOKEN.NOT-COVERED-BY-ENVELOPE-SIGNATURE","signature":"oPLiisnLIjJDhXrJVyv5t9paEpBFRChGEsIj_85N-8y6F-ZMFmFJEqxWksNDlmxf_dkrdWp9Tpd1hvIik0PiBw"}"#;

const NOW: i64 = 2_000;

/// A native source with no stored credential: absence is a value, and it must grant nothing.
struct NoCredential;

/// Synthetic identity injected only to construct the disabled client.
struct SyntheticDevice;

impl DeviceIdSource for SyntheticDevice {
    fn device_id(&self) -> Result<String, DeviceIdError> { Ok("synthetic-device".into()) }
    fn device_id_hash(&self) -> Result<String, DeviceIdError> { Ok("synthetic-hash".into()) }
}

impl NativeEntitlementSource for NoCredential {
    fn read(&self) -> Result<NativeCredential, NativeSourceError> {
        Ok(NativeCredential::Absent)
    }

    fn mark_clock_initialized(
        &self,
        _credential: &NativeSignedCredential,
    ) -> Result<(), NativeSourceError> {
        Err(NativeSourceError::Refused)
    }
}

fn main() -> Result<(), Failure> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rk-license-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = exercise(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn exercise(dir: &std::path::Path) -> Result<(), Failure> {
    // The disabled setup is deliberately inert; this does not qualify configured operations.
    let client = LicenseClient::new(LicenseSetup::Disabled, Arc::new(SyntheticDevice))?;
    assert!(client.is_disabled());
    let status = client.status();
    assert_eq!(status.mode, LicenseMode::Disabled);
    assert_eq!(status.status, ViewStatus::None);

    // Published token verifies for its app and device at its vector time.
    let entitlement = verify_token(LICENSE_TOKEN, &LICENSE_KEY, "heardright", "dev-1", NOW)
        .expect("published license-v2 vector must verify");
    assert_eq!(entitlement.tier, "pro");
    assert_eq!(
        entitlement.features,
        vec!["wake_word".to_string(), "voice_commands".to_string()]
    );

    // Refusals: wrong app, wrong device, expired, not a token, and a tampered payload.
    assert!(matches!(
        verify_token(LICENSE_TOKEN, &LICENSE_KEY, "other-app", "dev-1", NOW),
        Err(VerifyError::AppMismatch { .. })
    ));
    assert!(matches!(
        verify_token(LICENSE_TOKEN, &LICENSE_KEY, "heardright", "dev-2", NOW),
        Err(VerifyError::DeviceMismatch)
    ));
    assert!(matches!(
        verify_token(LICENSE_TOKEN, &LICENSE_KEY, "heardright", "dev-1", 10_000_000_000),
        Err(VerifyError::Expired { .. })
    ));
    assert!(matches!(
        verify_token("no-separator", &LICENSE_KEY, "heardright", "dev-1", NOW),
        Err(VerifyError::Malformed)
    ));
    let tampered = format!("f{}", &LICENSE_TOKEN[1..]);
    assert!(
        verify_token(&tampered, &LICENSE_KEY, "heardright", "dev-1", NOW).is_err(),
        "a changed payload byte must not verify"
    );

    // Published lease envelope verifies; a changed generation must not.
    let lease = verify_lease_envelope(LEASE_ENVELOPE, &LEASE_KEY)
        .expect("published lease-envelope-v1 vector must verify");
    assert_eq!(lease.lease_id, "lease_01HZX");
    assert_eq!(lease.generation, 3);
    assert_eq!(lease.grace_seconds, 604_800);
    let changed = LEASE_ENVELOPE.replace("\"generation\":3", "\"generation\":4");
    assert!(
        verify_lease_envelope(&changed, &LEASE_KEY).is_err(),
        "a changed generation must not verify"
    );

    // File clock store: a fresh directory has no ledger, which reads as absent.
    let clock = FileClockStore::new(dir.join("clock.json"));
    assert!(
        matches!(clock.load(), Ok(None)),
        "missing clock ledger must read as absent"
    );

    // Native engine: no credential is a value that grants no access, not an error.
    let engine = NativeEngineEntitlements::new(
        NoCredential,
        MemoryClockStore::new(),
        ProcessAnchor::new(NOW),
        LICENSE_KEY,
        "heardright",
        LifecyclePolicy::default(),
    );
    let report = engine
        .current_at(NOW)
        .expect("an absent native credential is a verdict, not an error");
    assert_eq!(report.lifecycle, Lifecycle::Unlicensed);
    assert!(!report.grants_access(), "absent credential must not grant access");

    println!("rightkit-license 0.2.4 registry consumer passed");
    Ok(())
}
