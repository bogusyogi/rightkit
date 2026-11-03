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
