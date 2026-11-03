use crate::{Result, Storage, StorageError};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverBindingRow {
    pub id: String,
    pub driver_kind: String,
    pub session_id: String,
    pub vendor_session_id: Option<String>,
    pub vendor_thread_id: Option<String>,
    pub vendor_turn_id: Option<String>,
    pub binary_version: String,
    pub schema_hash: String,
    pub status: String,
    pub last_event_key: Option<String>,
    pub last_event_cursor: Option<String>,
    /// The the host app agent id this binding belongs to. Empty string for
    /// pre-PR-5 bindings that pre-date the multi-provider runtime; populated
    /// by `upsert_driver_binding` from PR 5 onwards.
    pub host_agent_id: Option<String>,
    /// Parent the host app agent id. Empty string when this binding belongs to a
    /// root session (no parent); populated to the spawning child's id by the
    /// sub-agent CLI delegation path. Pre-PR-5 rows default to `""`.
    pub parent_agent_id: String,
    /// Provider identifier (e.g. `"claude"`, `"codex"`). Populated by the
    /// multi-provider CLI runtime migration (PR 9).
    pub provider_id: Option<String>,
    /// Transport kind (e.g. `"claude_stream_json"`, `"acp"`). Populated by
    /// the multi-provider CLI runtime migration (PR 9).
    pub transport_kind: Option<String>,
    /// Account profile id this binding is bound to. Populated by PR 9.
    pub account_profile_id: Option<String>,
    /// Auth mode (`native_cli_account`, `byok_or_external_api`, `unknown`).
    /// Populated by PR 9.
    pub auth_mode: Option<String>,
    /// One-way fingerprint of the vendor account. Non-secret.
    pub account_fingerprint: Option<String>,
    /// Model identifier the binding was opened with.
    pub model_id: Option<String>,
    /// Filesystem path to the vendor CLI executable. Re-validated against
    /// the stored SHA-256 on every binding load by the recovery flow
    /// (PR 13).
    pub executable_path: Option<String>,
    /// SHA-256 of the executable at `executable_path`. Re-validated on
    /// every binding load; mismatch forces re-qualification (PR 13).
    pub executable_sha256: Option<String>,
    /// Protocol version the binding was opened against. Mismatches
    /// against the live process are surfaced as
    /// `RecoveryError::ProtocolVersionMismatch` (PR 13).
    pub protocol_version: Option<String>,
    /// Profile hash for harness attestation lookup.
    pub profile_hash: Option<String>,
    /// Policy hash for harness attestation lookup.
    pub policy_hash: Option<String>,
    /// Tool schema hash for harness attestation lookup.
    pub tool_schema_hash: Option<String>,
    /// Id of the most recent attestation this binding was opened under.
    pub harness_attestation_id: Option<String>,
    /// Isolation mode the binding was opened with.
    pub isolation_mode: Option<String>,
    /// Resume strategy the binding was opened with.
    pub resume_strategy: Option<String>,
    /// Context epoch the binding was opened under. Compared against the
    /// live the host app session epoch on every resume (PR 13).
    pub context_epoch: u64,
    /// Process generation count. Incremented every time the vendor process
    /// restarts (PR 13). Compared against the live process generation on
    /// every resume.
    pub process_generation: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverEventInboxRow {
    pub binding_id: String,
    pub event_key: String,
    pub event_cursor: Option<String>,
    pub event_kind: String,
    pub payload_json: String,
    pub status: String,
    pub received_at: String,
    pub processed_at: Option<String>,
    pub result_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum DriverEventClaim {
    Claimed,
    Duplicate(DriverEventInboxRow),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCapabilityLeaseRow {
    pub id: String,
    pub binding_id: String,
    /// A one-way digest of the bearer token. Plaintext tokens must never enter storage.
    pub token_hash: String,
    pub nonce: String,
    pub session_id: String,
    pub canonical_workspace: String,
    pub tool_ceiling_json: String,
    pub created_at: String,
    pub expires_at: String,
    pub revoked_at: Option<String>,
    /// Digest of the random secret injected only into the currently supervised
    /// driver process and its MCP child. This prevents a leaked lease token
    /// from being used by an unrelated local process.
    pub process_instance_hash: Option<String>,
}

const DRIVER_BINDING_COLUMNS: &str = "id, driver_kind, session_id, vendor_session_id, \
    vendor_thread_id, vendor_turn_id, binary_version, schema_hash, status, last_event_key, \
    last_event_cursor, coderight_agent_id, parent_agent_id, provider_id, transport_kind, \
    account_profile_id, auth_mode, account_fingerprint, model_id, executable_path, \
    executable_sha256, protocol_version, profile_hash, policy_hash, tool_schema_hash, \
    harness_attestation_id, isolation_mode, resume_strategy, context_epoch, process_generation, \
    created_at, updated_at";

const DRIVER_EVENT_COLUMNS: &str =
    "binding_id, event_key, event_cursor, event_kind, payload_json, \
    status, received_at, processed_at, result_json";

const LEASE_COLUMNS: &str = "id, binding_id, token_hash, nonce, session_id, canonical_workspace, \
    tool_ceiling_json, created_at, expires_at, revoked_at, process_instance_hash";

impl Storage {
    pub fn upsert_driver_binding(&self, row: &DriverBindingRow) -> Result<()> {
        let context_epoch = sqlite_integer(row.context_epoch, "context_epoch")?;
        let process_generation = sqlite_integer(row.process_generation, "process_generation")?;
        self.conn.execute(
            "INSERT INTO driver_bindings \
             (id, driver_kind, session_id, vendor_session_id, vendor_thread_id, vendor_turn_id, \
              binary_version, schema_hash, status, last_event_key, last_event_cursor, \
              coderight_agent_id, parent_agent_id, provider_id, transport_kind, account_profile_id, \
              auth_mode, account_fingerprint, model_id, executable_path, executable_sha256, \
              protocol_version, profile_hash, policy_hash, tool_schema_hash, harness_attestation_id, \
              isolation_mode, resume_strategy, context_epoch, process_generation, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                      ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, \
                      ?31, ?32) \
             ON CONFLICT(id) DO UPDATE SET \
              driver_kind=excluded.driver_kind, session_id=excluded.session_id, \
              vendor_session_id=excluded.vendor_session_id, vendor_thread_id=excluded.vendor_thread_id, \
              vendor_turn_id=excluded.vendor_turn_id, binary_version=excluded.binary_version, \
              schema_hash=excluded.schema_hash, status=excluded.status, \
              last_event_key=excluded.last_event_key, last_event_cursor=excluded.last_event_cursor, \
              coderight_agent_id=excluded.coderight_agent_id, parent_agent_id=excluded.parent_agent_id, \
              provider_id=excluded.provider_id, transport_kind=excluded.transport_kind, \
              account_profile_id=excluded.account_profile_id, auth_mode=excluded.auth_mode, \
              account_fingerprint=excluded.account_fingerprint, model_id=excluded.model_id, \
              executable_path=excluded.executable_path, executable_sha256=excluded.executable_sha256, \
              protocol_version=excluded.protocol_version, profile_hash=excluded.profile_hash, \
              policy_hash=excluded.policy_hash, tool_schema_hash=excluded.tool_schema_hash, \
              harness_attestation_id=excluded.harness_attestation_id, \
              isolation_mode=excluded.isolation_mode, resume_strategy=excluded.resume_strategy, \
              context_epoch=excluded.context_epoch, process_generation=excluded.process_generation, \
              updated_at=excluded.updated_at",
            params![
                row.id,
                row.driver_kind,
                row.session_id,
                row.vendor_session_id,
                row.vendor_thread_id,
                row.vendor_turn_id,
                row.binary_version,
                row.schema_hash,
                row.status,
                row.last_event_key,
                row.last_event_cursor,
                row.host_agent_id,
                row.parent_agent_id,
                row.provider_id,
                row.transport_kind,
                row.account_profile_id,
                row.auth_mode,
                row.account_fingerprint,
                row.model_id,
                row.executable_path,
                row.executable_sha256,
                row.protocol_version,
                row.profile_hash,
                row.policy_hash,
                row.tool_schema_hash,
                row.harness_attestation_id,
                row.isolation_mode,
                row.resume_strategy,
                context_epoch,
                process_generation,
                row.created_at,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_driver_binding(&self, id: &str) -> Result<Option<DriverBindingRow>> {
        self.conn
            .query_row(
                &format!("SELECT {DRIVER_BINDING_COLUMNS} FROM driver_bindings WHERE id=?1"),
                [id],
                Self::map_driver_binding,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_driver_binding_for_session(
        &self,
        driver_kind: &str,
        session_id: &str,
    ) -> Result<Option<DriverBindingRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {DRIVER_BINDING_COLUMNS} FROM driver_bindings \
                     WHERE driver_kind=?1 AND session_id=?2"
                ),
                params![driver_kind, session_id],
                Self::map_driver_binding,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_driver_bindings_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<DriverBindingRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRIVER_BINDING_COLUMNS} FROM driver_bindings \
             WHERE session_id=?1 ORDER BY driver_kind ASC, id ASC"
        ))?;
        let rows = stmt.query_map([session_id], Self::map_driver_binding)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_driver_binding_progress(
        &self,
        id: &str,
        vendor_session_id: Option<&str>,
        vendor_thread_id: Option<&str>,
        vendor_turn_id: Option<&str>,
        status: &str,
        last_event_key: Option<&str>,
        last_event_cursor: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET vendor_session_id=?2, vendor_thread_id=?3, \
             vendor_turn_id=?4, status=?5, last_event_key=?6, last_event_cursor=?7, updated_at=?8 \
             WHERE id=?1",
            params![
                id,
                vendor_session_id,
                vendor_thread_id,
                vendor_turn_id,
                status,
                last_event_key,
                last_event_cursor,
                updated_at
            ],
        )? == 1)
    }

    pub fn delete_driver_binding(&self, id: &str) -> Result<bool> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        tx.execute("DELETE FROM driver_event_inbox WHERE binding_id=?1", [id])?;
        let deleted = tx.execute("DELETE FROM driver_bindings WHERE id=?1", [id])? == 1;
        tx.commit()?;
        Ok(deleted)
    }

    /// Increment `process_generation` for a single binding. PR 13 calls
    /// this every time the vendor process restarts so the next
    /// `resume()` can detect "this session is from an older process"
    /// by comparing the persisted value against the live generation.
    ///
    /// Returns `true` when the row existed and was updated; `false`
    /// when no row matched `id`. Callers should treat a `false`
    /// result as a signal that the binding has already been removed
    /// (e.g. the user stopped the session).
    pub fn bump_driver_binding_process_generation(
        &self,
        id: &str,
        updated_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET process_generation = process_generation + 1, \
             updated_at = ?2 WHERE id = ?1",
            params![id, updated_at],
        )? == 1)
    }

    /// Stamp a binding with an explicit `process_generation` value.
    /// Used by the deferred-update applier (PR 13) so a turn-boundary
    /// write can land the exact generation the supervisor handed out,
    /// not just a +1.
    pub fn set_driver_binding_process_generation(
        &self,
        id: &str,
        next_generation: u64,
        updated_at: &str,
    ) -> Result<bool> {
        let next_generation = sqlite_integer(next_generation, "process_generation")?;
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET process_generation = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![id, next_generation, updated_at],
        )? == 1)
    }

    /// Increment `context_epoch` after the host app rebuilds the parent
    /// context (workspace trust, authentication, model, or policy).
    pub fn bump_driver_binding_context_epoch(&self, id: &str, updated_at: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET context_epoch = context_epoch + 1, \
             updated_at = ?2 WHERE id = ?1",
            params![id, updated_at],
        )? == 1)
    }

    /// Stamp a binding with a fresh `context_epoch`. PR 13 calls this
    /// after the the host app context is rebuilt (workspace trust,
    /// re-auth, model swap, etc.) so subsequent resume attempts can
    /// detect the stale-epoch case.
    pub fn set_driver_binding_context_epoch(
        &self,
        id: &str,
        next_epoch: u64,
        updated_at: &str,
    ) -> Result<bool> {
        let next_epoch = sqlite_integer(next_epoch, "context_epoch")?;
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET context_epoch = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![id, next_epoch, updated_at],
        )? == 1)
    }

    /// Record a new `executable_sha256` against the binding. PR 13
    /// uses this after the operator re-qualifies a freshly-installed
    /// binary so the next resume attempt reads the new digest from
    /// disk and accepts it.
    pub fn set_driver_binding_executable_sha256(
        &self,
        id: &str,
        sha256: &str,
        updated_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET executable_sha256 = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![id, sha256, updated_at],
        )? == 1)
    }

    /// Stamp a binding with the `protocol_version` the live process is
    /// speaking. Used by PR 13's protocol-mismatch path so the next
    /// resume compares against the same string the supervisor just
    /// observed.
    pub fn set_driver_binding_protocol_version(
        &self,
        id: &str,
        protocol_version: &str,
        updated_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_bindings SET protocol_version = ?2, updated_at = ?3 \
             WHERE id = ?1",
            params![id, protocol_version, updated_at],
        )? == 1)
    }

    /// Walk every binding the daemon persisted before this restart.
    /// PR 13 calls this once at startup to decide which bindings can
    /// resume; the storage layer does not interpret the result, it
    /// only returns the rows.
    pub fn list_active_driver_bindings(&self) -> Result<Vec<DriverBindingRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRIVER_BINDING_COLUMNS} FROM driver_bindings \
             WHERE status IN ('leased','starting','connected','running',\
                              'waiting_for_approval','waiting_for_child','resuming') \
             ORDER BY id ASC"
        ))?;
        let rows = stmt.query_map([], Self::map_driver_binding)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Walk every binding regardless of status. Used by the test
    /// suite and by operator tooling that needs to enumerate all
    /// bindings (for example a migration diagnostic).
    pub fn list_all_driver_bindings(&self) -> Result<Vec<DriverBindingRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRIVER_BINDING_COLUMNS} FROM driver_bindings ORDER BY id ASC"
        ))?;
        let rows = stmt.query_map([], Self::map_driver_binding)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Claim one vendor event before applying it. The insert and binding cursor
    /// advance share one IMMEDIATE transaction; a replay returns the original row.
    pub fn claim_driver_event(&mut self, row: &DriverEventInboxRow) -> Result<DriverEventClaim> {
        let tx = rightkit_db::write_txn(&mut self.conn)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM driver_bindings WHERE id=?1)",
            [&row.binding_id],
            |value| value.get(0),
        )?;
        if !exists {
            return Err(StorageError::NotFound(format!(
                "driver binding {}",
                row.binding_id
            )));
        }
        let inserted = tx.execute(
            "INSERT INTO driver_event_inbox \
             (binding_id, event_key, event_cursor, event_kind, payload_json, status, received_at, processed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(binding_id, event_key) DO NOTHING",
            params![row.binding_id, row.event_key, row.event_cursor, row.event_kind,
                row.payload_json, row.status, row.received_at, row.processed_at],
        )?;
        if inserted == 0 {
            let duplicate = tx.query_row(
                &format!(
                    "SELECT {DRIVER_EVENT_COLUMNS} FROM driver_event_inbox \
                     WHERE binding_id=?1 AND event_key=?2"
                ),
                params![row.binding_id, row.event_key],
                Self::map_driver_event,
            )?;
            tx.commit()?;
            return Ok(DriverEventClaim::Duplicate(duplicate));
        }
        tx.execute(
            "UPDATE driver_bindings SET last_event_key=?2, last_event_cursor=?3, updated_at=?4 \
             WHERE id=?1",
            params![
                row.binding_id,
                row.event_key,
                row.event_cursor,
                row.received_at
            ],
        )?;
        tx.commit()?;
        Ok(DriverEventClaim::Claimed)
    }

    pub fn get_driver_event(
        &self,
        binding_id: &str,
        event_key: &str,
    ) -> Result<Option<DriverEventInboxRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {DRIVER_EVENT_COLUMNS} FROM driver_event_inbox \
                     WHERE binding_id=?1 AND event_key=?2"
                ),
                params![binding_id, event_key],
                Self::map_driver_event,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn mark_driver_event_processed(
        &self,
        binding_id: &str,
        event_key: &str,
        status: &str,
        processed_at: &str,
    ) -> Result<bool> {
        self.mark_driver_event_processed_with_result(
            binding_id,
            event_key,
            status,
            processed_at,
            None,
        )
    }

    pub fn mark_driver_event_processed_with_result(
        &self,
        binding_id: &str,
        event_key: &str,
        status: &str,
        processed_at: &str,
        result_json: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE driver_event_inbox SET status=?3, processed_at=?4, result_json=?5 \
             WHERE binding_id=?1 AND event_key=?2",
            params![binding_id, event_key, status, processed_at, result_json],
        )? == 1)
    }

    pub fn list_driver_events(&self, binding_id: &str) -> Result<Vec<DriverEventInboxRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRIVER_EVENT_COLUMNS} FROM driver_event_inbox \
             WHERE binding_id=?1 ORDER BY received_at ASC, event_key ASC"
        ))?;
        let rows = stmt.query_map([binding_id], Self::map_driver_event)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn upsert_session_capability_lease(&self, row: &SessionCapabilityLeaseRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session_capability_leases \
             (id, binding_id, token_hash, nonce, session_id, canonical_workspace, tool_ceiling_json, \
              created_at, expires_at, revoked_at, process_instance_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET binding_id=excluded.binding_id, token_hash=excluded.token_hash, nonce=excluded.nonce, \
              session_id=excluded.session_id, canonical_workspace=excluded.canonical_workspace, \
              tool_ceiling_json=excluded.tool_ceiling_json, expires_at=excluded.expires_at, \
              revoked_at=excluded.revoked_at, process_instance_hash=excluded.process_instance_hash",
            params![
                row.id,
                row.binding_id,
                row.token_hash,
                row.nonce,
                row.session_id,
                row.canonical_workspace,
                row.tool_ceiling_json,
                row.created_at,
                row.expires_at,
                row.revoked_at,
                row.process_instance_hash
            ],
        )?;
        Ok(())
    }

    pub fn get_session_capability_lease(
        &self,
        id: &str,
    ) -> Result<Option<SessionCapabilityLeaseRow>> {
        self.conn
            .query_row(
                &format!("SELECT {LEASE_COLUMNS} FROM session_capability_leases WHERE id=?1"),
                [id],
                Self::map_session_capability_lease,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The newest unrevoked lease for a binding, expired or not.
    ///
    /// `get_active_session_capability_lease` filters expiry away, which is right
    /// for authorization but useless for reporting: a caller that only ever sees
    /// "no active lease" cannot tell an expired lease from one that never
    /// existed. The read-only status route needs that distinction to say
    /// "the lease expired" instead of showing a generic failure.
    pub fn get_latest_session_capability_lease_for_binding(
        &self,
        binding_id: &str,
    ) -> Result<Option<SessionCapabilityLeaseRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {LEASE_COLUMNS} FROM session_capability_leases \
                     WHERE binding_id=?1 AND revoked_at IS NULL \
                     ORDER BY expires_at DESC LIMIT 1"
                ),
                [binding_id],
                Self::map_session_capability_lease,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Resolve only an active lease bound to the exact token digest, nonce,
    /// the host app session, and canonical workspace. Timestamps are canonical
    /// RFC3339 UTC strings, whose lexical ordering matches chronological order.
    pub fn get_active_session_capability_lease(
        &self,
        token_hash: &str,
        nonce: &str,
        session_id: &str,
        canonical_workspace: &str,
        now: &str,
    ) -> Result<Option<SessionCapabilityLeaseRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {LEASE_COLUMNS} FROM session_capability_leases \
                     WHERE token_hash=?1 AND nonce=?2 AND session_id=?3 AND canonical_workspace=?4 \
                       AND revoked_at IS NULL AND expires_at>?5"
                ),
                params![token_hash, nonce, session_id, canonical_workspace, now],
                Self::map_session_capability_lease,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn revoke_session_capability_lease(&self, id: &str, revoked_at: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE session_capability_leases SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
            params![id, revoked_at],
        )? == 1)
    }

    pub fn set_session_capability_lease_process_instance(
        &self,
        id: &str,
        process_instance_hash: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE session_capability_leases SET process_instance_hash=?2 WHERE id=?1 AND revoked_at IS NULL",
            params![id, process_instance_hash],
        )? == 1)
    }

    pub fn list_claimed_driver_events(&self, binding_id: &str) -> Result<Vec<DriverEventInboxRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DRIVER_EVENT_COLUMNS} FROM driver_event_inbox \
             WHERE binding_id=?1 AND status='claimed' ORDER BY received_at ASC, event_key ASC"
        ))?;
        let rows = stmt.query_map([binding_id], Self::map_driver_event)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Explicitly resolves an interrupted claim without retrying its effect.
    /// A new driver run may begin only after this operator-authorized action.
    pub fn abandon_claimed_driver_events(&self, binding_id: &str, at: &str) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE driver_event_inbox SET status='recovery_abandoned', processed_at=?2 \
             WHERE binding_id=?1 AND status='claimed'",
                params![binding_id, at],
            )
            .map_err(Into::into)
    }

    pub fn revoke_active_session_capability_leases_for_session(
        &self,
        session_id: &str,
        revoked_at: &str,
    ) -> Result<usize> {
        self.conn
            .execute(
                "UPDATE session_capability_leases SET revoked_at=?2 \
                 WHERE session_id=?1 AND revoked_at IS NULL",
                params![session_id, revoked_at],
            )
            .map_err(Into::into)
    }

    pub fn delete_session_capability_lease(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM session_capability_leases WHERE id=?1", [id])?
            == 1)
    }

    fn map_driver_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<DriverBindingRow> {
        Ok(DriverBindingRow {
            id: row.get(0)?,
            driver_kind: row.get(1)?,
            session_id: row.get(2)?,
            vendor_session_id: row.get(3)?,
            vendor_thread_id: row.get(4)?,
            vendor_turn_id: row.get(5)?,
            binary_version: row.get(6)?,
            schema_hash: row.get(7)?,
            status: row.get(8)?,
            last_event_key: row.get(9)?,
            last_event_cursor: row.get(10)?,
            host_agent_id: row.get(11)?,
            parent_agent_id: row.get(12)?,
            provider_id: row.get(13)?,
            transport_kind: row.get(14)?,
            account_profile_id: row.get(15)?,
            auth_mode: row.get(16)?,
            account_fingerprint: row.get(17)?,
            model_id: row.get(18)?,
            executable_path: row.get(19)?,
            executable_sha256: row.get(20)?,
            protocol_version: row.get(21)?,
            profile_hash: row.get(22)?,
            policy_hash: row.get(23)?,
            tool_schema_hash: row.get(24)?,
            harness_attestation_id: row.get(25)?,
            isolation_mode: row.get(26)?,
            resume_strategy: row.get(27)?,
            context_epoch: row_u64(row, 28)?,
            process_generation: row_u64(row, 29)?,
            created_at: row.get(30)?,
            updated_at: row.get(31)?,
        })
    }

    fn map_driver_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<DriverEventInboxRow> {
        Ok(DriverEventInboxRow {
            binding_id: row.get(0)?,
            event_key: row.get(1)?,
            event_cursor: row.get(2)?,
            event_kind: row.get(3)?,
            payload_json: row.get(4)?,
            status: row.get(5)?,
            received_at: row.get(6)?,
            processed_at: row.get(7)?,
            result_json: row.get(8)?,
        })
    }

    fn map_session_capability_lease(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<SessionCapabilityLeaseRow> {
        Ok(SessionCapabilityLeaseRow {
            id: row.get(0)?,
            binding_id: row.get(1)?,
            token_hash: row.get(2)?,
            nonce: row.get(3)?,
            session_id: row.get(4)?,
            canonical_workspace: row.get(5)?,
            tool_ceiling_json: row.get(6)?,
            created_at: row.get(7)?,
            expires_at: row.get(8)?,
            revoked_at: row.get(9)?,
            process_instance_hash: row.get(10)?,
        })
    }
}

fn sqlite_integer(value: u64, column: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StorageError::Serde(format!(
            "driver_bindings.{column} exceeds SQLite signed-integer range"
        ))
    })
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
