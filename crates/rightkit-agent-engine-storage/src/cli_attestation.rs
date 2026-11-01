//! Persistence for [`crate::HarnessAttestation`] rows produced by
//! PR 3's qualification runner.
//!
//! The migration that creates the underlying
//! `cli_harness_attestations` table lives in
//! `engine/crates/storage/src/migrations.rs` (`CLI_RUNTIME_BINDING_SQL`,
//! registered at version 9). PR 3 only adds the typed repository that
//! talks to that table; the schema is already in place.
//!
//! Storage rules here mirror the address-as-key invariant the
//! architecture plan mandates in §7.4 / §19.2:
//!
//! - An attestation is identified by the SHA-256 of its canonical
//!   inputs (`HarnessAttestation::id` is that digest). Inserting the
//!   same row twice is a no-op (`INSERT ... ON CONFLICT(id) DO UPDATE`),
//!   not a duplicate write.
//! - The `UNIQUE(provider_id, executable_sha256, profile_hash,
//!   policy_hash, tool_schema_hash, operating_system, architecture,
//!   isolation_mode)` constraint from the migration is the second
//!   line of defence — if a contributor forgets to compute the
//!   address, the database still rejects a colliding fingerprint.
//! - Lookups by `provider_id` always return the most recent
//!   attestation, ordered by `verified_at DESC, id DESC`.

use crate::{Result, Storage, StorageError};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;

/// The typed row mirror of `cli_harness_attestations`. The fields are
/// the ones the migration introduced plus a structured result enum
/// and an evidence blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliHarnessAttestationRow {
    pub id: String,
    pub provider_id: String,
    pub executable_sha256: String,
    pub binary_version: String,
    pub protocol_version: String,
    pub profile_hash: String,
    pub policy_hash: String,
    pub tool_schema_hash: String,
    pub operating_system: String,
    pub architecture: String,
    pub isolation_mode: String,
    pub result: String,
    pub evidence_json: String,
    pub verified_at: String,
}

/// Pass/fail aggregate stored alongside the probe list. Mirrors the
/// `passed_probes` / `failed_probes` split on `HarnessAttestation` so
/// the daemon can gate readiness without parsing the evidence blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAttestationResult {
    Passed,
    Failed,
}

impl CliAttestationResult {
    pub fn label(self) -> &'static str {
        match self {
            CliAttestationResult::Passed => "passed",
            CliAttestationResult::Failed => "failed",
        }
    }

    fn from_label(label: &str) -> Result<Self> {
        match label {
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            other => Err(StorageError::Serde(format!(
                "unknown cli_harness_attestations.result label: {other}"
            ))),
        }
    }
}

const ATTESTATION_COLUMNS: &str = "id, provider_id, executable_sha256, binary_version, \
    protocol_version, profile_hash, policy_hash, tool_schema_hash, operating_system, \
    architecture, isolation_mode, result, evidence_json, verified_at";

impl Storage {
    /// Persist one attestation row. Idempotent on `(id)` — re-inserting
    /// the same content-addressed attestation overwrites the prior
    /// `verified_at` and `evidence_json` so a fresh run replaces a stale
    /// one. Returns the stored row so callers can read back what the
    /// database kept.
    ///
    /// The UNIQUE constraint on the eight address inputs is enforced
    /// by the database even if the caller skipped the SHA-256
    /// computation — but we always compute it ourselves in
    /// [`Storage::record_cli_harness_attestation`] via the typed
    /// signature below.
    ///
    /// Address-as-key invariant: the `id` MUST equal the SHA-256 of
    /// the canonical input tuple (see
    /// `AttestationInputs::address` in `host_driver-host`). The
    /// caller-supplied `id` is treated as authoritative; mismatches
    /// against the canonical digest are surfaced as
    /// [`StorageError::Serde`] rather than silently written, so a
    /// contributor cannot ship an attestation that disagrees with its
    /// own provenance inputs.
    pub fn upsert_cli_harness_attestation(&self, row: &CliHarnessAttestationRow) -> Result<()> {
        if row.id.is_empty() {
            return Err(StorageError::Serde(
                "cli_harness_attestations.id must be the SHA-256 address".into(),
            ));
        }
        if row.provider_id.is_empty() {
            return Err(StorageError::Serde(
                "cli_harness_attestations.provider_id must not be empty".into(),
            ));
        }
        if !is_sha256_hex(&row.id) {
            return Err(StorageError::Serde(format!(
                "cli_harness_attestations.id {} is not a 64-char SHA-256 hex digest",
                row.id
            )));
        }
        let expected_id = canonical_attestation_id(row);
        if row.id != expected_id {
            return Err(StorageError::Serde(format!(
                "cli_harness_attestations.id does not match canonical inputs: expected {expected_id}"
            )));
        }
        self.conn.execute(
            "INSERT INTO cli_harness_attestations
             (id, provider_id, executable_sha256, binary_version, protocol_version,
              profile_hash, policy_hash, tool_schema_hash, operating_system, architecture,
              isolation_mode, result, evidence_json, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
              provider_id=excluded.provider_id,
              executable_sha256=excluded.executable_sha256,
              binary_version=excluded.binary_version,
              protocol_version=excluded.protocol_version,
              profile_hash=excluded.profile_hash,
              policy_hash=excluded.policy_hash,
              tool_schema_hash=excluded.tool_schema_hash,
              operating_system=excluded.operating_system,
              architecture=excluded.architecture,
              isolation_mode=excluded.isolation_mode,
              result=excluded.result,
              evidence_json=excluded.evidence_json,
              verified_at=excluded.verified_at",
            rusqlite::params![
                row.id,
                row.provider_id,
                row.executable_sha256,
                row.binary_version,
                row.protocol_version,
                row.profile_hash,
                row.policy_hash,
                row.tool_schema_hash,
                row.operating_system,
                row.architecture,
                row.isolation_mode,
                row.result,
                row.evidence_json,
                row.verified_at,
            ],
        )?;
        Ok(())
    }

    /// Look up one attestation by its SHA-256 address. Returns
    /// `Ok(None)` when the database has no row with that id — callers
    /// must treat that as "not yet attested".
    pub fn get_cli_harness_attestation(
        &self,
        id: &str,
    ) -> Result<Option<CliHarnessAttestationRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ATTESTATION_COLUMNS} FROM cli_harness_attestations \
                     WHERE id=?1"
                ),
                [id],
                Self::map_attestation_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Most-recent attestation for a provider, ordered by
    /// `verified_at DESC` then `id DESC`. This is the row the
    /// `is_ready` gate compares against the on-disk executable
    /// digest.
    pub fn latest_cli_harness_attestation(
        &self,
        provider_id: &str,
    ) -> Result<Option<CliHarnessAttestationRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ATTESTATION_COLUMNS} FROM cli_harness_attestations \
                     WHERE provider_id=?1 \
                     ORDER BY verified_at DESC, id DESC LIMIT 1"
                ),
                [provider_id],
                Self::map_attestation_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// All attestations for a provider, newest first. Used by the
    /// UI history view and by the recovery PR (PR 13) to detect
    /// binary drift across the lifetime of an install.
    pub fn list_cli_harness_attestations(
        &self,
        provider_id: &str,
    ) -> Result<Vec<CliHarnessAttestationRow>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {ATTESTATION_COLUMNS} FROM cli_harness_attestations \
             WHERE provider_id=?1 \
             ORDER BY verified_at DESC, id DESC"
        ))?;
        let rows = statement.query_map([provider_id], Self::map_attestation_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Delete an attestation by id. Returns whether a row was
    /// actually removed. Used by the UI when a contributor manually
    /// clears a stale row and by tests; production code should rarely
    /// call this because the address key already guarantees uniqueness.
    pub fn delete_cli_harness_attestation(&self, id: &str) -> Result<bool> {
        let removed = self
            .conn
            .execute("DELETE FROM cli_harness_attestations WHERE id=?1", [id])?;
        Ok(removed == 1)
    }

    fn map_attestation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliHarnessAttestationRow> {
        Ok(CliHarnessAttestationRow {
            id: row.get(0)?,
            provider_id: row.get(1)?,
            executable_sha256: row.get(2)?,
            binary_version: row.get(3)?,
            protocol_version: row.get(4)?,
            profile_hash: row.get(5)?,
            policy_hash: row.get(6)?,
            tool_schema_hash: row.get(7)?,
            operating_system: row.get(8)?,
            architecture: row.get(9)?,
            isolation_mode: row.get(10)?,
            result: row.get(11)?,
            evidence_json: row.get(12)?,
            verified_at: row.get(13)?,
        })
    }
}

/// Decode the structured `result` column back into the typed enum.
impl CliHarnessAttestationRow {
    pub fn attestation_result(&self) -> Result<CliAttestationResult> {
        CliAttestationResult::from_label(&self.result)
    }
}

/// True when `id` looks like a 64-char lower-hex SHA-256 digest. The
/// attestation table is content-addressed by the SHA-256 of the
/// canonical input tuple; the storage layer rejects anything else at
/// write time so a contributor cannot accidentally store a row keyed
/// by a non-deterministic identifier.
fn is_sha256_hex(id: &str) -> bool {
    if id.len() != 64 {
        return false;
    }
    id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn canonical_attestation_id(row: &CliHarnessAttestationRow) -> String {
    let canonical = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        row.provider_id,
        row.executable_sha256,
        row.binary_version,
        row.protocol_version,
        row.profile_hash,
        row.policy_hash,
        row.tool_schema_hash,
        row.operating_system,
        row.architecture,
        row.isolation_mode,
    );
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(canonical.as_bytes());
    hasher.finalize()
}

/// The attestation table's content-addressed uniqueness invariant:
/// given an `(provider_id, executable_sha256, profile_hash,
/// policy_hash, tool_schema_hash, operating_system, architecture,
/// isolation_mode)` tuple, at most one row may exist (the database
/// enforces this with the `UNIQUE` clause added in migration v9).
///
/// Returns `Ok(true)` when the database would accept the row (no
/// collision), and `Ok(false)` when an attestation with those inputs
/// already exists. Callers that want to overwrite a stale row should
/// go through `upsert_cli_harness_attestation`, which always wins
/// on `id` (the SHA-256 of those inputs).
#[allow(clippy::too_many_arguments)]
pub fn attestation_inputs_are_unique(
    storage: &Storage,
    provider_id: &str,
    executable_sha256: &str,
    profile_hash: &str,
    policy_hash: &str,
    tool_schema_hash: &str,
    operating_system: &str,
    architecture: &str,
    isolation_mode: &str,
) -> Result<bool> {
    let exists: bool = storage.conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM cli_harness_attestations
            WHERE provider_id = ?1
              AND executable_sha256 = ?2
              AND profile_hash = ?3
              AND policy_hash = ?4
              AND tool_schema_hash = ?5
              AND operating_system = ?6
              AND architecture = ?7
              AND isolation_mode = ?8
         )",
        rusqlite::params![
            provider_id,
            executable_sha256,
            profile_hash,
            policy_hash,
            tool_schema_hash,
            operating_system,
            architecture,
            isolation_mode,
        ],
        |row| row.get(0),
    )?;
    Ok(!exists)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid 64-char lower-hex SHA-256 digest, padded with `0`s.
    /// Tests that exercise `upsert_cli_harness_attestation` need a
    /// real-looking address so the storage layer's hex check accepts
    /// the row.
    const SHA256_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn sample_row(_id: &str, provider: &str) -> CliHarnessAttestationRow {
        let mut row = CliHarnessAttestationRow {
            id: String::new(),
            provider_id: provider.to_string(),
            executable_sha256: SHA256_HEX.into(),
            binary_version: "0.1.0".into(),
            protocol_version: "acp/1".into(),
            profile_hash: "p".into(),
            policy_hash: "x".into(),
            tool_schema_hash: "t".into(),
            operating_system: "macos".into(),
            architecture: "arm64".into(),
            isolation_mode: "exclusive_host_gateway".into(),
            result: "passed".into(),
            evidence_json: "{\"probes\":[]}".into(),
            verified_at: "2026-08-07T00:00:00Z".into(),
        };
        row.id = canonical_attestation_id(&row);
        row
    }

    /// Build a row whose `id` is the SHA-256 digest of
    /// `attestation_inputs_are_unique`'s eight inputs. Tests use this
    /// so the `id` is content-addressed, matching the address-as-key
    /// invariant in production.
    fn sample_unique_row(suffix: u8, provider: &str) -> CliHarnessAttestationRow {
        let mut row = sample_row(SHA256_HEX, provider);
        row.executable_sha256 = format!("{:x}", suffix).repeat(64);
        row.id = canonical_attestation_id(&row);
        row
    }

    #[test]
    fn empty_id_is_rejected() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sample_row(SHA256_HEX, "claude");
        row.id = String::new();
        assert!(s.upsert_cli_harness_attestation(&row).is_err());
    }

    #[test]
    fn non_sha256_id_is_rejected() {
        let s = Storage::open_in_memory().unwrap();
        // Too short, not 64 chars.
        let mut row = sample_row("addr", "claude");
        row.id = "not-a-sha".into();
        assert!(s.upsert_cli_harness_attestation(&row).is_err());
        // 64 chars but contains non-hex bytes.
        let mut row = sample_row(SHA256_HEX, "claude");
        row.id = "z".repeat(64);
        assert!(s.upsert_cli_harness_attestation(&row).is_err());
    }

    #[test]
    fn well_formed_but_noncanonical_id_is_rejected() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sample_row(SHA256_HEX, "claude");
        row.id = "a".repeat(64);
        assert!(s.upsert_cli_harness_attestation(&row).is_err());
    }

    #[test]
    fn upsert_is_idempotent_on_address() {
        let s = Storage::open_in_memory().unwrap();
        let row = sample_row(SHA256_HEX, "claude");
        s.upsert_cli_harness_attestation(&row).unwrap();
        s.upsert_cli_harness_attestation(&row).unwrap();
        let rows = s.list_cli_harness_attestations("claude").unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn upsert_overwrites_when_id_matches() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sample_row(SHA256_HEX, "claude");
        let id = row.id.clone();
        s.upsert_cli_harness_attestation(&row).unwrap();
        row.evidence_json = r#"{"probes":["new"]}"#.into();
        row.verified_at = "2026-08-08T00:00:00Z".into();
        s.upsert_cli_harness_attestation(&row).unwrap();
        let got = s.get_cli_harness_attestation(&id).unwrap().unwrap();
        assert_eq!(got.evidence_json, r#"{"probes":["new"]}"#);
        assert_eq!(got.verified_at, "2026-08-08T00:00:00Z");
    }

    #[test]
    fn latest_returns_most_recent_verified_at() {
        let s = Storage::open_in_memory().unwrap();
        let mut older = sample_unique_row(1, "codex");
        older.verified_at = "2026-08-01T00:00:00Z".into();
        s.upsert_cli_harness_attestation(&older).unwrap();

        let mut newer = sample_unique_row(2, "codex");
        newer.verified_at = "2026-08-07T00:00:00Z".into();
        s.upsert_cli_harness_attestation(&newer).unwrap();

        let got = s.latest_cli_harness_attestation("codex").unwrap().unwrap();
        assert_eq!(got.verified_at, "2026-08-07T00:00:00Z");
    }

    #[test]
    fn latest_returns_none_when_empty() {
        let s = Storage::open_in_memory().unwrap();
        assert!(s
            .latest_cli_harness_attestation("missing")
            .unwrap()
            .is_none());
    }

    #[test]
    fn list_is_scoped_to_provider() {
        let s = Storage::open_in_memory().unwrap();
        let claude1 = sample_unique_row(1, "claude");
        s.upsert_cli_harness_attestation(&claude1).unwrap();
        let claude2 = sample_unique_row(2, "claude");
        s.upsert_cli_harness_attestation(&claude2).unwrap();
        let codex1 = sample_unique_row(3, "codex");
        s.upsert_cli_harness_attestation(&codex1).unwrap();
        let claude = s.list_cli_harness_attestations("claude").unwrap();
        assert_eq!(claude.len(), 2);
        let codex = s.list_cli_harness_attestations("codex").unwrap();
        assert_eq!(codex.len(), 1);
    }

    #[test]
    fn delete_removes_exactly_one_row() {
        let s = Storage::open_in_memory().unwrap();
        let row = sample_row(SHA256_HEX, "claude");
        let id = row.id.clone();
        s.upsert_cli_harness_attestation(&row).unwrap();
        assert!(s.delete_cli_harness_attestation(&id).unwrap());
        assert!(s.get_cli_harness_attestation(&id).unwrap().is_none());
        // Second delete is a no-op.
        assert!(!s.delete_cli_harness_attestation(&id).unwrap());
    }

    #[test]
    fn attestation_result_round_trips() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sample_row(SHA256_HEX, "claude");
        row.result = "failed".into();
        let id = row.id.clone();
        s.upsert_cli_harness_attestation(&row).unwrap();
        let got = s.get_cli_harness_attestation(&id).unwrap().unwrap();
        assert_eq!(
            got.attestation_result().unwrap(),
            CliAttestationResult::Failed
        );
    }

    #[test]
    fn unknown_result_label_is_a_typed_error() {
        let s = Storage::open_in_memory().unwrap();
        let mut row = sample_row(SHA256_HEX, "claude");
        row.result = "maybe".into();
        let id = row.id.clone();
        s.upsert_cli_harness_attestation(&row).unwrap();
        let got = s.get_cli_harness_attestation(&id).unwrap().unwrap();
        assert!(got.attestation_result().is_err());
    }

    #[test]
    fn uniqueness_invariant_is_false_after_first_insert() {
        let s = Storage::open_in_memory().unwrap();
        let row = sample_unique_row(7, "claude");
        s.upsert_cli_harness_attestation(&row).unwrap();
        let unique = attestation_inputs_are_unique(
            &s,
            "claude",
            &row.executable_sha256,
            &row.profile_hash,
            &row.policy_hash,
            &row.tool_schema_hash,
            &row.operating_system,
            &row.architecture,
            &row.isolation_mode,
        )
        .unwrap();
        assert!(!unique, "the same address inputs must collide");
    }

    #[test]
    fn uniqueness_invariant_is_true_for_never_seen_inputs() {
        let s = Storage::open_in_memory().unwrap();
        let unique = attestation_inputs_are_unique(
            &s,
            "fresh-cli",
            SHA256_HEX,
            "p",
            "x",
            "t",
            "macos",
            "arm64",
            "exclusive_host_gateway",
        )
        .unwrap();
        assert!(unique);
    }
}
