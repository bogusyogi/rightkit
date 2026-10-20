//! The stored licence: the envelope, and the record inside it.
//!
//! The whole licence is ONE credential item whose text is
//! `{"rkv":1,"revision":"<random 128-bit>","state":"active"|"tombstone","record":{...}}`.
//! - `revision` is fresh random text on every write, so two writes are never confused for one
//!   another and a reader can tell a late or stale write by it. The revision value never leaves
//!   this crate (a view carries only [`RevisionState`](super::view::RevisionState)).
//! - A `tombstone` is a deactivated licence: no `record`, and recovery never turns it back into
//!   an active one.
//! - An older bare `@rightkit/license` record (no `rkv`) is read as an active record with no
//!   revision.
//!
//! Token-bearing values are `Zeroizing` and have a redacted `Debug`. No type here implements
//! `Serialize`; the item text is built by hand in [`Envelope::encode`].

use std::fmt;

use rightkit_credentials::{Secret, Store};
use serde_json::{json, Map, Value};
use zeroize::Zeroizing;

use super::time::parse_rfc3339;
use super::view::LicenseError;
use crate::record_text::{ENVELOPE_KEY, ENVELOPE_VERSION};

/// The stored text is not a record this crate understands. Never "no licence".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Corrupt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordState {
    Active,
    Tombstone,
}

/// The server's standing of the licence as last stored. Anything unrecognised is `Invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordStatus {
    Active,
    /// The server said the licence is no longer active (`revoked`, `removed`).
    Revoked,
    /// Unknown, failed or invalid; grants nothing.
    Invalid,
}

impl RecordStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Invalid => "invalid",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "active" => Self::Active,
            "revoked" | "removed" => Self::Revoked,
            _ => Self::Invalid,
        }
    }
}

/// A device lease persisted inside the record.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LeaseRecord {
    pub lease_id: String,
    pub generation: u64,
    /// `expiresAt` in Unix seconds.
    pub expires_at: i64,
    pub grace_seconds: i64,
    pub device_id_hash: Option<String>,
    pub pool: Option<String>,
    pub issued_at: Option<String>,
    pub expires_at_text: Option<String>,
    pub signature: Option<String>,
}

impl fmt::Debug for LeaseRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseRecord")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// The licence record. Holds the native-only credentials.
#[derive(Clone)]
pub(crate) struct StoredRecord {
    pub status: RecordStatus,
    /// The id the signed token is bound to: the raw device id for a typed-key activation, the
    /// device-id hash for a lease.
    pub device_id: String,
    pub activation_token: Option<Zeroizing<String>>,
    pub signed_token: Option<Zeroizing<String>>,
    pub license_id: Option<String>,
    /// The anti-rollback ledger was started for this install; a missing ledger is then tampering.
    pub clock_initialized: bool,
    /// First failed refresh of the current offline episode, Unix seconds.
    pub stale_since: Option<i64>,
    pub activated_at: Option<i64>,
    pub last_refreshed_at: Option<i64>,
    pub lease: Option<LeaseRecord>,
}

impl fmt::Debug for StoredRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredRecord")
            .field("status", &self.status)
            .field("has_signed_token", &self.signed_token.is_some())
            .field("has_activation_token", &self.activation_token.is_some())
            .finish_non_exhaustive()
    }
}

impl StoredRecord {
    /// An empty active record for `device_id`.
    pub(crate) fn new(device_id: String) -> Self {
        Self {
            status: RecordStatus::Active,
            device_id,
            activation_token: None,
            signed_token: None,
            license_id: None,
            clock_initialized: false,
            stale_since: None,
            activated_at: None,
            last_refreshed_at: None,
            lease: None,
        }
    }

    pub(crate) fn signed_token(&self) -> Option<&str> {
        self.signed_token.as_deref().map(String::as_str)
    }

    pub(crate) fn activation_token(&self) -> Option<&str> {
        self.activation_token.as_deref().map(String::as_str)
    }

    fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("status".into(), json!(self.status.as_str()));
        map.insert("deviceId".into(), json!(self.device_id));
        if let Some(token) = &self.activation_token {
            map.insert("activationToken".into(), json!(token.as_str()));
        }
        if let Some(token) = &self.signed_token {
            map.insert("signedToken".into(), json!(token.as_str()));
        }
        if let Some(id) = &self.license_id {
            map.insert("licenseId".into(), json!(id));
        }
        map.insert("clockInitialized".into(), json!(self.clock_initialized));
        for (name, value) in [
            ("staleSince", self.stale_since),
            ("activatedAt", self.activated_at),
            ("lastRefreshedAt", self.last_refreshed_at),
        ] {
            if let Some(value) = value {
                map.insert(name.into(), json!(value));
            }
        }
        if let Some(lease) = &self.lease {
            let mut inner = Map::new();
            inner.insert("leaseId".into(), json!(lease.lease_id));
            inner.insert("generation".into(), json!(lease.generation));
            inner.insert("expiresAtUnix".into(), json!(lease.expires_at));
            inner.insert("graceSeconds".into(), json!(lease.grace_seconds));
            for (name, value) in [
                ("deviceIdHash", &lease.device_id_hash),
                ("pool", &lease.pool),
                ("issuedAt", &lease.issued_at),
                ("expiresAt", &lease.expires_at_text),
                ("signature", &lease.signature),
            ] {
                if let Some(value) = value {
                    inner.insert(name.into(), json!(value));
                }
            }
            map.insert("lease".into(), Value::Object(inner));
        }
        Value::Object(map)
    }

    /// Read a record object: this crate's own form, or the bare `@rightkit/license` /
    /// HeardRight form (camelCase or snake_case names; ISO or numeric times).
    pub(crate) fn from_map(map: &Map<String, Value>) -> Self {
        let status = get_str(map, &["status"])
            .map(|s| RecordStatus::parse(&s))
            // A record with no status at all is a bare record of the older form: active.
            .unwrap_or(RecordStatus::Active);
        let lease = lease_from_map(map);
        Self {
            status,
            device_id: get_str(map, &["deviceId", "device_id"]).unwrap_or_default(),
            activation_token: get_str(map, &["activationToken", "activation_token"])
                .map(Zeroizing::new),
            signed_token: get_str(map, &["signedToken", "signed_token"]).map(Zeroizing::new),
            license_id: get_str(map, &["licenseId", "license_id"]),
            clock_initialized: get_bool(
                map,
                &[
                    "clockInitialized",
                    "license_clock_initialized",
                    "licenseClockInitialized",
                ],
            ),
            stale_since: get_time(map, &["staleSince", "stale_since"]),
            activated_at: get_time(map, &["activatedAt", "activated_at"]),
            last_refreshed_at: get_time(map, &["lastRefreshedAt", "last_refreshed_at"]),
            lease,
        }
    }
}

fn lease_from_map(map: &Map<String, Value>) -> Option<LeaseRecord> {
    // This crate's form: a nested `lease` object.
    if let Some(inner) = map.get("lease").and_then(Value::as_object) {
        return Some(LeaseRecord {
            lease_id: get_str(inner, &["leaseId"])?,
            generation: inner.get("generation").and_then(Value::as_u64)?,
            expires_at: get_time(inner, &["expiresAtUnix"])
                .or_else(|| get_time(inner, &["expiresAt"]))?,
            grace_seconds: inner
                .get("graceSeconds")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            device_id_hash: get_str(inner, &["deviceIdHash"]),
            pool: get_str(inner, &["pool"]),
            issued_at: get_str(inner, &["issuedAt"]),
            expires_at_text: get_str(inner, &["expiresAt"]),
            signature: get_str(inner, &["signature"]),
        });
    }
    // HeardRight's flat form: lease_id, lease_generation, lease_expires_at_unix, ...
    let lease_id = get_str(map, &["lease_id", "leaseId"])?;
    Some(LeaseRecord {
        lease_id,
        generation: map
            .get("lease_generation")
            .or_else(|| map.get("leaseGeneration"))
            .and_then(Value::as_u64)?,
        expires_at: get_time(map, &["lease_expires_at_unix", "leaseExpiresAtUnix"])?,
        grace_seconds: map
            .get("lease_grace_seconds")
            .or_else(|| map.get("leaseGraceSeconds"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        device_id_hash: None,
        pool: None,
        issued_at: None,
        expires_at_text: None,
        signature: None,
    })
}

fn get<'a>(map: &'a Map<String, Value>, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .filter(|value| !value.is_null())
}

fn get_str(map: &Map<String, Value>, names: &[&str]) -> Option<String> {
    get(map, names)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn get_bool(map: &Map<String, Value>, names: &[&str]) -> bool {
    get(map, names).and_then(Value::as_bool).unwrap_or(false)
}

/// A time as Unix seconds: a number, or an RFC 3339 / digits string.
fn get_time(map: &Map<String, Value>, names: &[&str]) -> Option<i64> {
    match get(map, names)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => parse_rfc3339(text).or_else(|| text.trim().parse().ok()),
        _ => None,
    }
}

/// The stored item: a revision, a state and (when active) a record.
#[derive(Clone)]
pub(crate) struct Envelope {
    /// `None` for an older bare record.
    pub revision: Option<String>,
    pub state: RecordState,
    pub record: Option<StoredRecord>,
}

impl fmt::Debug for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Envelope")
            .field("has_revision", &self.revision.is_some())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// A fresh random 128-bit revision (OS CSPRNG, base64 text).
pub(crate) fn new_revision() -> Result<String, LicenseError> {
    let secret = Store::generate_key(16).map_err(|_| LicenseError::Store)?;
    secret
        .expose_str()
        .map(str::to_owned)
        .map_err(|_| LicenseError::Store)
}

impl Envelope {
    /// An active envelope with a fresh revision.
    pub(crate) fn active(record: StoredRecord) -> Result<Self, LicenseError> {
        Ok(Self {
            revision: Some(new_revision()?),
            state: RecordState::Active,
            record: Some(record),
        })
    }

    /// The deactivation tombstone, with a fresh revision.
    pub(crate) fn tombstone() -> Result<Self, LicenseError> {
        Ok(Self {
            revision: Some(new_revision()?),
            state: RecordState::Tombstone,
            record: None,
        })
    }

    /// Read the stored text: an envelope, or a bare record (active, no revision).
    pub(crate) fn parse(text: &str) -> Result<Self, Corrupt> {
        let value: Value = serde_json::from_str(text).map_err(|_| Corrupt)?;
        let top = value.as_object().ok_or(Corrupt)?;
        if !top.contains_key(ENVELOPE_KEY) {
            return Ok(Self {
                revision: None,
                state: RecordState::Active,
                record: Some(StoredRecord::from_map(top)),
            });
        }
        if top.get(ENVELOPE_KEY).and_then(Value::as_u64) != Some(ENVELOPE_VERSION) {
            return Err(Corrupt);
        }
        let revision = get_str(top, &["revision"]).ok_or(Corrupt)?;
        match top.get("state").and_then(Value::as_str) {
            Some("tombstone") => Ok(Self {
                revision: Some(revision),
                state: RecordState::Tombstone,
                record: None,
            }),
            Some("active") => {
                let record = top
                    .get("record")
                    .and_then(Value::as_object)
                    .ok_or(Corrupt)?;
                Ok(Self {
                    revision: Some(revision),
                    state: RecordState::Active,
                    record: Some(StoredRecord::from_map(record)),
                })
            }
            // An unknown state is never read as active.
            _ => Err(Corrupt),
        }
    }

    /// The item text. Only an envelope with a revision is written; a bare record is never
    /// re-written in its old form.
    pub(crate) fn encode(&self) -> Secret {
        let mut top = Map::new();
        top.insert(ENVELOPE_KEY.into(), json!(ENVELOPE_VERSION));
        if let Some(revision) = &self.revision {
            top.insert("revision".into(), json!(revision));
        }
        match (self.state, &self.record) {
            (RecordState::Active, Some(record)) => {
                top.insert("state".into(), json!("active"));
                top.insert("record".into(), record.to_value());
            }
            _ => {
                top.insert("state".into(), json!("tombstone"));
            }
        }
        // The text is handed straight to a zeroizing secret.
        let text = Zeroizing::new(Value::Object(top).to_string());
        Secret::from_string(text.as_str())
    }

    pub(crate) fn is_tombstone(&self) -> bool {
        self.state == RecordState::Tombstone
    }
}

/// Parse a credential item into an envelope.
pub(crate) fn parse_secret(secret: &Secret) -> Result<Envelope, Corrupt> {
    let text = secret.expose_str().map_err(|_| Corrupt)?;
    Envelope::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> StoredRecord {
        let mut r = StoredRecord::new("dev-1".into());
        r.activation_token = Some(Zeroizing::new("SENTINEL_ACT".into()));
        r.signed_token = Some(Zeroizing::new("SENTINEL_SIGNED".into()));
        r.license_id = Some("lic_1".into());
        r.clock_initialized = true;
        r.stale_since = Some(1_000);
        r.lease = Some(LeaseRecord {
            lease_id: "L1".into(),
            generation: 3,
            expires_at: 2_000,
            grace_seconds: 60,
            device_id_hash: Some("h".into()),
            pool: Some("computer".into()),
            issued_at: Some("2026-10-16T00:00:00Z".into()),
            expires_at_text: Some("2026-10-17T00:00:00Z".into()),
            signature: Some("sig".into()),
        });
        r
    }

    #[test]
    fn an_envelope_round_trips_with_a_fresh_revision_each_time() {
        let a = Envelope::active(record()).unwrap();
        let b = Envelope::active(record()).unwrap();
        assert_ne!(a.revision, b.revision, "128-bit random revisions differ");
        assert!(a.revision.as_deref().unwrap().len() >= 22);
        let secret = a.encode();
        let back = parse_secret(&secret).unwrap();
        assert_eq!(back.revision, a.revision);
        assert_eq!(back.state, RecordState::Active);
        let r = back.record.unwrap();
        assert_eq!(r.signed_token(), Some("SENTINEL_SIGNED"));
        assert_eq!(r.activation_token(), Some("SENTINEL_ACT"));
        assert!(r.clock_initialized);
        assert_eq!(r.stale_since, Some(1_000));
        let lease = r.lease.unwrap();
        assert_eq!(
            (lease.generation, lease.expires_at, lease.grace_seconds),
            (3, 2_000, 60)
        );
        assert_eq!(lease.signature.as_deref(), Some("sig"));
    }

    #[test]
    fn a_tombstone_has_no_record_and_never_reads_as_active() {
        let tomb = Envelope::tombstone().unwrap();
        assert!(tomb.is_tombstone() && tomb.record.is_none());
        let text = tomb.encode();
        let text = text.expose_str().unwrap();
        assert!(text.contains(r#""state":"tombstone""#));
        assert!(!text.contains("record"));
        let back = Envelope::parse(text).unwrap();
        assert!(back.is_tombstone() && back.record.is_none() && back.revision.is_some());
        // A tombstone that still carries a record (hand-edited) holds no record either.
        let sneaky =
            r#"{"rkv":1,"revision":"r","state":"tombstone","record":{"signedToken":"a.b"}}"#;
        let parsed = Envelope::parse(sneaky).unwrap();
        assert!(parsed.is_tombstone() && parsed.record.is_none());
    }

    #[test]
    fn an_older_bare_record_is_active_with_no_revision() {
        let bare = r#"{"status":"active","deviceId":"dev-1","activationToken":"A","signedToken":"S","licenseId":"lic_1","activatedAt":"2026-10-16T00:00:00Z","staleSince":"2026-10-16T00:00:10Z"}"#;
        let env = Envelope::parse(bare).unwrap();
        assert_eq!(env.revision, None);
        assert_eq!(env.state, RecordState::Active);
        let r = env.record.as_ref().unwrap();
        assert_eq!(r.status, RecordStatus::Active);
        assert_eq!(r.signed_token(), Some("S"));
        assert_eq!(r.activated_at, Some(1_792_108_800));
        assert_eq!(r.stale_since, Some(1_792_108_810));
        // Re-encoding a bare record writes an envelope.
        let secret = env.encode();
        assert!(secret.expose_str().unwrap().contains(r#""rkv":1"#));
    }

    #[test]
    fn a_heardright_flat_lease_record_is_read() {
        let flat = r#"{"status":"active","device_id":"d","signed_token":"S","license_clock_initialized":true,"lease_id":"L","lease_generation":4,"lease_expires_at_unix":99,"lease_grace_seconds":5}"#;
        let r = Envelope::parse(flat).unwrap().record.unwrap();
        assert!(r.clock_initialized);
        let lease = r.lease.unwrap();
        assert_eq!(
            (lease.lease_id.as_str(), lease.generation, lease.expires_at),
            ("L", 4, 99)
        );
        assert_eq!(lease.grace_seconds, 5);
    }

    #[test]
    fn unknown_statuses_never_become_active() {
        for status in ["weird", "failed", "none", ""] {
            let text = format!(r#"{{"status":"{status}","signedToken":"S"}}"#);
            let r = Envelope::parse(&text).unwrap().record.unwrap();
            // An empty status is "no status": the bare form, active. Others are not active.
            if status.is_empty() {
                assert_eq!(r.status, RecordStatus::Active);
            } else {
                assert_eq!(r.status, RecordStatus::Invalid, "{status}");
            }
        }
        let removed = Envelope::parse(r#"{"status":"removed"}"#)
            .unwrap()
            .record
            .unwrap();
        assert_eq!(removed.status, RecordStatus::Revoked);
    }

    #[test]
    fn corrupt_text_and_unknown_envelopes_are_errors_not_absence() {
        for text in [
            "",
            "garbage",
            "[1]",
            r#"{"rkv":2,"revision":"r","state":"active","record":{}}"#,
            r#"{"rkv":1,"state":"active","record":{}}"#,
            r#"{"rkv":1,"revision":"r","state":"future","record":{}}"#,
            r#"{"rkv":1,"revision":"r","state":"active"}"#,
        ] {
            assert!(Envelope::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn debug_never_prints_a_credential() {
        let env = Envelope::active(record()).unwrap();
        let text = format!("{env:?} {:?}", env.record.as_ref().unwrap());
        assert!(!text.contains("SENTINEL"), "{text}");
    }
}
