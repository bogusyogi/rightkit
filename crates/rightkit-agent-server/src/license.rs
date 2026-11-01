//! Engine-side entitlement gate for the daemon (Pro entitlement).
//!
//! The daemon decides Pro itself, over the same stored record the licence owner (the desktop
//! shell's `rightkit-license` client) maintains. It never accepts a verdict, a token, a licence key
//! or a raw device id from its caller: not by environment, not by argument, not by a request. The
//! shell hands it only NON-secret locators (the licence directory, the credential namespaces and
//! accounts, see [`LicenseLocators`]); everything secret is read here, from the OS credential store,
//! and verified here.
//!
//! * Record: `CredentialTokenSource::from_record_store`, the published daemon-side reader. Each read
//!   takes the per-app licence operation lock (bounded 250 ms) and checks the rotation fence, so a
//!   write in flight is never read half-done; while it is busy or unresolved the check fails closed
//!   for that read. Nothing is stored as a result: the gate re-reads after a short, bounded TTL, so
//!   a refresh, revocation, deactivation or new activation is picked up without a daemon restart,
//!   and a launch that raced a licence write recovers by itself.
//! * Verification: the published `EngineEntitlements::require_pro` (signature against the suite key
//!   pinned below, app, device binding, expiry) over an anti-rollback clock on the shell's ledger
//!   (`<licence dir>/clock.json`, the owner's documented interop path).
//! * Device binding: the daemon opens the shell's device credential READ-ONLY
//!   (`DeviceService::get`; the daemon never mints an identity). A typed-key activation binds the token to the raw
//!   device id, a lease to its hash; both are derived locally and each is tried, so the token's own
//!   claim is never trusted to name its device.
//!
//! The locators are read once at process start and removed from the daemon's own environment
//! ([`capture_launch_locators`]); child processes and `${VAR}` expansion additionally refuse any
//! licence-named variable (`rightkit_app_settings::agent_config::licence_env`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_app_settings::agent_config::licence_env::{
    is_licence_env_name, ENV_LICENSE_ACCOUNT, ENV_LICENSE_DEVICE_ACCOUNT,
    ENV_LICENSE_DEVICE_SERVICE, ENV_LICENSE_DIR, ENV_LICENSE_SERVICE,
};
use rightkit_credentials::{Config, Interaction, Namespace, Store};
use rightkit_device::credentials::CredentialStore;
use rightkit_device::DeviceService;
use rightkit_license::{
    system_now_unix, AntiRollbackClock, ClockRefusal, CredentialTokenSource, EngineEntitlements,
    EngineEntitlementsConfig, Entitlement, FileClockStore, ProcessAnchor, TrustedClock,
};

/// Suite license-signing PUBLIC key (Ed25519). Public material only; the
/// private half never ships in any client or daemon binary.
pub const LICENSE_PUBKEY: [u8; 32] = [
    115, 7, 50, 136, 83, 85, 196, 64, 12, 17, 85, 146, 164, 44, 145, 184, 176, 39, 125, 70, 12,
    178, 181, 213, 156, 30, 40, 21, 167, 77, 93, 34,
];

/// The app identity RightKit's suite tokens are signed for.
pub const EXPECTED_APP: &str = "coderight";

/// The anti-rollback ledger inside the licence directory (the native client's default path).
const CLOCK_FILE: &str = "clock.json";
/// How long a granting verdict is served before the record is read again.
const GRANT_TTL: Duration = Duration::from_secs(30);
/// How long a refusal is served. Short, so a launch that raced a licence write (the owner holds
/// the operation lock) recovers within seconds.
const DENIAL_TTL: Duration = Duration::from_secs(5);

// ---- launch locators ---------------------------------------------------------------------------

/// Where the licence record and the device identity live. Nothing here is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseLocators {
    /// `<app data>/license`: operation lock, rotation lock directory and the clock ledger.
    pub license_dir: PathBuf,
    /// Credential namespace of the licence record.
    pub license_service: String,
    /// Credential account of the licence record.
    pub license_account: String,
    /// Credential namespace of the device identity.
    pub device_service: String,
    /// Credential account of the device identity.
    pub device_account: String,
}

impl LicenseLocators {
    /// The locators named by `lookup`, or `None` unless every one is present and non-empty.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let value = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Some(Self {
            license_dir: PathBuf::from(value(ENV_LICENSE_DIR)?),
            license_service: value(ENV_LICENSE_SERVICE)?,
            license_account: value(ENV_LICENSE_ACCOUNT)?,
            device_service: value(ENV_LICENSE_DEVICE_SERVICE)?,
            device_account: value(ENV_LICENSE_DEVICE_ACCOUNT)?,
        })
    }
}

static LAUNCH_LOCATORS: OnceLock<Option<LicenseLocators>> = OnceLock::new();

/// Read the launch locators once, then remove EVERY licence-named variable from this process's
/// environment, so no tool, hook, script or MCP server inherits one. Call first thing in `main`,
/// before any other thread exists.
pub fn capture_launch_locators() {
    let locators = LicenseLocators::from_lookup(|name| std::env::var(name).ok());
    let inherited: Vec<std::ffi::OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_str().is_some_and(is_licence_env_name))
        .collect();
    for name in inherited {
        std::env::remove_var(name);
    }
    let _ = LAUNCH_LOCATORS.set(locators);
}

/// The locators the shell passed this daemon at launch, if any.
pub fn launch_locators() -> Option<&'static LicenseLocators> {
    LAUNCH_LOCATORS.get().and_then(Option::as_ref)
}

// ---- the gate ----------------------------------------------------------------------------------

type DeviceRead = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

/// One anti-rollback clock shared by the verifiers (one ledger writer per process).
struct SharedClock<C>(Arc<Mutex<C>>);

impl<C> Clone for SharedClock<C> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<C: TrustedClock> TrustedClock for SharedClock<C> {
    fn trusted_now(&self, wall_now: i64, clock_was_initialized: bool) -> Result<i64, ClockRefusal> {
        match self.0.lock() {
            Ok(clock) => clock.trusted_now(wall_now, clock_was_initialized),
            Err(_) => Err(ClockRefusal::Unavailable(
                "clock state poisoned".to_string(),
            )),
        }
    }
}

struct Verdict {
    at: Instant,
    /// `exp` of the granting entitlement (a cached grant is never served past it).
    exp: Option<i64>,
    features: HashSet<String>,
}

struct GateState {
    verify: Box<dyn Fn() -> Option<Entitlement> + Send>,
    verdict: Option<Verdict>,
}

/// The daemon's licence gate: the verified Pro features of the stored record, live.
pub struct LicenseGate {
    state: Mutex<GateState>,
    grant_ttl: Duration,
    denial_ttl: Duration,
}

fn credential_store(service: &str) -> Result<Store, String> {
    let namespace = Namespace::new(service)
        .map_err(|error| format!("could not open secure storage: {error}"))?;
    // Background reads never raise a Keychain prompt.
    Ok(Store::open(
        Config::new(namespace).interaction(Interaction::NonInteractive),
    ))
}

fn verifier<C: TrustedClock>(
    app: &str,
    pubkey: [u8; 32],
    license_dir: &Path,
    records: Store,
    account: &str,
    device: impl Fn() -> Result<String, String> + Send + Sync + 'static,
    clock: C,
) -> EngineEntitlements<C> {
    let source = CredentialTokenSource::from_record_store(records, account, license_dir);
    // The gate caches; the verifier itself re-reads and re-verifies on every call.
    let config = EngineEntitlementsConfig::new(pubkey, app, license_dir, device)
        .with_token_source(source)
        .with_cache_ttl_secs(0);
    EngineEntitlements::with_clock(config, clock)
}

impl LicenseGate {
    /// Open the gate over the shell's record and device credential. Opens nothing for writing.
    pub fn open(locators: &LicenseLocators) -> Result<Self, String> {
        let records = credential_store(&locators.license_service)?;
        let device_store = CredentialStore::new(
            credential_store(&locators.device_service)?,
            locators.device_account.as_str(),
        )
        .map_err(|error| format!("could not open the device identity: {error}"))?;
        // Read-only: the daemon never mints a device identity.
        let device = DeviceService::new(device_store);
        let read_device: DeviceRead = Arc::new(move || match device.get() {
            Ok(Some(id)) if !id.trim().is_empty() => Ok(id),
            Ok(_) => Err("no device identity".to_string()),
            Err(_) => Err("device identity unavailable".to_string()),
        });
        let now = system_now_unix().map_err(|error| format!("no wall clock: {error}"))?;
        let clock = AntiRollbackClock::new(
            FileClockStore::new(locators.license_dir.join(CLOCK_FILE)),
            ProcessAnchor::new(now),
        );
        Ok(Self::with_parts(
            LICENSE_PUBKEY,
            EXPECTED_APP,
            &locators.license_dir,
            records,
            &locators.license_account,
            read_device,
            clock,
            (GRANT_TTL, DENIAL_TTL),
        ))
    }

    /// The gate over explicit parts (the production wiring above, and tests over a memory store).
    pub(crate) fn with_parts<C>(
        pubkey: [u8; 32],
        app: &str,
        license_dir: &Path,
        records: Store,
        account: &str,
        device: DeviceRead,
        clock: C,
        (grant_ttl, denial_ttl): (Duration, Duration),
    ) -> Self
    where
        C: TrustedClock + Send + 'static,
    {
        let clock = SharedClock(Arc::new(Mutex::new(clock)));
        // A typed-key activation binds its token to the raw device id, a lease to the id's hash.
        let raw = {
            let device = Arc::clone(&device);
            verifier(
                app,
                pubkey,
                license_dir,
                records.clone(),
                account,
                move || device(),
                clock.clone(),
            )
        };
        let hashed = verifier(
            app,
            pubkey,
            license_dir,
            records,
            account,
            move || device().map(|id| rightkit_device::device_id_hash(&id)),
            clock,
        );
        let verify = move || raw.require_pro().or_else(|_| hashed.require_pro()).ok();
        Self {
            state: Mutex::new(GateState {
                verify: Box::new(verify),
                verdict: None,
            }),
            grant_ttl,
            denial_ttl,
        }
    }

    /// The lower-cased feature names of the verified Pro entitlement; empty on Free, a tombstone,
    /// an unverifiable, expired, rolled-back or unreadable record, and while the owner is writing.
    pub fn features(&self) -> HashSet<String> {
        let Ok(mut state) = self.state.lock() else {
            return HashSet::new();
        };
        if let Some(verdict) = &state.verdict {
            // `exp` is set exactly when the verdict is a grant.
            let ttl = if verdict.exp.is_some() {
                self.grant_ttl
            } else {
                self.denial_ttl
            };
            let unexpired = verdict
                .exp
                .map_or(true, |exp| system_now_unix().is_ok_and(|now| now < exp));
            if verdict.at.elapsed() < ttl && unexpired {
                return verdict.features.clone();
            }
        }
        let entitlement = (state.verify)();
        let verdict = Verdict {
            at: Instant::now(),
            exp: entitlement.as_ref().map(|entitlement| entitlement.exp),
            features: entitlement
                .map(|entitlement| {
                    entitlement
                        .features
                        .iter()
                        .map(|feature| feature.trim().to_ascii_lowercase())
                        .filter(|feature| !feature.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        };
        let features = verdict.features.clone();
        state.verdict = Some(verdict);
        features
    }

    /// Whether the verified Pro entitlement includes `feature`.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features()
            .contains(&feature.trim().to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use rightkit_credentials::{MemoryBackend, Secret};
    use rightkit_license::SystemClock;

    const ACCOUNT: &str = "license-record";
    const DEVICE: &str = "dev-1";

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    fn pubkey() -> [u8; 32] {
        signing_key().verifying_key().to_bytes()
    }

    fn sign_with(key: &SigningKey, payload: &str) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let signature = key.sign(payload.as_bytes());
        format!(
            "{}.{}",
            engine.encode(payload.as_bytes()),
            engine.encode(signature.to_bytes())
        )
    }

    fn token(app: &str, tier: &str, device: &str, exp: i64) -> String {
        sign_with(
            &signing_key(),
            &format!(
                r#"{{"v":2,"id":"lic_1","app":"{app}","tier":"{tier}","features":["Routing","schedules"],"device_id":"{device}","iat":1000,"exp":{exp}}}"#
            ),
        )
    }

    const FAR_FUTURE: i64 = 9_999_999_999;

    fn active_record(token: &str) -> String {
        format!(r#"{{"status":"active","deviceId":"{DEVICE}","signedToken":"{token}"}}"#)
    }

    const TOMBSTONE: &str = r#"{"rkv":1,"revision":"r","state":"tombstone","record":{}}"#;

    struct Rig {
        dir: tempfile::TempDir,
        records: Store,
    }

    impl Rig {
        fn new() -> Self {
            let namespace = Namespace::new("cc.coderight.licence.test").unwrap();
            let records =
                Store::open(Config::new(namespace).backend(Arc::new(MemoryBackend::new())));
            Self {
                dir: tempfile::tempdir().unwrap(),
                records,
            }
        }

        fn put(&self, text: &str) {
            self.records
                .set(ACCOUNT, &Secret::from_string(text))
                .unwrap();
        }

        fn gate_with(&self, pubkey: [u8; 32], app: &str, device: &'static str) -> LicenseGate {
            LicenseGate::with_parts(
                pubkey,
                app,
                self.dir.path(),
                self.records.clone(),
                ACCOUNT,
                Arc::new(move || Ok(device.to_string())),
                SystemClock,
                (Duration::ZERO, Duration::ZERO),
            )
        }

        fn gate(&self) -> LicenseGate {
            self.gate_with(pubkey(), "coderight", DEVICE)
        }
    }

    #[test]
    fn a_verified_pro_record_grants_its_features_lower_cased() {
        let rig = Rig::new();
        rig.put(&active_record(&token(
            "coderight",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        let gate = rig.gate();
        assert!(gate.has_feature("routing"));
        assert!(gate.has_feature(" Schedules "));
        assert!(!gate.has_feature("evolution"));
    }

    #[test]
    fn a_lease_bound_to_the_device_hash_grants_too() {
        let rig = Rig::new();
        let hash = rightkit_device::device_id_hash(DEVICE);
        rig.put(&active_record(&token(
            "coderight",
            "pro",
            &hash,
            FAR_FUTURE,
        )));
        assert!(rig.gate().has_feature("schedules"));
    }

    #[test]
    fn nothing_stored_or_a_tombstone_is_free() {
        let rig = Rig::new();
        assert!(rig.gate().features().is_empty());
        rig.put(TOMBSTONE);
        assert!(rig.gate().features().is_empty());
    }

    #[test]
    fn a_token_for_another_device_app_tier_key_or_time_grants_nothing() {
        let rig = Rig::new();
        let other = token("coderight", "pro", "another-device", FAR_FUTURE);
        rig.put(&active_record(&other));
        assert!(rig.gate().features().is_empty());

        rig.put(&active_record(&token(
            "heardright",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        assert!(rig.gate().features().is_empty());

        rig.put(&active_record(&token(
            "coderight",
            "plus",
            DEVICE,
            FAR_FUTURE,
        )));
        assert!(rig.gate().features().is_empty());

        rig.put(&active_record(&token("coderight", "pro", DEVICE, 2_000)));
        assert!(rig.gate().features().is_empty(), "expired");

        rig.put(&active_record(&token(
            "coderight",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        assert!(
            rig.gate_with([9_u8; 32], "coderight", DEVICE)
                .features()
                .is_empty(),
            "signed by another key"
        );
        assert!(!rig.gate().features().is_empty(), "control: the right key");
    }

    #[test]
    fn a_forged_or_unreadable_record_grants_nothing() {
        let rig = Rig::new();
        for text in [
            active_record("forged.token"),
            "{not json".to_string(),
            "not json".to_string(),
            r#"{"status":"revoked","signedToken":"a.b"}"#.to_string(),
        ] {
            rig.put(&text);
            assert!(rig.gate().features().is_empty(), "{text}");
        }
    }

    #[test]
    fn an_unavailable_device_identity_grants_nothing() {
        let rig = Rig::new();
        rig.put(&active_record(&token(
            "coderight",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        let gate = LicenseGate::with_parts(
            pubkey(),
            "coderight",
            rig.dir.path(),
            rig.records.clone(),
            ACCOUNT,
            Arc::new(|| Err("no device identity".to_string())),
            SystemClock,
            (Duration::ZERO, Duration::ZERO),
        );
        assert!(gate.features().is_empty());
    }

    #[test]
    fn a_new_activation_and_a_deactivation_are_picked_up_without_a_restart() {
        let rig = Rig::new();
        let gate = rig.gate();
        assert!(gate.features().is_empty(), "free at launch");

        rig.put(&active_record(&token(
            "coderight",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        assert!(
            gate.has_feature("routing"),
            "activation reaches the live gate"
        );

        rig.put(TOMBSTONE);
        assert!(
            gate.features().is_empty(),
            "deactivation reaches the live gate"
        );
    }

    #[test]
    fn a_granting_verdict_is_served_for_its_ttl_and_a_refusal_for_its_own() {
        let rig = Rig::new();
        let gate = LicenseGate::with_parts(
            pubkey(),
            "coderight",
            rig.dir.path(),
            rig.records.clone(),
            ACCOUNT,
            Arc::new(|| Ok(DEVICE.to_string())),
            SystemClock,
            (Duration::from_secs(3600), Duration::from_secs(3600)),
        );
        assert!(gate.features().is_empty());
        rig.put(&active_record(&token(
            "coderight",
            "pro",
            DEVICE,
            FAR_FUTURE,
        )));
        assert!(
            gate.features().is_empty(),
            "the refusal is cached for its ttl"
        );
    }

    #[test]
    fn the_locators_need_every_name_and_carry_no_secret() {
        let all = |name: &str| {
            Some(
                match name {
                    ENV_LICENSE_DIR => "/data/license",
                    ENV_LICENSE_SERVICE => "cc.coderight.licence",
                    ENV_LICENSE_ACCOUNT => "license-record",
                    ENV_LICENSE_DEVICE_SERVICE => "cc.coderight.device",
                    ENV_LICENSE_DEVICE_ACCOUNT => "device",
                    _ => return None,
                }
                .to_string(),
            )
        };
        let locators = LicenseLocators::from_lookup(all).expect("complete");
        assert_eq!(locators.license_dir, PathBuf::from("/data/license"));
        assert_eq!(locators.license_service, "cc.coderight.licence");
        assert_eq!(locators.device_account, "device");

        // The legacy handoff names are not locators and are never read.
        let legacy_only = |name: &str| match name {
            "RIGHTKIT_SIGNED_LICENSE_TOKEN" | "RIGHTKIT_LICENSE_DEVICE_ID" => {
                Some("secret".to_string())
            }
            _ => None,
        };
        assert_eq!(LicenseLocators::from_lookup(legacy_only), None);
        // One missing or blank name leaves the daemon on the Free tier.
        let blank_account = |name: &str| {
            if name == ENV_LICENSE_ACCOUNT {
                Some("  ".to_string())
            } else {
                all(name)
            }
        };
        assert_eq!(LicenseLocators::from_lookup(blank_account), None);
    }

    #[test]
    fn the_engine_has_no_environment_path_for_a_token_or_a_feature_list() {
        let source = include_str!("license.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("production part");
        for forbidden in [
            "var(\"RIGHTKIT_SIGNED_LICENSE_TOKEN\")",
            "var(\"RIGHTKIT_LICENSE_DEVICE_ID\")",
            "var(\"RIGHTKIT_LICENSE_FEATURES\")",
            "verify_token(",
            "get_or_create",
        ] {
            assert!(!production.contains(forbidden), "{forbidden}");
        }
    }
}
