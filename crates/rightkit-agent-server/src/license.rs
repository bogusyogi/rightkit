//! Engine-side entitlement gate for the daemon (Pro entitlement).
//!
//! The daemon decides Pro itself, but it never reads the owner's credential store: only the owner
//! binary (the desktop shell's `rightkit-license` client) reads the OS store
//! (`rightkit-credentials` README, "Helpers, sidecars and CLIs"). The owner hands the daemon one
//! snapshot (`licence.token`, `licence.device`, `licence.clock`, see
//! `rightkit_license::SNAPSHOT_NAMES`) and the daemon verifies it again, here, under the suite key
//! and its own app identity. The snapshot is not a verdict: the daemon accepts no verdict, feature
//! list, licence key or raw device id from its caller, not by environment, not by argument.
//!
//! * Delivery: the owner POSTs the snapshot as one `rightkit_credentials::handoff` Reply frame
//!   (octet stream, `RKHO`) to `/internal/license-snapshot`, behind the daemon's per-launch bearer
//!   token (the daemon's existing owner channel), at launch and after every licence change. The
//!   daemon applies it to an [`EngineSnapshotCell`] as a whole replacement and answers the owner's
//!   Ack frame ([`LicenseGate::deliver`]).
//! * Fail closed: until the first delivery the gate is not Pro, and an owner read failure or a
//!   malformed snapshot leaves it not Pro until the next good delivery.
//! * Verification: `EngineEntitlements::from_snapshot` (signature against the suite key, app,
//!   device binding the snapshot names, expiry) over an anti-rollback clock on the owner's ledger
//!   (`<licence dir>/clock.json`), which the daemon only READS (`OwnerLedgerReader`).
//! * Liveness: the verdict is cached for a short TTL and also dropped whenever a delivery arrives
//!   (the cell's generation), so a refresh, revocation, deactivation or new activation needs no
//!   daemon restart.
//!
//! The only launch locator is the licence directory (non-secret, for the ledger). It is read once
//! at process start and removed from the daemon's own environment ([`capture_launch_locators`]);
//! child processes and `${VAR}` expansion additionally refuse any licence-named variable
//! (`rightkit_app_settings::agent_config::licence_env`).

use std::collections::HashSet;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_app_settings::agent_config::licence_env::{is_licence_env_name, ENV_LICENSE_DIR};
use rightkit_credentials::handoff::{
    receive_snapshot_delivery, write_message, Limits, Message, SnapshotDelivery,
};
use rightkit_license::{
    system_now_unix, AntiRollbackClock, ClockRefusal, EngineEntitlements, EngineEntitlementsConfig,
    EngineSnapshotCell, Entitlement, OwnerLedgerReader, ProcessAnchor, TrustedClock,
};

/// The app identity RightKit's suite tokens are signed for.
pub const EXPECTED_APP: &str = "coderight";

/// How long a granting verdict is served before the snapshot is verified again.
const GRANT_TTL: Duration = Duration::from_secs(30);
/// How long a refusal is served.
const DENIAL_TTL: Duration = Duration::from_secs(5);

// ---- launch locators ---------------------------------------------------------------------------

/// Where the owner's anti-rollback ledger lives. Nothing here is secret, and no credential
/// namespace or account is passed: the daemon never opens the owner's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseLocators {
    /// `<app data>/license`: holds the clock ledger the owner writes and the daemon reads.
    pub license_dir: PathBuf,
}

impl LicenseLocators {
    /// The locators named by `lookup`, or `None` unless the licence directory is present and
    /// non-empty.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let dir = lookup(ENV_LICENSE_DIR)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())?;
        Some(Self {
            license_dir: PathBuf::from(dir),
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

/// One anti-rollback clock behind a lock, so the verifier stays `Send`.
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
    /// The cell's generation the verdict was computed under; a later delivery voids it.
    generation: u64,
    /// `exp` of the granting entitlement (a cached grant is never served past it).
    exp: Option<i64>,
    features: HashSet<String>,
}

struct GateState {
    verify: Box<dyn Fn() -> Option<Entitlement> + Send>,
    verdict: Option<Verdict>,
}

/// Why [`LicenseGate::deliver`] kept nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryError {
    /// The bytes are not exactly one handoff Reply frame.
    Malformed,
    /// The Ack could not be encoded.
    Ack,
}

/// The daemon's licence gate: the verified Pro features of the owner's snapshot, live.
pub struct LicenseGate {
    cell: EngineSnapshotCell,
    state: Mutex<GateState>,
    grant_ttl: Duration,
    denial_ttl: Duration,
}

impl LicenseGate {
    /// Open the gate over the owner's ledger. Holds no snapshot yet, so it is not Pro until the
    /// owner delivers one. Opens nothing for writing and no credential store.
    pub fn open(locators: &LicenseLocators) -> Result<Self, String> {
        let cell = EngineSnapshotCell::new();
        let now = system_now_unix().map_err(|error| format!("no wall clock: {error}"))?;
        let clock = AntiRollbackClock::new(
            OwnerLedgerReader::new(&locators.license_dir),
            ProcessAnchor::process(now),
        );
        let config = EngineEntitlementsConfig::suite_snapshot(EXPECTED_APP, &cell);
        Ok(Self::with_config(
            cell,
            config,
            clock,
            (GRANT_TTL, DENIAL_TTL),
        ))
    }

    /// The gate over an explicit configuration (the production wiring above, and tests).
    pub(crate) fn with_config<C>(
        cell: EngineSnapshotCell,
        config: EngineEntitlementsConfig,
        clock: C,
        (grant_ttl, denial_ttl): (Duration, Duration),
    ) -> Self
    where
        C: TrustedClock + Send + 'static,
    {
        // The gate caches; the verifier itself verifies on every call.
        let config = config.with_cache_ttl_secs(0);
        let verifier =
            EngineEntitlements::with_clock(config, SharedClock(Arc::new(Mutex::new(clock))));
        let verify = move || verifier.require_pro().ok();
        Self {
            cell,
            state: Mutex::new(GateState {
                verify: Box::new(verify),
                verdict: None,
            }),
            grant_ttl,
            denial_ttl,
        }
    }

    /// Accept the owner's snapshot: `frame` is exactly one handoff Reply frame. Replaces
    /// everything the gate holds and returns the owner's Ack frame. The caller must have
    /// authenticated the owner (the daemon's bearer token) first. A frame that does not decode
    /// leaves the gate as it was. A snapshot the cell refuses (owner read failure, malformed) is
    /// still acknowledged, and leaves the gate not Pro.
    pub fn deliver(&self, frame: &[u8]) -> Result<Vec<u8>, DeliveryError> {
        let mut cursor = Cursor::new(frame);
        let SnapshotDelivery { id, snapshot } =
            receive_snapshot_delivery(&mut cursor, &Limits::default())
                .map_err(|_| DeliveryError::Malformed)?;
        if cursor.position() as usize != frame.len() {
            return Err(DeliveryError::Malformed);
        }
        if let Err(error) = self.cell.apply(snapshot) {
            eprintln!("[license] licence snapshot refused, running as Free: {error}");
        }
        let mut ack = Vec::new();
        write_message(&mut ack, &Message::ack(id)).map_err(|_| DeliveryError::Ack)?;
        Ok(ack)
    }

    /// The lower-cased feature names of the verified Pro entitlement; empty before the first
    /// delivery and on Free, an unverifiable, expired, rolled-back or unreadable snapshot.
    pub fn features(&self) -> HashSet<String> {
        let Ok(mut state) = self.state.lock() else {
            return HashSet::new();
        };
        let generation = self.cell.generation();
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
            if verdict.generation == generation && verdict.at.elapsed() < ttl && unexpired {
                return verdict.features.clone();
            }
        }
        let entitlement = (state.verify)();
        let verdict = Verdict {
            at: Instant::now(),
            generation,
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
    use rightkit_credentials::handoff::{ErrorCode, Snapshot};
    use rightkit_credentials::Secret;
    use rightkit_license::{
        SystemClock, CLOCK_INITIALIZED, SNAPSHOT_CLOCK, SNAPSHOT_DEVICE, SNAPSHOT_TOKEN,
    };

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

    /// The owner's snapshot: the token and the device it names, or "no licence".
    fn snapshot(token: Option<&str>, device: &str) -> Snapshot {
        let mut snapshot = Snapshot::new();
        match token {
            Some(token) => {
                snapshot
                    .insert_present(SNAPSHOT_TOKEN, Secret::from_string(token))
                    .unwrap();
                snapshot
                    .insert_present(SNAPSHOT_DEVICE, Secret::from_string(device))
                    .unwrap();
                snapshot
                    .insert_present(SNAPSHOT_CLOCK, Secret::from_string(CLOCK_INITIALIZED))
                    .unwrap();
            }
            None => {
                snapshot.insert_absent(SNAPSHOT_TOKEN).unwrap();
                snapshot.insert_absent(SNAPSHOT_DEVICE).unwrap();
                snapshot.insert_absent(SNAPSHOT_CLOCK).unwrap();
            }
        }
        snapshot
    }

    fn frame(id: u64, snapshot: Snapshot) -> Vec<u8> {
        let mut wire = Vec::new();
        write_message(&mut wire, &Message::reply_snapshot(id, snapshot)).unwrap();
        wire
    }

    fn gate_with(pubkey: [u8; 32], app: &str, ttl: (Duration, Duration)) -> LicenseGate {
        let cell = EngineSnapshotCell::new();
        let config = EngineEntitlementsConfig::from_snapshot(pubkey, app, &cell);
        LicenseGate::with_config(cell, config, SystemClock, ttl)
    }

    fn gate_with_key(app: &str) -> LicenseGate {
        gate_with(pubkey(), app, (Duration::ZERO, Duration::ZERO))
    }

    fn deliver(gate: &LicenseGate, token: Option<&str>, device: &str) {
        gate.deliver(&frame(1, snapshot(token, device)))
            .expect("delivery accepted");
    }

    #[test]
    fn a_verified_pro_snapshot_grants_its_features_lower_cased() {
        let gate = gate_with_key("coderight");
        deliver(
            &gate,
            Some(&token("coderight", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(gate.has_feature("routing"));
        assert!(gate.has_feature(" Schedules "));
        assert!(!gate.has_feature("evolution"));
    }

    #[test]
    fn a_lease_bound_to_the_device_hash_grants_too() {
        // The owner names the device the token was issued for: the raw id for a typed key, its
        // hash for a lease. The daemon derives neither.
        let gate = gate_with_key("coderight");
        let hash = "hash-of-the-device";
        deliver(
            &gate,
            Some(&token("coderight", "pro", hash, FAR_FUTURE)),
            hash,
        );
        assert!(gate.has_feature("schedules"));
    }

    #[test]
    fn before_any_delivery_the_gate_is_not_pro() {
        let gate = gate_with_key("coderight");
        assert!(gate.features().is_empty());
    }

    #[test]
    fn no_licence_in_the_snapshot_is_free() {
        let gate = gate_with_key("coderight");
        deliver(
            &gate,
            Some(&token("coderight", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(gate.has_feature("routing"));
        deliver(&gate, None, DEVICE);
        assert!(gate.features().is_empty());
    }

    #[test]
    fn a_token_for_another_device_app_tier_key_or_time_grants_nothing() {
        let gate = gate_with_key("coderight");
        deliver(
            &gate,
            Some(&token("coderight", "pro", "another-device", FAR_FUTURE)),
            DEVICE,
        );
        assert!(gate.features().is_empty(), "device");

        deliver(
            &gate,
            Some(&token("heardright", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(gate.features().is_empty(), "app");

        deliver(
            &gate,
            Some(&token("coderight", "plus", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(gate.features().is_empty(), "tier");

        deliver(
            &gate,
            Some(&token("coderight", "pro", DEVICE, 2_000)),
            DEVICE,
        );
        assert!(gate.features().is_empty(), "expired");

        let other_key = gate_with([9_u8; 32], "coderight", (Duration::ZERO, Duration::ZERO));
        deliver(
            &other_key,
            Some(&token("coderight", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(other_key.features().is_empty(), "signed by another key");

        deliver(
            &gate,
            Some(&token("coderight", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(!gate.features().is_empty(), "control: the right key");
    }

    #[test]
    fn a_forged_token_grants_nothing() {
        let gate = gate_with_key("coderight");
        deliver(&gate, Some("forged.token"), DEVICE);
        assert!(gate.features().is_empty());
    }

    #[test]
    fn an_owner_read_failure_or_a_snapshot_without_a_device_fails_closed() {
        let gate = gate_with_key("coderight");
        let good = token("coderight", "pro", DEVICE, FAR_FUTURE);
        deliver(&gate, Some(&good), DEVICE);
        assert!(gate.has_feature("routing"));

        let mut failed = Snapshot::new();
        failed
            .insert_failed(SNAPSHOT_TOKEN, ErrorCode::StoreError)
            .unwrap();
        gate.deliver(&frame(2, failed)).expect("acknowledged");
        assert!(gate.features().is_empty(), "owner could not read");

        deliver(&gate, Some(&good), DEVICE);
        assert!(gate.has_feature("routing"));
        let mut no_device = Snapshot::new();
        no_device
            .insert_present(SNAPSHOT_TOKEN, Secret::from_string(good))
            .unwrap();
        gate.deliver(&frame(3, no_device)).expect("acknowledged");
        assert!(gate.features().is_empty(), "no device binding");
    }

    #[test]
    fn a_delivery_is_acknowledged_with_its_id_and_a_bad_frame_changes_nothing() {
        let gate = gate_with_key("coderight");
        let good = token("coderight", "pro", DEVICE, FAR_FUTURE);
        let ack = gate
            .deliver(&frame(41, snapshot(Some(&good), DEVICE)))
            .expect("delivery");
        rightkit_credentials::handoff::expect_ack(
            rightkit_credentials::handoff::read_message(&mut ack.as_slice(), &Limits::default())
                .expect("ack frame"),
            41,
        )
        .expect("ack for the delivery id");

        assert_eq!(gate.deliver(b"not a frame"), Err(DeliveryError::Malformed));
        let mut trailing = frame(42, snapshot(None, DEVICE));
        trailing.push(0);
        assert_eq!(gate.deliver(&trailing), Err(DeliveryError::Malformed));
        let request = {
            let mut wire = Vec::new();
            write_message(
                &mut wire,
                &Message::request(5, rightkit_credentials::handoff::Mode::Snapshot, &["x"])
                    .unwrap(),
            )
            .unwrap();
            wire
        };
        assert_eq!(gate.deliver(&request), Err(DeliveryError::Malformed));
        assert!(
            gate.has_feature("routing"),
            "the held snapshot is untouched"
        );
    }

    #[test]
    fn a_granting_verdict_is_served_for_its_ttl_but_never_across_a_delivery() {
        let gate = gate_with(
            pubkey(),
            "coderight",
            (Duration::from_secs(3600), Duration::from_secs(3600)),
        );
        assert!(gate.features().is_empty());
        deliver(
            &gate,
            Some(&token("coderight", "pro", DEVICE, FAR_FUTURE)),
            DEVICE,
        );
        assert!(
            gate.has_feature("routing"),
            "a delivery voids the cached refusal"
        );
        deliver(&gate, None, DEVICE);
        assert!(
            gate.features().is_empty(),
            "a delivery voids the cached grant"
        );
    }

    #[test]
    fn the_locator_is_the_licence_dir_alone_and_carries_no_secret() {
        let all = |name: &str| (name == ENV_LICENSE_DIR).then(|| "/data/license".to_string());
        let locators = LicenseLocators::from_lookup(all).expect("complete");
        assert_eq!(locators.license_dir, PathBuf::from("/data/license"));

        // The legacy handoff names are not locators and are never read.
        let legacy_only = |name: &str| match name {
            "RIGHTKIT_SIGNED_LICENSE_TOKEN" | "RIGHTKIT_LICENSE_DEVICE_ID" => {
                Some("secret".to_string())
            }
            _ => None,
        };
        assert_eq!(LicenseLocators::from_lookup(legacy_only), None);
        let blank = |_: &str| Some("  ".to_string());
        assert_eq!(LicenseLocators::from_lookup(blank), None);
    }

    #[test]
    fn the_engine_never_opens_the_owners_store_or_takes_a_token_from_the_environment() {
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
            "Store::open",
            "DeviceService",
            "FileClockStore",
        ] {
            assert!(!production.contains(forbidden), "{forbidden}");
        }
    }
}
