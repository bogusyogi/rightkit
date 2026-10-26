//! The Right Suite API host contract (`contracts/right-suite-api.v1.json`), owned by RightKit.
//!
//! Always compiled, no dependencies. The constants are generated (`scripts/gen-suite-api.mjs`)
//! and re-exported here; the functions build every URL and host list an app needs from them, so
//! an app never carries the API host or an artifact host of its own.
//!
//! * The update-check host is the current host only. A legacy host is exported for refusal tests
//!   and gates; it is never in an allowlist.
//! * Artifact hosts are per app (`APP_ARTIFACT_HOSTS`); an app not in the contract has none.
//! * App keys match `^[a-z][a-z0-9-]*$`, so a key can never add a path segment or a query.
//!
//! The same contract owns the suite's trust roots, so no app pastes one:
//!
//! * [`LICENSE_PUBLIC_KEY`] verifies every suite licence token; [`app_license_host`] is the brand
//!   host an app declared in the registry ([`registered_apps`]).
//! * [`updater_keys`] lists every minisign key a suite app accepts for updates and
//!   [`UPDATER_SIGNING_KEY`] the one releases are signed with today. [`select_updater_key`] picks
//!   the key that checks a release by the key id inside the release signature, so a rotation is a
//!   change to the list (docs/SUITE-TRUST-ROOTS.md).

#[path = "suite_api_generated.rs"]
mod suite_api_generated;

/// A route template with `{app}` filled in. The client has validated the app key already (its own
/// rule, which also admits `_` and a leading digit), so this only substitutes.
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(crate) fn fill_app(template: &str, app: &str) -> String {
    template.replace("{app}", app)
}

pub use suite_api_generated::{
    ACCOUNT_DELETE_PATH, ACCOUNT_DEVICES_PATH, ACCOUNT_LOGOUT_PATH,
    ACCOUNT_MAGIC_LINK_CONSUME_PATH, ACCOUNT_MAGIC_LINK_REQUEST_PATH, ACCOUNT_ME_PATH,
    ADMIN_PATCHES_PATH, ADMIN_RELEASES_PATH, ADMIN_RUNTIME_ARTIFACTS_PATH, API_HOST, API_ORIGIN,
    APP_ARTIFACT_HOSTS, APP_KEYS, APP_LICENSE_HOSTS, ARTIFACT_PATH_PREFIX, BRIDGE_EXCHANGE_PATH,
    BRIDGE_FROM_ACTIVATION_PATH, BRIDGE_FROM_PLAY_PATH, BRIDGE_FROM_STOREKIT_PATH,
    ENTITLEMENT_PATH, GATED_ARTIFACT_HOST, LEASE_ISSUE_PATH, LEASE_REMOVE_PATH, LEASE_RENEW_PATH,
    LEGACY_HOSTS, LICENSE_ACTIVATE_PATH, LICENSE_DEACTIVATE_PATH, LICENSE_PUBLIC_KEY_HEX,
    LICENSE_REFRESH_PATH, LICENSE_TRIAL_PATH, PENDING_NATIVE_UPDATER, PUBLIC_ARTIFACT_HOST,
    PUBLIC_INSTALLER_BASE, RELEASE_STATUS_PATH, UPDATER_MANIFEST_PATH, UPDATER_PUBLIC_KEYS,
    UPDATER_SIGNING_KEY_ID, WINDOWS_LINK_REDEEM_PATH, WINDOWS_LINK_REQUEST_PATH,
};

use base64::Engine as _;
use std::fmt;

/// Why a Right Suite API value could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SuiteApiError {
    /// The app key is not `^[a-z][a-z0-9-]*$`.
    BadAppKey,
    /// The app key is well formed but the contract does not list the app (or, for artifact hosts,
    /// lists it without any).
    UnknownApp,
    /// The app is in the registry but has not declared a licence host.
    NoLicenseHost,
}

impl fmt::Display for SuiteApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BadAppKey => "the app key is not a valid Right Suite app key",
            Self::UnknownApp => "the app is not in the Right Suite API contract",
            Self::NoLicenseHost => {
                "the app has not declared a licence host in the Right Suite API contract"
            }
        })
    }
}

impl std::error::Error for SuiteApiError {}

/// The one host that serves update metadata (a check), as a list for allowlist constructors.
const CHECK_HOSTS: &[&str] = &[API_HOST];

/// Whether `app` is `^[a-z][a-z0-9-]*$`.
fn valid_app_key(app: &str) -> bool {
    let mut bytes = app.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn checked_key(app: &str) -> Result<&str, SuiteApiError> {
    if valid_app_key(app) {
        Ok(app)
    } else {
        Err(SuiteApiError::BadAppKey)
    }
}

/// The updater feed for `app`: the API origin plus the contract's manifest path with `{app}`
/// filled in. Tauri's `{{target}}` placeholder stays literal; the updater fills it at runtime.
pub fn updater_endpoint(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(format!(
        "{API_ORIGIN}{}",
        UPDATER_MANIFEST_PATH.replace("{app}", app)
    ))
}

/// The release-status URL for `app`. The contract's path has no platform placeholder, so none is
/// added: the answer names the platform itself.
pub fn release_status_url(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(format!(
        "{API_ORIGIN}{}",
        RELEASE_STATUS_PATH.replace("{app}", app)
    ))
}

/// The app's path on its artifact hosts, such as `/mailright`.
pub fn artifact_path_prefix(app: &str) -> Result<String, SuiteApiError> {
    let app = checked_key(app)?;
    Ok(ARTIFACT_PATH_PREFIX.replace("{app}", app))
}

/// The Ed25519 public key that verifies every suite licence token, decoded from the contract's
/// hex at compile time. An app passes this to `LicenseClientConfig::new` or
/// `EngineEntitlementsConfig::new` instead of carrying a copy.
pub const LICENSE_PUBLIC_KEY: [u8; 32] = decode_license_key(LICENSE_PUBLIC_KEY_HEX);

const fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => panic!("the licence public key is not lower-case hex"),
    }
}

const fn decode_license_key(hex: &str) -> [u8; 32] {
    let digits = hex.as_bytes();
    assert!(
        digits.len() == 64,
        "the licence public key is not 64 hex digits"
    );
    let mut key = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        key[i] = (hex_digit(digits[2 * i]) << 4) | hex_digit(digits[2 * i + 1]);
        i += 1;
    }
    key
}

/// Every app in the registry (the suite apps; Ember keeps its own updater key and is not one).
pub fn registered_apps() -> &'static [&'static str] {
    APP_KEYS
}

/// `app` as the registry spells it. `BadAppKey` for a malformed key, `UnknownApp` for one the
/// registry does not list.
pub fn app_registered(app: &str) -> Result<&'static str, SuiteApiError> {
    let app = checked_key(app)?;
    APP_KEYS
        .iter()
        .copied()
        .find(|key| *key == app)
        .ok_or(SuiteApiError::UnknownApp)
}

/// The licence host `app` declared in the registry (a bare brand hostname). An app that is
/// registered without one is `NoLicenseHost`: no default is ever invented for it.
pub fn app_license_host(app: &str) -> Result<&'static str, SuiteApiError> {
    let app = app_registered(app)?;
    APP_LICENSE_HOSTS
        .iter()
        .find(|(key, _)| *key == app)
        .map(|(_, host)| *host)
        .ok_or(SuiteApiError::NoLicenseHost)
}

/// A minisign public key the suite accepts for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdaterKey {
    /// The 16 upper-case hex digit minisign key id.
    pub key_id: &'static str,
    /// The key as `tauri.conf.json` and the updater plugin carry it: base64 of the two-line
    /// minisign public key text.
    pub public_key: &'static str,
    /// The first unix second at which the key is no longer accepted; `None` for a key that does
    /// not expire.
    pub not_after: Option<i64>,
}

impl UpdaterKey {
    /// Whether the key is accepted at `now` (unix seconds). A key with an end date is refused when
    /// the time is unknown, so an unreadable clock never extends a retired key.
    pub fn accepted_at(&self, now: Option<i64>) -> bool {
        match self.not_after {
            None => true,
            Some(end) => now.is_some_and(|now| now < end),
        }
    }
}

const fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn find_signing_key() -> UpdaterKey {
    let mut i = 0;
    while i < UPDATER_PUBLIC_KEYS.len() {
        let (key_id, public_key, not_after) = UPDATER_PUBLIC_KEYS[i];
        if same_text(key_id, UPDATER_SIGNING_KEY_ID) {
            return UpdaterKey {
                key_id,
                public_key,
                not_after,
            };
        }
        i += 1;
    }
    panic!("the contract's updaterSigningKeyId is not one of its updaterPublicKeys")
}

/// The key suite releases are signed with today, and so the key a suite app's updater is
/// configured with. Resolved at compile time: a contract whose signing key id is not listed does
/// not build.
pub const UPDATER_SIGNING_KEY: UpdaterKey = find_signing_key();

/// Every updater key the suite accepts, the signing key included.
pub fn updater_keys() -> Vec<UpdaterKey> {
    UPDATER_PUBLIC_KEYS
        .iter()
        .map(|&(key_id, public_key, not_after)| UpdaterKey {
            key_id,
            public_key,
            not_after,
        })
        .collect()
}

/// Why no updater key could be chosen for a release signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UpdaterKeyError {
    /// The signature is not a minisign signature (or a minisign key is not a minisign key).
    Malformed,
    /// The signature names a key id none of the accepted keys has.
    UnknownKey,
    /// The signature names an accepted key whose end date has passed.
    KeyExpired,
}

impl fmt::Display for UpdaterKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "the signature is not a minisign signature",
            Self::UnknownKey => "the signature was made by a key the suite does not accept",
            Self::KeyExpired => "the signature was made by a key the suite no longer accepts",
        })
    }
}

impl std::error::Error for UpdaterKeyError {}

fn key_id_text(raw: &[u8]) -> String {
    // The id is stored little endian; the usual (and the contract's) text is the big endian hex.
    let mut id = [0u8; 8];
    id.copy_from_slice(&raw[2..10]);
    format!("{:016X}", u64::from_le_bytes(id))
}

// ASCII whitespace only, as the TypeScript and JavaScript readers trim: str::trim would also strip U+0085, String.trim U+FEFF.
fn decode_text(base64: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64.trim_ascii())
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// The key id a release signature names. `signature` is the text a Tauri `latest.json` carries
/// (base64 of the minisign `.sig` file). Only the key id is read: the updater plugin verifies the
/// signature itself, with the key this id selects, so a forged id can only make a download fail.
pub fn signature_key_id(signature: &str) -> Result<String, UpdaterKeyError> {
    let text = decode_text(signature).ok_or(UpdaterKeyError::Malformed)?;
    let line = text.lines().nth(1).ok_or(UpdaterKeyError::Malformed)?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(line.trim_ascii())
        .map_err(|_| UpdaterKeyError::Malformed)?;
    if raw.len() != 74 || raw[0] != b'E' || !matches!(raw[1], b'd' | b'D') {
        return Err(UpdaterKeyError::Malformed);
    }
    Ok(key_id_text(&raw))
}

/// The key id of a minisign public key written as `tauri.conf.json` carries it.
pub fn updater_public_key_id(public_key: &str) -> Result<String, UpdaterKeyError> {
    let text = decode_text(public_key).ok_or(UpdaterKeyError::Malformed)?;
    let mut lines = text.lines();
    let comment = lines.next().ok_or(UpdaterKeyError::Malformed)?;
    let line = lines.next().ok_or(UpdaterKeyError::Malformed)?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(line.trim_ascii())
        .map_err(|_| UpdaterKeyError::Malformed)?;
    if raw.len() != 42 || raw[0] != b'E' || raw[1] != b'd' {
        return Err(UpdaterKeyError::Malformed);
    }
    let id = key_id_text(&raw);
    // The comment is only a label, but a key whose label disagrees with its id was edited.
    if comment.strip_prefix("untrusted comment: minisign public key: ") != Some(id.as_str()) {
        return Err(UpdaterKeyError::Malformed);
    }
    Ok(id)
}

/// The key of `keys` that checks `signature`: the one whose key id the signature names. A key id
/// nothing lists is `UnknownKey`; a listed key past its end date at `now` is `KeyExpired`.
pub fn select_updater_key(
    keys: &[UpdaterKey],
    signature: &str,
    now: Option<i64>,
) -> Result<UpdaterKey, UpdaterKeyError> {
    let id = signature_key_id(signature)?;
    let key = keys
        .iter()
        .find(|key| key.key_id == id)
        .ok_or(UpdaterKeyError::UnknownKey)?;
    if key.accepted_at(now) {
        Ok(*key)
    } else {
        Err(UpdaterKeyError::KeyExpired)
    }
}

/// [`select_updater_key`] over the contract's key list.
pub fn updater_key_for_signature(
    signature: &str,
    now: Option<i64>,
) -> Result<UpdaterKey, UpdaterKeyError> {
    select_updater_key(&updater_keys(), signature, now)
}

/// The hosts that serve update metadata: the current API host only, never a legacy host.
pub fn update_check_hosts() -> &'static [&'static str] {
    CHECK_HOSTS
}

/// The hosts that may serve `app`'s artifacts. An app the contract does not list is an error, so
/// no app inherits another app's hosts.
pub fn artifact_hosts(app: &str) -> Result<Vec<&'static str>, SuiteApiError> {
    let app = checked_key(app)?;
    APP_ARTIFACT_HOSTS
        .iter()
        .find(|(key, _)| *key == app)
        .map(|(_, hosts)| hosts.to_vec())
        .ok_or(SuiteApiError::UnknownApp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTOR: &str = include_str!("../test-vectors/right-suite-api.v1.json");

    fn contract() -> Value {
        serde_json::from_str(VECTOR).expect("the vendored contract is JSON")
    }

    fn text<'a>(value: &'a Value, path: &[&str]) -> &'a str {
        let mut at = value;
        for key in path {
            at = &at[*key];
        }
        at.as_str()
            .unwrap_or_else(|| panic!("{path:?} is a string"))
    }

    #[test]
    fn the_constants_equal_the_vendored_contract() {
        let c = contract();
        assert_eq!(text(&c, &["schema"]), "right-suite-api.v1");
        assert_eq!(API_HOST, text(&c, &["host"]));
        assert_eq!(API_ORIGIN, text(&c, &["origin"]));
        assert_eq!(API_ORIGIN, format!("https://{API_HOST}"));
        let legacy: Vec<&str> = c["legacyHosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(LEGACY_HOSTS, legacy.as_slice());
        assert_eq!(
            UPDATER_MANIFEST_PATH,
            text(&c, &["paths", "updaterManifest"])
        );
        assert_eq!(RELEASE_STATUS_PATH, text(&c, &["paths", "releaseStatus"]));
        assert_eq!(ADMIN_PATCHES_PATH, text(&c, &["paths", "adminPatches"]));
        assert_eq!(ADMIN_RELEASES_PATH, text(&c, &["paths", "adminReleases"]));
        assert_eq!(
            ADMIN_RUNTIME_ARTIFACTS_PATH,
            text(&c, &["paths", "adminRuntimeArtifacts"])
        );
        assert_eq!(
            GATED_ARTIFACT_HOST,
            text(&c, &["artifactHostSets", "gated"])
        );
        assert_eq!(
            PUBLIC_ARTIFACT_HOST,
            text(&c, &["artifactHostSets", "public"])
        );
        assert_eq!(PUBLIC_INSTALLER_BASE, text(&c, &["publicInstallerBase"]));
        assert_eq!(ARTIFACT_PATH_PREFIX, text(&c, &["artifactPathPrefix"]));

        let apps = c["apps"].as_object().unwrap();
        let with_hosts = apps
            .values()
            .filter(|app| app.get("artifactHosts").is_some())
            .count();
        assert_eq!(APP_ARTIFACT_HOSTS.len(), with_hosts);
        for (key, hosts) in APP_ARTIFACT_HOSTS {
            let expected: Vec<&str> = apps[*key]["artifactHosts"]
                .as_array()
                .unwrap_or_else(|| panic!("{key} is in the contract"))
                .iter()
                .map(|set| text(&c, &["artifactHostSets", set.as_str().unwrap()]))
                .collect();
            assert_eq!(*hosts, expected.as_slice(), "{key}");
        }
    }

    #[test]
    fn the_updater_endpoint_is_exact_and_keeps_the_runtime_placeholder() {
        assert_eq!(
            updater_endpoint("mailright").unwrap(),
            "https://api.orthiclabs.com/v1/apps/mailright/releases/latest.json?platform={{target}}"
        );
        assert_eq!(
            release_status_url("viewright").unwrap(),
            "https://api.orthiclabs.com/v1/apps/viewright/releases/status.json"
        );
        assert_eq!(artifact_path_prefix("coderight").unwrap(), "/coderight");
    }

    #[test]
    fn a_malformed_app_key_builds_nothing() {
        for bad in [
            "",
            "Mail",
            "a/b",
            "../x",
            "1app",
            "-app",
            "app_x",
            "a b",
            "a?b",
            "a#b",
            "app\n",
            "é",
            "a.b",
            "mailright/",
        ] {
            assert_eq!(
                updater_endpoint(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                release_status_url(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                artifact_path_prefix(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                artifact_hosts(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
        }
        for ok in ["a", "mailright", "app-2", "a-b-c"] {
            assert!(updater_endpoint(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn the_check_host_is_the_current_host_and_never_a_legacy_one() {
        assert_eq!(update_check_hosts(), &[API_HOST]);
        for legacy in LEGACY_HOSTS {
            assert!(!update_check_hosts().contains(legacy));
            for (app, hosts) in APP_ARTIFACT_HOSTS {
                assert!(!hosts.contains(legacy), "{app}");
            }
        }
    }

    #[test]
    fn artifact_hosts_are_per_app_and_an_unlisted_app_has_none() {
        let viewright = artifact_hosts("viewright").unwrap();
        assert_eq!(viewright, vec![PUBLIC_ARTIFACT_HOST]);
        assert!(!viewright.contains(&GATED_ARTIFACT_HOST));
        assert_eq!(
            artifact_hosts("mailright").unwrap(),
            vec![GATED_ARTIFACT_HOST, PUBLIC_ARTIFACT_HOST]
        );
        assert_eq!(artifact_hosts("nosuchapp"), Err(SuiteApiError::UnknownApp));
        for (app, hosts) in APP_ARTIFACT_HOSTS {
            assert!(
                !hosts.contains(&API_HOST),
                "{app}: the API host is not an artifact host"
            );
        }
    }

    #[test]
    fn the_client_paths_equal_the_vendored_contract() {
        let c = contract();
        let paths = &c["clientPaths"];
        for (constant, key) in [
            (LICENSE_ACTIVATE_PATH, "licenseActivate"),
            (LICENSE_REFRESH_PATH, "licenseRefresh"),
            (LICENSE_DEACTIVATE_PATH, "licenseDeactivate"),
            (LICENSE_TRIAL_PATH, "licenseTrial"),
            (BRIDGE_FROM_ACTIVATION_PATH, "bridgeFromActivation"),
            (BRIDGE_EXCHANGE_PATH, "bridgeExchange"),
            (BRIDGE_FROM_PLAY_PATH, "bridgeFromPlay"),
            (BRIDGE_FROM_STOREKIT_PATH, "bridgeFromStorekit"),
            (LEASE_ISSUE_PATH, "leaseIssue"),
            (LEASE_RENEW_PATH, "leaseRenew"),
            (LEASE_REMOVE_PATH, "leaseRemove"),
            (WINDOWS_LINK_REQUEST_PATH, "windowsLinkRequest"),
            (WINDOWS_LINK_REDEEM_PATH, "windowsLinkRedeem"),
            (ACCOUNT_MAGIC_LINK_REQUEST_PATH, "accountMagicLinkRequest"),
            (ACCOUNT_MAGIC_LINK_CONSUME_PATH, "accountMagicLinkConsume"),
            (ACCOUNT_ME_PATH, "accountMe"),
            (ACCOUNT_LOGOUT_PATH, "accountLogout"),
            (ACCOUNT_DEVICES_PATH, "accountDevices"),
            (ACCOUNT_DELETE_PATH, "accountDelete"),
            (ENTITLEMENT_PATH, "entitlement"),
        ] {
            assert_eq!(constant, text(paths, &[key]), "{key}");
        }
        assert_eq!(
            paths.as_object().unwrap().len(),
            20,
            "no client path is left out of this list"
        );
        assert_eq!(
            fill_app(LEASE_RENEW_PATH, "heardright"),
            "/v1/pro/heardright/devices/lease/renew"
        );
    }

    // ---- trust roots -----------------------------------------------------------------------

    const KEY_B: &str = "0123456789ABCDEF";

    fn std_b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// A minisign public key text (as `tauri.conf.json` carries it) for a made-up key id.
    fn fixture_public_key(key_id: &str) -> String {
        let mut raw = vec![b'E', b'd'];
        raw.extend(hex_bytes(key_id).iter().rev());
        raw.extend([7u8; 32]);
        std_b64(
            format!(
                "untrusted comment: minisign public key: {key_id}\n{}\n",
                std_b64(&raw)
            )
            .as_bytes(),
        )
    }

    /// A release signature (as `latest.json` carries it) made by a made-up key id.
    fn fixture_signature(key_id: &str, algorithm: u8) -> String {
        let mut raw = vec![b'E', algorithm];
        raw.extend(hex_bytes(key_id).iter().rev());
        raw.extend([0u8; 64]);
        std_b64(
            format!(
                "untrusted comment: signature from tauri secret key\n{}\ntrusted comment: timestamp:1\n{}\n",
                std_b64(&raw),
                std_b64(&[0u8; 64])
            )
            .as_bytes(),
        )
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    fn fixture_keys() -> Vec<UpdaterKey> {
        let leak = |text: String| -> &'static str { Box::leak(text.into_boxed_str()) };
        vec![
            UpdaterKey {
                key_id: "29395F9FF466261D",
                public_key: leak(fixture_public_key("29395F9FF466261D")),
                not_after: Some(2_000),
            },
            UpdaterKey {
                key_id: KEY_B,
                public_key: leak(fixture_public_key(KEY_B)),
                not_after: None,
            },
        ]
    }

    #[test]
    fn suite_key_matches_contract() {
        let c = contract();
        let hex: String = LICENSE_PUBLIC_KEY
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, text(&c, &["licensePublicKeyHex"]));
        assert_eq!(LICENSE_PUBLIC_KEY_HEX, text(&c, &["licensePublicKeyHex"]));
        assert_ne!(LICENSE_PUBLIC_KEY, [0u8; 32]);
        assert_eq!(UPDATER_SIGNING_KEY_ID, text(&c, &["updaterSigningKeyId"]));
        assert_eq!(UPDATER_SIGNING_KEY.key_id, UPDATER_SIGNING_KEY_ID);
        assert_eq!(UPDATER_SIGNING_KEY.not_after, None);
        let listed = c["updaterPublicKeys"].as_array().unwrap();
        assert_eq!(UPDATER_PUBLIC_KEYS.len(), listed.len());
        for (&(kid, key, not_after), entry) in UPDATER_PUBLIC_KEYS.iter().zip(listed) {
            assert_eq!(kid, text(entry, &["kid"]));
            assert_eq!(key, text(entry, &["publicKey"]));
            assert_eq!(not_after.is_some(), entry.get("notAfter").is_some());
            assert_eq!(updater_public_key_id(key).as_deref(), Ok(kid));
        }
        assert_eq!(UPDATER_SIGNING_KEY.public_key, updater_keys()[0].public_key);
    }

    #[test]
    fn the_licence_key_decoder_is_exact() {
        let hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let key = decode_license_key(hex);
        assert_eq!(key.to_vec(), (0u8..32).collect::<Vec<_>>());
        assert_eq!(decode_license_key(&hex.replace("1f", "ff"))[31], 0xff);
    }

    #[test]
    #[should_panic(expected = "lower-case hex")]
    fn the_licence_key_decoder_refuses_upper_case_and_text_that_is_not_hex() {
        decode_license_key(&"AB".repeat(32));
    }

    #[test]
    #[should_panic(expected = "64 hex digits")]
    fn the_licence_key_decoder_refuses_a_short_key() {
        decode_license_key("00ff");
    }

    #[test]
    fn the_registry_is_the_contract_and_a_missing_host_is_never_invented() {
        let c = contract();
        let apps = c["apps"].as_object().unwrap();
        let mut listed: Vec<&str> = apps.keys().map(String::as_str).collect();
        listed.sort_unstable();
        assert_eq!(registered_apps(), listed.as_slice());
        let mut pending: Vec<&str> = c["pendingNativeUpdater"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        pending.sort_unstable();
        assert_eq!(PENDING_NATIVE_UPDATER, pending.as_slice());
        for (app, entry) in apps {
            assert_eq!(
                app_registered(app),
                Ok(registered_apps()[registered_apps().iter().position(|k| k == app).unwrap()])
            );
            match entry.get("licenseHost") {
                Some(host) => {
                    assert_eq!(app_license_host(app), Ok(host.as_str().unwrap()), "{app}")
                }
                None => assert_eq!(
                    app_license_host(app),
                    Err(SuiteApiError::NoLicenseHost),
                    "{app}"
                ),
            }
        }
        assert_eq!(app_license_host("coderight"), Ok("coderight.cc"));
        assert_eq!(
            app_license_host("nosuchapp"),
            Err(SuiteApiError::UnknownApp)
        );
        assert_eq!(app_registered("ember"), Err(SuiteApiError::UnknownApp));
        for bad in ["", "Mail", "a/b", "../x", "app\n"] {
            assert_eq!(
                app_registered(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
            assert_eq!(
                app_license_host(bad),
                Err(SuiteApiError::BadAppKey),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_signature_names_its_key_and_anything_else_is_malformed() {
        assert_eq!(
            signature_key_id(&fixture_signature("29395F9FF466261D", b'd')).as_deref(),
            Ok("29395F9FF466261D")
        );
        // The prehashed algorithm (what current minisign writes) names its key the same way.
        assert_eq!(
            signature_key_id(&fixture_signature(KEY_B, b'D')).as_deref(),
            Ok(KEY_B)
        );
        let good = fixture_signature(KEY_B, b'd');
        let one_line = std_b64(b"untrusted comment: x\n");
        let wrong_algorithm = fixture_signature(KEY_B, b'X');
        let mut short = vec![b'E', b'd'];
        short.extend([1u8; 9]);
        let short = std_b64(format!("untrusted comment: x\n{}\n", std_b64(&short)).as_bytes());
        for bad in [
            "",
            "not base64!",
            &good[..good.len() / 8 * 4],
            one_line.as_str(),
            wrong_algorithm.as_str(),
            short.as_str(),
        ] {
            assert_eq!(
                signature_key_id(bad),
                Err(UpdaterKeyError::Malformed),
                "{bad:?}"
            );
        }
    }

    /// The shared vectors (`contracts/updater-signature-vectors.v1.json`, byte-copied beside this crate) that the
    /// TypeScript and release-tooling readers of a signature's key id are held to as well.
    const SIGNATURE_VECTORS: &str =
        include_str!("../test-vectors/updater-signature-vectors.v1.json");

    #[test]
    fn signature_key_ids_match_the_shared_vectors() {
        let vectors: Value = serde_json::from_str(SIGNATURE_VECTORS).unwrap();
        assert_eq!(text(&vectors, &["schema"]), "updater-signature-key-id.v1");
        let cases = vectors["cases"].as_array().unwrap();
        assert!(cases.len() >= 15, "the vectors are not silently emptied");
        for case in cases {
            let signature = text(case, &["signature"]);
            let expected = case["keyId"].as_str().map(str::to_owned);
            assert_eq!(
                signature_key_id(signature).ok(),
                expected,
                "{}",
                text(case, &["name"])
            );
        }
    }

    #[test]
    fn a_public_key_text_must_agree_with_its_own_key_id() {
        assert_eq!(
            updater_public_key_id(&fixture_public_key(KEY_B)).as_deref(),
            Ok(KEY_B)
        );
        let relabelled = std_b64(
            String::from_utf8(
                base64::engine::general_purpose::STANDARD
                    .decode(fixture_public_key(KEY_B))
                    .unwrap(),
            )
            .unwrap()
            .replace(KEY_B, "FFFFFFFFFFFFFFFF")
            .as_bytes(),
        );
        for bad in [
            "",
            "PLACEHOLDER-REPLACE-WITH-SUITE-UPDATER-PUBKEY",
            relabelled.as_str(),
        ] {
            assert_eq!(
                updater_public_key_id(bad),
                Err(UpdaterKeyError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_key_that_checks_a_release_is_the_one_its_signature_names() {
        let keys = fixture_keys();
        let old = fixture_signature("29395F9FF466261D", b'D');
        let rotated = fixture_signature(KEY_B, b'D');
        // Before the old key's end date both releases are accepted, each by its own key.
        assert_eq!(
            select_updater_key(&keys, &old, Some(1_999)).unwrap().key_id,
            "29395F9FF466261D"
        );
        assert_eq!(
            select_updater_key(&keys, &rotated, Some(1_999))
                .unwrap()
                .key_id,
            KEY_B
        );
        // From the end date on the old key is refused, the rotated key still is not.
        assert_eq!(
            select_updater_key(&keys, &old, Some(2_000)),
            Err(UpdaterKeyError::KeyExpired)
        );
        assert_eq!(
            select_updater_key(&keys, &rotated, Some(5_000_000_000))
                .unwrap()
                .key_id,
            KEY_B
        );
        // An unreadable clock never extends a key that has an end date, and never hurts one without.
        assert_eq!(
            select_updater_key(&keys, &old, None),
            Err(UpdaterKeyError::KeyExpired)
        );
        assert_eq!(
            select_updater_key(&keys, &rotated, None).unwrap().key_id,
            KEY_B
        );
        // A key nothing lists is refused, and so is text that is not a signature.
        assert_eq!(
            select_updater_key(&keys, &fixture_signature("FEDCBA9876543210", b'D'), Some(0)),
            Err(UpdaterKeyError::UnknownKey)
        );
        assert_eq!(
            select_updater_key(&keys, "garbage", Some(0)),
            Err(UpdaterKeyError::Malformed)
        );
        assert_eq!(
            select_updater_key(&[], &old, Some(0)),
            Err(UpdaterKeyError::UnknownKey)
        );
    }

    #[test]
    fn the_contract_list_accepts_a_release_signed_by_the_signing_key_and_refuses_a_stranger() {
        let signed = fixture_signature(UPDATER_SIGNING_KEY_ID, b'D');
        assert_eq!(
            updater_key_for_signature(&signed, Some(0)),
            Ok(UPDATER_SIGNING_KEY)
        );
        assert_eq!(
            updater_key_for_signature(&fixture_signature(KEY_B, b'D'), Some(0)),
            Err(UpdaterKeyError::UnknownKey)
        );
    }
}
