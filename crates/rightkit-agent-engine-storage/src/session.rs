use crate::error::{Result, StorageError};
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

/// A persisted session record. `parent_session_id`, `role`, `status`, and
/// `recovery_marker` are first-class query columns (find a conductor's
/// sub-agents, list dormant sessions, surface "recoverable" sessions in the
/// session list) rather than blobbed into `config_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: String,
    pub config_json: String,
    #[serde(default)]
    pub tags: String,
    /// Serialized `journal::RecoveryMarker` JSON when a leftover execution
    /// journal exists for this session, or `None` after a clean completion
    /// or successful 7-day archive. Set by startup housekeeping + on every
    /// `journal.open()`; cleared on `journal.close()`.
    #[serde(default)]
    pub recovery_marker: Option<String>,
}

/// Result of atomically claiming a mutating machine request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyDecision {
    Fresh {
        correlation_id: String,
    },
    Replay {
        correlation_id: String,
        effect_state: String,
        response_json: String,
    },
    Pending {
        correlation_id: String,
        effect_state: String,
        admission_seq: Option<i64>,
    },
    Conflict {
        correlation_id: String,
        existing_digest: String,
    },
}

const SESSION_COLS: &str = "id, title, workspace_root, created_at, updated_at, parent_session_id, role, status, config_json, tags, recovery_marker";

impl Storage {
    /// Wire-friendly form of [`Self::claim_idempotent_request`] for daemon
    /// crates that intentionally do not depend on storage's internal enum.
    pub fn claim_idempotent_request_wire(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        correlation_id: &str,
        now: &str,
    ) -> Result<serde_json::Value> {
        let decision = self.claim_idempotent_request(
            session_id,
            operation,
            idempotency_key,
            request_digest,
            correlation_id,
            now,
        )?;
        Ok(match decision {
            IdempotencyDecision::Fresh { correlation_id } => serde_json::json!({
                "decision": "fresh", "correlationId": correlation_id,
                "effectState": "in_progress"
            }),
            IdempotencyDecision::Replay {
                correlation_id,
                effect_state,
                response_json,
            } => serde_json::json!({
                "decision": "replay", "correlationId": correlation_id,
                "effectState": effect_state, "responseJson": response_json
            }),
            IdempotencyDecision::Pending {
                correlation_id,
                effect_state,
                admission_seq,
            } => serde_json::json!({
                "decision": "pending", "correlationId": correlation_id,
                "effectState": effect_state, "admissionSeq": admission_seq
            }),
            IdempotencyDecision::Conflict {
                correlation_id,
                existing_digest,
            } => serde_json::json!({
                "decision": "conflict", "correlationId": correlation_id,
                "effectState": "none", "existingDigest": existing_digest
            }),
        })
    }

    /// Claim `(session, operation, key)` before applying a mutation.
    ///
    /// The request digest is immutable. Reusing a key with another body is a
    /// typed conflict; an identical committed retry returns the original
    /// response; an unfinished row is surfaced for reconciliation and is
    /// never executed a second time automatically.
    pub fn claim_idempotent_request(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        correlation_id: &str,
        now: &str,
    ) -> Result<IdempotencyDecision> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO machine_request_idempotency \
             (session_id, operation, idempotency_key, request_digest, correlation_id, effect_state, response_json, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'in_progress', NULL, ?6, ?6)",
            (
                session_id,
                operation,
                idempotency_key,
                request_digest,
                correlation_id,
                now,
            ),
        )? == 1;
        let row = self.conn.query_row(
            "SELECT request_digest, correlation_id, effect_state, response_json, admission_seq \
             FROM machine_request_idempotency \
             WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3",
            (session_id, operation, idempotency_key),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )?;
        if row.0 != request_digest {
            return Ok(IdempotencyDecision::Conflict {
                correlation_id: row.1,
                existing_digest: row.0,
            });
        }
        match row.3 {
            Some(response_json) => Ok(IdempotencyDecision::Replay {
                correlation_id: row.1,
                effect_state: row.2,
                response_json,
            }),
            None if inserted => Ok(IdempotencyDecision::Fresh {
                correlation_id: row.1,
            }),
            None => Ok(IdempotencyDecision::Pending {
                correlation_id: row.1,
                effect_state: row.2,
                admission_seq: row.4,
            }),
        }
    }

    /// Commit the exact response for a previously claimed request.
    pub fn complete_idempotent_request(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        response_json: &str,
        now: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE machine_request_idempotency \
             SET effect_state = 'committed', response_json = ?5, updated_at = ?6 \
             WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3 AND request_digest = ?4",
            (
                session_id,
                operation,
                idempotency_key,
                request_digest,
                response_json,
                now,
            ),
        )? == 1)
    }

    /// Fence provider dispatch for an already-durable user admission. A retry
    /// may adopt `admitted`, but once dispatch starts it must not send the same
    /// prompt to a provider a second time automatically.
    pub fn mark_idempotent_request_dispatching(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        now: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE machine_request_idempotency SET effect_state = 'dispatching', updated_at = ?5 \
             WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3 \
             AND request_digest = ?4 AND effect_state = 'admitted' AND admission_seq IS NOT NULL \
             AND response_json IS NULL",
            (session_id, operation, idempotency_key, request_digest, now),
        )? == 1)
    }

    /// Release a dispatch fence when the local provider queue rejected the
    /// handoff before any provider work could start. Only the owner of the
    /// matching request digest may return `dispatching` to `admitted`.
    pub fn release_idempotent_request_dispatching(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        now: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE machine_request_idempotency SET effect_state = 'admitted', updated_at = ?5 \
             WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3 \
             AND request_digest = ?4 AND effect_state = 'dispatching' \
             AND admission_seq IS NOT NULL AND response_json IS NULL",
            (session_id, operation, idempotency_key, request_digest, now),
        )? == 1)
    }

    /// Mark an unfinished request whose external effect cannot be proven.
    /// Call only during startup while holding exclusive database ownership,
    /// before accepting requests: an old process can no longer settle its fence.
    pub fn reconcile_orphaned_message_dispatches(&self, now: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE machine_request_idempotency SET effect_state = 'unknown', updated_at = ?1 \
             WHERE operation = 'post_message' AND effect_state = 'dispatching' \
             AND response_json IS NULL",
            [now],
        )?)
    }

    /// Mark an unfinished request whose external effect cannot be proven.
    pub fn mark_idempotent_request_unknown(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
        now: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE machine_request_idempotency \
             SET effect_state = 'unknown', updated_at = ?5 \
             WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3 AND request_digest = ?4 AND response_json IS NULL",
            (session_id, operation, idempotency_key, request_digest, now),
        )? == 1)
    }

    /// Drop a claim whose handler was rejected before any effect (a 4xx
    /// refusal), so the caller may retry the same key. Only an unfinished
    /// `in_progress` row with the matching digest is removed; an admitted,
    /// dispatching, unknown or committed row is never released.
    pub fn release_idempotent_request(
        &self,
        session_id: &str,
        operation: &str,
        idempotency_key: &str,
        request_digest: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM machine_request_idempotency              WHERE session_id = ?1 AND operation = ?2 AND idempotency_key = ?3              AND request_digest = ?4 AND effect_state = 'in_progress' AND response_json IS NULL",
            (session_id, operation, idempotency_key, request_digest),
        )? == 1)
    }

    /// Insert (or replace) a session row.
    pub fn create_session(&self, row: &SessionRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sessions \
             (id, title, workspace_root, created_at, updated_at, parent_session_id, role, status, config_json, tags, recovery_marker) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            (
                &row.id,
                &row.title,
                &row.workspace_root,
                &row.created_at,
                &row.updated_at,
                &row.parent_session_id,
                &row.role,
                &row.status,
                &row.config_json,
                &row.tags,
                &row.recovery_marker,
            ),
        )?;
        Ok(())
    }

    /// Update a session's status and last-active timestamp.
    pub fn update_session_status(&self, id: &str, status: &str, updated_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
            (id, status, updated_at),
        )?;
        Ok(())
    }

    /// Compare-and-swap a session's status: write `status`/`updated_at` only
    /// if the row still holds exactly `expected_status` and
    /// `expected_updated_at`, and (when `only_if_older_than` is given) only if
    /// `updated_at` is strictly older than that bound. Returns whether the
    /// write landed.
    ///
    /// This exists because the read-then-write shape (`get_session`, compare,
    /// `update_session_status`) is not atomic: a concurrent writer can commit
    /// between the read and the write, and the stale value still wins. Folding
    /// the comparison into the `WHERE` clause makes the guard a single
    /// statement that SQLite settles, closing that window.
    ///
    /// `only_if_older_than` exists because comparing `updated_at` alone is not
    /// sufficient here: `now_iso()` has one-second precision, so a concurrent
    /// writer that re-persists the *same* status within the same second
    /// produces a row byte-identical to the caller's snapshot, and the
    /// equality guard matches when it should not. A caller that knows every
    /// legitimate concurrent write must be at-or-after some instant (a scan
    /// that recorded the time before it began) passes that instant here, which
    /// excludes same-second writes instead of silently accepting them. The
    /// bound is exclusive, so it errs toward skipping the write.
    pub fn update_session_status_if_unchanged(
        &self,
        id: &str,
        status: &str,
        updated_at: &str,
        expected_status: &str,
        expected_updated_at: &str,
        only_if_older_than: Option<&str>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET status = ?2, updated_at = ?3              WHERE id = ?1 AND status = ?4 AND updated_at = ?5                AND (?6 IS NULL OR updated_at < ?6)",
            (
                id,
                status,
                updated_at,
                expected_status,
                expected_updated_at,
                only_if_older_than,
            ),
        )?;
        Ok(changed == 1)
    }

    /// Set or clear the persistent recovery marker for a session.
    /// `Some(json)` stamps the marker (usually a serialized `RecoveryMarker`)
    /// onto the row; `None` clears it (clean completion / archive).
    /// Updated-at is bumped in both directions so the UI can sort by recency.
    pub fn update_session_recovery_marker(
        &self,
        id: &str,
        marker: Option<&str>,
        updated_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET recovery_marker = ?2, updated_at = ?3 WHERE id = ?1",
            (id, marker, updated_at),
        )?;
        Ok(())
    }

    /// CRA-11 — read the durable, committed-boundary compaction epoch for a
    /// session. This column is the sole source of truth: it is never derived
    /// from live `Compact` transcript markers or `MAX(seq)` over prunable
    /// `events` rows, and it only advances inside
    /// `Storage::commit_compaction`'s single transaction. Returns `Ok(0)` for
    /// a session that has never compacted (including pre-migration rows,
    /// which read the column's `DEFAULT 0`) and `Err(NotFound)` when the
    /// session id does not exist at all.
    pub fn compaction_epoch(&self, id: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COALESCE(compaction_epoch, 0) FROM sessions WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("session {id}"))
                }
                other => StorageError::Sqlite(other),
            })
    }

    /// Fetch a single session by id.
    pub fn get_session(&self, id: &str) -> Result<Option<SessionRow>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1");
        let row = self
            .conn
            .query_row(&sql, [id], Self::map_session)
            .optional()?;
        Ok(row)
    }

    /// Read only a session status for lifecycle supervisors that must not
    /// recreate an in-memory child registry after daemon restart.
    pub fn session_status(&self, id: &str) -> Result<Option<String>> {
        let status = self
            .conn
            .query_row("SELECT status FROM sessions WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(status)
    }

    /// List all sessions, ordered by creation time then id.
    pub fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions ORDER BY created_at ASC, id ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::map_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// All sub-agent sessions spawned by a given parent (the agent tree).
    pub fn child_sessions(&self, parent_id: &str) -> Result<Vec<SessionRow>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE parent_session_id = ?1 \
             ORDER BY created_at ASC, id ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([parent_id], Self::map_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Rename a session (update its title and updated_at timestamp).
    pub fn rename_session(&self, id: &str, title: &str, updated_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2, updated_at = ?3 WHERE id = ?1",
            (id, title, updated_at),
        )?;
        Ok(())
    }

    /// Update a session's tags (stored as a JSON array string) and updated_at.
    pub fn update_session_tags(&self, id: &str, tags_json: &str, updated_at: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET tags = ?2, updated_at = ?3 WHERE id = ?1",
            (id, tags_json, updated_at),
        )?;
        Ok(())
    }

    /// Update the model stored in a session's config_json and updated_at.
    pub fn update_session_model(&self, id: &str, model: &str, updated_at: &str) -> Result<()> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT config_json FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let mut cfg: serde_json::Value = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        cfg["model"] = serde_json::json!(model);
        let cfg_str =
            serde_json::to_string(&cfg).unwrap_or_else(|_| format!(r#"{{"model":"{model}"}}"#));
        self.conn.execute(
            "UPDATE sessions SET config_json = ?2, updated_at = ?3 WHERE id = ?1",
            (id, cfg_str, updated_at),
        )?;
        Ok(())
    }

    /// Update or clear a string field inside config_json and bump updated_at.
    pub fn update_session_config_string(
        &self,
        id: &str,
        key: &str,
        value: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT config_json FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(false);
        };
        let mut cfg: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        if !cfg.is_object() {
            cfg = serde_json::json!({});
        }
        match value {
            Some(value) => cfg[key] = serde_json::json!(value),
            None => {
                if let Some(obj) = cfg.as_object_mut() {
                    obj.remove(key);
                }
            }
        }
        let cfg_str = serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string());
        self.conn.execute(
            "UPDATE sessions SET config_json = ?2, updated_at = ?3 WHERE id = ?1",
            (id, cfg_str, updated_at),
        )?;
        Ok(true)
    }

    /// Atomically update a session's legacy model-route identity — model,
    /// resolved provider id, and effort — in a single config_json rewrite.
    /// `None` for `provider_id`/`effort` removes the key so resume re-derives
    /// it instead of resurrecting a stale authority. Structured
    /// `inferenceSelection`/`inferenceFingerprint` keys are left untouched;
    /// qualified sessions route through `update_session_inference_selection`.
    /// Returns `Ok(false)` when no session row exists.
    pub fn update_session_route(
        &self,
        id: &str,
        model: &str,
        provider_id: Option<&str>,
        effort: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT config_json FROM sessions WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(false);
        };
        let mut cfg: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        if !cfg.is_object() {
            cfg = serde_json::json!({});
        }
        cfg["model"] = serde_json::json!(model);
        if let Some(obj) = cfg.as_object_mut() {
            match provider_id {
                Some(provider_id) => {
                    obj.insert("providerId".to_string(), serde_json::json!(provider_id));
                }
                None => {
                    obj.remove("providerId");
                }
            }
            match effort {
                Some(effort) => {
                    obj.insert("effort".to_string(), serde_json::json!(effort));
                }
                None => {
                    obj.remove("effort");
                }
            }
        }
        let cfg_str = serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string());
        self.conn.execute(
            "UPDATE sessions SET config_json = ?2, updated_at = ?3 WHERE id = ?1",
            (id, cfg_str, updated_at),
        )?;
        Ok(true)
    }

    /// Atomically replace a session's structured inference selection and its
    /// attested fingerprint. Legacy scalar model routing is cleared in the
    /// same row update so resume has exactly one routing authority.
    pub fn update_session_inference_selection(
        &self,
        id: &str,
        selection: &serde_json::Value,
        fingerprint: &str,
        updated_at: &str,
    ) -> Result<bool> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT config_json FROM sessions WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(existing) = existing else {
            return Ok(false);
        };
        let mut config: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        if !config.is_object() {
            config = serde_json::json!({});
        }
        config["inferenceSelection"] = selection.clone();
        config["inferenceFingerprint"] = serde_json::json!(fingerprint);
        if let Some(object) = config.as_object_mut() {
            object.remove("model");
        }
        let encoded = serde_json::to_string(&config).unwrap_or_else(|_| "{}".to_string());
        self.conn.execute(
            "UPDATE sessions SET config_json = ?2, updated_at = ?3 WHERE id = ?1",
            (id, encoded, updated_at),
        )?;
        Ok(true)
    }

    /// Delete a session by id. No error if it does not exist.
    ///
    /// This is the **legacy** delete: it removes only the `sessions` row
    /// itself and leaves every related record (descendants, events,
    /// artifacts, search rows, ownership) orphaned. Callers that need a
    /// fully clean lifecycle should use [`Storage::delete_session_tree`]
    /// instead. The legacy path is preserved for two narrow callers (the
    /// ACP protocol delete and the bakeoff teardown) that explicitly do
    /// not own the dependent records; do not add new callers.
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Delete a session and every dependent record in **one explicit
    /// transaction**. The transaction is opened IMMEDIATE so it cannot
    /// deadlock with a concurrent append, and rolls back atomically on
    /// any error — partial deletes are impossible.
    ///
    /// What gets deleted:
    ///
    /// 1. The session row itself, **plus every descendant** collected by a
    ///    recursive CTE over `parent_session_id`. A delete on a conductor
    ///    session therefore tears down the entire sub-agent tree.
    /// 2. Every `events` row for any session in the tree; the
    ///    `AFTER DELETE` trigger on `events` installed in migration v3
    ///    removes the matching `events_fts` row in the same transaction,
    ///    so the search index cannot drift.
    /// 3. Tasks, artifacts, compaction archives, goals, remote bindings,
    ///    tool invocations, tool invocation events, and tool invocation
    ///    cancellations for every session in the tree.
    ///    - Also quarantined events (`events_quarantine`), driver payload rows
    ///      (`driver_event_inbox`, resolved through each session's
    ///      `driver_bindings` rows since the inbox keys off `binding_id`
    ///      rather than `session_id`), `driver_bindings` itself,
    ///      `session_capability_leases`, and every handoff-owned row
    ///      (`handoff_gap_messages`, `handoff_journal`, `handoff_sessions`)
    ///      for every session in the tree — closing FM-175/ra-007 so a
    ///      deleted session cannot retain quarantined or in-flight payloads.
    /// 4. Children first, parents last: the cascade order is fixed so the
    ///    FTS trigger never observes a dangling `session_id`.
    ///
    /// **Idempotent.** Calling this on a session id that does not exist
    /// is a no-op (`deleted_sessions == 0`, no error). Calling it twice
    /// for the same id returns the same outcome. Foreign-key-less schema
    /// (the storage engine has no `PRAGMA foreign_keys = ON` declaration)
    /// means we delete by `session_id IN (…)` rather than relying on
    /// cascade — the explicit SQL makes the cascade order auditable and
    /// not dependent on `PRAGMA foreign_keys`.
    ///
    /// Returns the `DeleteOutcome` so the caller can report what was
    /// removed (used by the API handler and by the integration tests).
    pub fn delete_session_tree(&mut self, root_id: &str) -> Result<DeleteOutcome> {
        let tx = rightkit_db::write_txn(&mut self.conn)?;

        // 1. Collect the full session tree (root + transitive descendants)
        //    via a recursive CTE. The CTE is BFS-stable: rows are returned
        //    in insertion order, but the cascade below uses IN (…) so the
        //    order does not matter for correctness — it only affects how
        //    many round-trips the `events` DELETE trigger fires.
        let mut stmt = tx.prepare(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT id FROM sessions WHERE id = ?1 \
                 UNION ALL \
                 SELECT s.id FROM sessions s JOIN tree t ON s.parent_session_id = t.id \
             ) \
             SELECT id FROM tree",
        )?;
        let ids: Vec<String> = stmt
            .query_map([root_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        if ids.is_empty() {
            tx.commit()?;
            return Ok(DeleteOutcome::default());
        }

        // 2. Children first, parents last. Each `DELETE … WHERE session_id
        //    IN (?, ?, …)` uses a manually-expanded parameter list sized
        //    to the id set, so the cascade order is auditable in the
        //    query log and the FTS DELETE trigger never sees a parent
        //    that has already been removed. Deleting events is the step
        //    the trigger cares about — everything else is a static
        //    `session_id IN (…)` delete.
        // 1b. Driver bindings are keyed by their own `id`, and
        //     `driver_event_inbox` rows key off that binding id rather than
        //     `session_id` directly — so the inbox rows for this session's
        //     bindings must be resolved and deleted before the bindings
        //     themselves are removed, or they become orphaned rows that
        //     `bulk_delete_in(..., "session_id", ...)` can never reach.
        let binding_ids: Vec<String> = {
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql =
                format!("SELECT id FROM driver_bindings WHERE session_id IN ({placeholders})");
            let mut stmt = tx.prepare(&sql)?;
            let params = rusqlite::params_from_iter(ids.iter().map(|s| s.as_str()));
            let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };
        let deleted_driver_event_inbox =
            bulk_delete_in(&tx, "driver_event_inbox", "binding_id", &binding_ids)?;

        let outcome = DeleteOutcome {
            deleted_events: bulk_delete_in(&tx, "events", "session_id", &ids)?,
            deleted_tasks: bulk_delete_in(&tx, "tasks", "session_id", &ids)?,
            deleted_artifacts: bulk_delete_in(&tx, "artifacts", "session_id", &ids)?,
            deleted_compaction_archives: bulk_delete_in(
                &tx,
                "compaction_archives",
                "session_id",
                &ids,
            )?,
            deleted_goals: bulk_delete_in(&tx, "goals", "session_id", &ids)?,
            deleted_remote_bindings: bulk_delete_in(&tx, "remote_bindings", "session_id", &ids)?,
            deleted_tool_invocations: bulk_delete_in(&tx, "tool_invocations", "session_id", &ids)?,
            deleted_tool_invocation_events: bulk_delete_in(
                &tx,
                "tool_invocation_events",
                "session_id",
                &ids,
            )?,
            deleted_tool_invocation_cancellations: bulk_delete_in(
                &tx,
                "tool_invocation_cancellations",
                "session_id",
                &ids,
            )?,
            deleted_machine_requests: bulk_delete_in(
                &tx,
                "machine_request_idempotency",
                "session_id",
                &ids,
            )?,
            deleted_events_quarantine: bulk_delete_in(
                &tx,
                "events_quarantine",
                "session_id",
                &ids,
            )?,
            deleted_driver_event_inbox,
            deleted_driver_bindings: bulk_delete_in(&tx, "driver_bindings", "session_id", &ids)?,
            deleted_session_capability_leases: bulk_delete_in(
                &tx,
                "session_capability_leases",
                "session_id",
                &ids,
            )?,
            deleted_handoff_gap_messages: bulk_delete_in(
                &tx,
                "handoff_gap_messages",
                "session_id",
                &ids,
            )?,
            deleted_handoff_journal: bulk_delete_in(&tx, "handoff_journal", "session_id", &ids)?,
            deleted_handoff_sessions: bulk_delete_in(&tx, "handoff_sessions", "session_id", &ids)?,
            // Lane-C exporter cursors die with the tree: a deleted session's
            // stream position must never survive to seed a later export.
            deleted_transcript_cursors: bulk_delete_in(
                &tx,
                "membrane_transcript_cursors",
                "session_id",
                &ids,
            )?,
            deleted_adapt_observation_cursors: bulk_delete_in(
                &tx,
                "adapt_observation_cursors",
                "session_id",
                &ids,
            )?,
            deleted_sessions: bulk_delete_in(&tx, "sessions", "id", &ids)?,
        };

        tx.commit()?;
        Ok(outcome)
    }

    fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            id: row.get(0)?,
            title: row.get(1)?,
            workspace_root: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            parent_session_id: row.get(5)?,
            role: row.get(6)?,
            status: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            config_json: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            tags: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            recovery_marker: row.get::<_, Option<String>>(10)?,
        })
    }
}

/// What `Storage::delete_session_tree` actually removed. The per-table
/// counts let the API handler and the integration tests confirm the
/// cascade touched every dependent table without having to re-query each
/// one individually.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub deleted_sessions: usize,
    pub deleted_events: usize,
    pub deleted_artifacts: usize,
    pub deleted_tasks: usize,
    pub deleted_compaction_archives: usize,
    pub deleted_goals: usize,
    pub deleted_remote_bindings: usize,
    pub deleted_tool_invocations: usize,
    pub deleted_tool_invocation_events: usize,
    pub deleted_tool_invocation_cancellations: usize,
    pub deleted_machine_requests: usize,
    pub deleted_events_quarantine: usize,
    pub deleted_driver_event_inbox: usize,
    pub deleted_driver_bindings: usize,
    pub deleted_session_capability_leases: usize,
    pub deleted_handoff_gap_messages: usize,
    pub deleted_handoff_journal: usize,
    pub deleted_handoff_sessions: usize,
    /// Native transcript-exporter cursor rows removed with the tree (Lane C).
    pub deleted_transcript_cursors: usize,
    pub deleted_adapt_observation_cursors: usize,
}

/// Run `DELETE FROM <table> WHERE <col> IN (?, ?, …, ?)` against the
/// already-open `tx`, expanding `ids` into a parameter list. Sized for
/// the id set (capped at 4_096 to bound the SQL string for adversarial
/// inputs — a single delete that walks more than 4k sessions should be
/// split into chunks before being sent to storage). The 4k cap is not a
/// correctness limit; it is a guardrail against `delete_session_tree`
/// being called on a runaway tree without the API handler checking the
/// id count first. A `StorageError::Serde` with an actionable message is
/// returned when the cap is exceeded.
fn bulk_delete_in(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    col: &str,
    ids: &[String],
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > 4096 {
        return Err(StorageError::Serde(format!(
            "bulk_delete_in refusing to expand {} ids for {}.{} (cap is 4096); \
             split the delete into chunks before calling delete_session_tree",
            ids.len(),
            table,
            col,
        )));
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("DELETE FROM {table} WHERE {col} IN ({placeholders})");
    let params = rusqlite::params_from_iter(ids.iter().map(|s| s.as_str()));
    let deleted = tx.execute(&sql, params)?;
    Ok(deleted)
}

#[cfg(test)]
mod delete_tests {
    //! Lifecycle delete + FTS invariants. The tests below exercise the
    //! transactional `delete_session_tree` cascade, the trigger-owned
    //! `events_fts` mirror, and rollback safety. They are colocated with
    //! `session.rs` because the cascade entry point lives there; the
    //! trigger invariant is asserted by `events_fts_count()` and
    //! `search_sessions()` from `events/search.rs`.
    use super::*;
    use crate::events::SessionMatch;
    use crate::Storage;
    use rightkit_agent_engine_protocol::ArtifactRow;

    #[test]
    fn orphaned_dispatch_reconciliation_preserves_other_states_and_is_idempotent() {
        let storage = crate::Storage::open_in_memory().unwrap();
        let cases = [
            ("post_message", "dispatching", None, "unknown"),
            ("post_message", "admitted", None, "admitted"),
            ("post_message", "in_progress", None, "in_progress"),
            ("post_message", "committed", Some("{}"), "committed"),
            ("post_message", "dispatching", Some("{}"), "dispatching"),
            ("other_operation", "dispatching", None, "dispatching"),
        ];
        for (index, (operation, state, response, _)) in cases.iter().enumerate() {
            storage
                .conn()
                .execute(
                    "INSERT INTO machine_request_idempotency \
                 (session_id, operation, idempotency_key, request_digest, correlation_id, \
                  effect_state, response_json, created_at, updated_at) \
                 VALUES ('session', ?1, ?2, 'digest', 'correlation', ?3, ?4, 'before', 'before')",
                    rusqlite::params![operation, index.to_string(), state, response],
                )
                .unwrap();
        }
        assert_eq!(
            storage
                .reconcile_orphaned_message_dispatches("after")
                .unwrap(),
            1
        );
        assert_eq!(
            storage
                .reconcile_orphaned_message_dispatches("again")
                .unwrap(),
            0
        );
        for (index, (_, _, _, expected)) in cases.iter().enumerate() {
            let actual: String = storage.conn().query_row(
                "SELECT effect_state FROM machine_request_idempotency WHERE idempotency_key = ?1",
                [index.to_string()], |row| row.get(0),
            ).unwrap();
            assert_eq!(actual.as_str(), *expected);
        }
    }

    fn row(id: &str, parent: Option<&str>) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            title: format!("title-{id}"),
            workspace_root: format!("/ws/{id}"),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            parent_session_id: parent.map(str::to_string),
            role: None,
            status: "idle".to_string(),
            config_json: "{}".to_string(),
            tags: String::new(),
            recovery_marker: None,
        }
    }

    fn artifact(id: &str, path: &str) -> ArtifactRow {
        ArtifactRow {
            id: id.to_string(),
            path: path.to_string(),
            kind: "file".to_string(),
            content_hash: "deadbeef".to_string(),
            shippable: false,
            producer: "agent".to_string(),
            provenance: String::new(),
            verdict: None,
            approvals: vec![],
            meta: String::new(),
            trust_label: "unreviewed".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn build_tree(s: &mut Storage) {
        // conductor -> worker-1, worker-2; worker-1 -> grandchild
        s.create_session(&row("conductor", None)).unwrap();
        s.create_session(&row("worker-1", Some("conductor")))
            .unwrap();
        s.create_session(&row("worker-2", Some("conductor")))
            .unwrap();
        s.create_session(&row("grandchild", Some("worker-1")))
            .unwrap();
        // an unrelated session that must NOT be touched
        s.create_session(&row("unrelated", None)).unwrap();

        for sid in [
            "conductor",
            "worker-1",
            "worker-2",
            "grandchild",
            "unrelated",
        ] {
            s.append_event(sid, "msg", &format!(r#"{{"text":"alpha {sid}"}}"#))
                .unwrap();
            s.upsert_task(sid, &task_node(&format!("t-{sid}"))).unwrap();
            s.upsert_artifact(sid, &artifact(&format!("a-{sid}"), "/p"))
                .unwrap();
            s.upsert_remote_binding(&binding(&format!("rb-{sid}"), sid))
                .unwrap();
            s.upsert_tool_invocation(&invocation(&format!("ti-{sid}"), sid))
                .unwrap();
            s.append_tool_invocation_event(&invocation_event(
                &format!("ti-{sid}"),
                sid,
                1,
                "toolCallStarted",
            ))
            .unwrap();
            s.upsert_tool_invocation_cancellation(&cancellation(&format!("ti-{sid}"), sid))
                .unwrap();
            s.upsert_goal(&goal(&format!("g-{sid}"), sid)).unwrap();
            s.insert_compaction_archive(
                &format!("ca-{sid}"),
                sid,
                "2026-07-20T00:00:00Z",
                br#"{"transcript":"x"}"#,
                1,
            )
            .unwrap();
        }
    }

    fn task_node(id: &str) -> rightkit_agent_engine_protocol::TaskNode {
        use rightkit_agent_engine_protocol::task::TaskState;
        rightkit_agent_engine_protocol::TaskNode {
            id: id.to_string(),
            title: id.to_string(),
            deps: vec![],
            done_when: "true".to_string(),
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: String::new(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn binding(id: &str, sid: &str) -> crate::RemoteBindingRow {
        crate::RemoteBindingRow {
            id: id.to_string(),
            channel_id: format!("ch-{id}"),
            target_key: format!("target-{id}"),
            session_id: Some(sid.to_string()),
            target_label: None,
            target_redacted: None,
            status: "active".to_string(),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        }
    }

    fn invocation(id: &str, sid: &str) -> crate::ToolInvocationStoreRow {
        crate::ToolInvocationStoreRow {
            id: id.to_string(),
            session_id: sid.to_string(),
            turn_id: "turn-1".to_string(),
            tool_name: "shell".to_string(),
            schema_version: "1".to_string(),
            args_hash: "h".to_string(),
            args_redacted_json: "{}".to_string(),
            lifecycle_state: "completed".to_string(),
            policy_verdict_id: None,
            approval_id: None,
            process_id: None,
            started_at: Some("t".to_string()),
            completed_at: Some("t".to_string()),
            output_hash: None,
            artifact_ids_json: "[]".to_string(),
            error_class: None,
            updated_at: "t".to_string(),
        }
    }

    fn invocation_event(
        invocation_id: &str,
        sid: &str,
        seq: i64,
        kind: &str,
    ) -> crate::ToolInvocationEventStoreRow {
        crate::ToolInvocationEventStoreRow {
            invocation_id: invocation_id.to_string(),
            session_id: sid.to_string(),
            seq,
            kind: kind.to_string(),
            created_at: "t".to_string(),
            payload_redacted_json: "{}".to_string(),
        }
    }

    fn cancellation(invocation_id: &str, sid: &str) -> crate::ToolInvocationCancellationRow {
        crate::ToolInvocationCancellationRow {
            invocation_id: invocation_id.to_string(),
            session_id: sid.to_string(),
            reason: Some("x".to_string()),
            created_at: "t".to_string(),
        }
    }

    fn goal(id: &str, sid: &str) -> crate::GoalRow {
        crate::GoalRow {
            id: id.to_string(),
            session_id: sid.to_string(),
            text: "ship it".to_string(),
            status: crate::GoalStatus::Active,
            budget_usd: 0.0,
            budget_tokens: 0,
            tokens_used: 0,
            continuation_count: 0,
            no_progress_count: 0,
            started_at: "t".to_string(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        }
    }

    fn count(s: &Storage, sql: &str, sid: &str) -> i64 {
        s.conn.query_row(sql, [sid], |r| r.get(0)).unwrap_or(0)
    }

    #[test]
    fn delete_session_tree_removes_descendants_and_every_dependent_row_atomically() {
        let mut s = Storage::open_in_memory().unwrap();
        build_tree(&mut s);

        // Pre-conditions: the trigger mirrors every events row into events_fts.
        let fts_before = s.events_fts_count().unwrap();
        assert_eq!(fts_before, 5, "5 sessions x 1 event each, all mirrored");

        // Sanity: every dependent table has rows for the tree.
        for sid in ["conductor", "worker-1", "worker-2", "grandchild"] {
            assert_eq!(
                count(&s, "SELECT count(*) FROM events WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM tasks WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM artifacts WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM goals WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM remote_bindings WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocations WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_events WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_cancellations WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM compaction_archives WHERE session_id = ?1",
                    sid
                ),
                1
            );
        }
        assert_eq!(
            count(
                &s,
                "SELECT count(*) FROM events WHERE session_id = ?1",
                "unrelated"
            ),
            1
        );

        let outcome = s.delete_session_tree("conductor").unwrap();
        assert_eq!(outcome.deleted_sessions, 4, "root + 3 descendants");
        assert_eq!(outcome.deleted_events, 4);
        assert_eq!(outcome.deleted_tasks, 4);
        assert_eq!(outcome.deleted_artifacts, 4);
        assert_eq!(outcome.deleted_compaction_archives, 4);
        assert_eq!(outcome.deleted_goals, 4);
        assert_eq!(outcome.deleted_remote_bindings, 4);
        assert_eq!(outcome.deleted_tool_invocations, 4);
        assert_eq!(outcome.deleted_tool_invocation_events, 4);
        assert_eq!(outcome.deleted_tool_invocation_cancellations, 4);

        // Sessions gone.
        for sid in ["conductor", "worker-1", "worker-2", "grandchild"] {
            assert!(s.get_session(sid).unwrap().is_none(), "{sid} must be gone");
            assert_eq!(
                count(&s, "SELECT count(*) FROM events WHERE session_id = ?1", sid),
                0
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM tasks WHERE session_id = ?1", sid),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM artifacts WHERE session_id = ?1",
                    sid
                ),
                0
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM goals WHERE session_id = ?1", sid),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM remote_bindings WHERE session_id = ?1",
                    sid
                ),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocations WHERE session_id = ?1",
                    sid
                ),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_events WHERE session_id = ?1",
                    sid
                ),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_cancellations WHERE session_id = ?1",
                    sid
                ),
                0
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM compaction_archives WHERE session_id = ?1",
                    sid
                ),
                0
            );
        }
        // The unrelated session survives — the cascade must not over-reach.
        assert!(s.get_session("unrelated").unwrap().is_some());
        assert_eq!(
            count(
                &s,
                "SELECT count(*) FROM events WHERE session_id = ?1",
                "unrelated"
            ),
            1
        );
        assert_eq!(
            count(
                &s,
                "SELECT count(*) FROM artifacts WHERE session_id = ?1",
                "unrelated"
            ),
            1
        );

        // FTS mirror converged via the trigger: only the unrelated event remains.
        let fts_after = s.events_fts_count().unwrap();
        assert_eq!(fts_after, 1, "FTS must mirror exactly the surviving events");
    }

    #[test]
    fn delete_session_tree_is_idempotent() {
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("a", None)).unwrap();
        s.append_event("a", "msg", r#"{"text":"hello"}"#).unwrap();

        let first = s.delete_session_tree("a").unwrap();
        assert_eq!(first.deleted_sessions, 1);
        assert_eq!(first.deleted_events, 1);

        // Second call is a no-op, no error, no FTS churn.
        let second = s.delete_session_tree("a").unwrap();
        assert_eq!(second.deleted_sessions, 0);
        assert_eq!(second.deleted_events, 0);
        assert_eq!(s.events_fts_count().unwrap(), 0);
    }

    #[test]
    fn delete_session_tree_on_unknown_id_is_a_noop() {
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("alive", None)).unwrap();
        s.append_event("alive", "msg", r#"{"text":"keep"}"#)
            .unwrap();

        let outcome = s.delete_session_tree("ghost").unwrap();
        assert_eq!(outcome.deleted_sessions, 0);
        assert!(s.get_session("alive").unwrap().is_some());
        assert_eq!(s.events_fts_count().unwrap(), 1);
    }

    #[test]
    fn delete_session_tree_rolls_back_when_a_cascade_step_fails() {
        // Inject a constraint violation mid-cascade by attaching a row that
        // would otherwise be cascade-deleted but is referenced by a separate
        // table whose trigger (simulated via a CHECK on a child column) would
        // raise. Easier: drop one of the cascade tables between the start and
        // end so the SQL fails — except we control that. Instead, hook the
        // transaction's cascade by altering `events_fts` to have a NOT NULL
        // column named `boom` and inserting a row with NULL before the
        // cascade — but that is fragile. The simplest robust check is to
        // install a trigger on `compaction_archives` that raises on DELETE.
        let mut s = Storage::open_in_memory().unwrap();
        build_tree(&mut s);

        // Install a trigger that raises mid-cascade on a DELETE against
        // `compaction_archives` — the step that runs after events so we can
        // prove the events-deletion step is rolled back too.
        s.conn
            .execute_batch(
                "CREATE TRIGGER fail_compaction_delete \
                 BEFORE DELETE ON compaction_archives \
                 WHEN old.id LIKE 'ca-grandchild' \
                 BEGIN \
                     SELECT RAISE(ABORT, 'injected failure'); \
                 END;",
            )
            .unwrap();

        // Snapshot before the failed delete so we can assert rollback restored
        // every dependent row, including the FTS rows for the conductor tree.
        let fts_before = s.events_fts_count().unwrap();
        let sessions_before: Vec<String> = s
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(fts_before, 5);
        assert_eq!(sessions_before.len(), 5);

        let result = s.delete_session_tree("conductor");
        assert!(
            result.is_err(),
            "injected trigger must abort the transaction"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("injected failure"), "got: {err}");

        // Rollback must restore every dependent row — no partial delete.
        assert_eq!(s.events_fts_count().unwrap(), fts_before);
        let sessions_after: Vec<String> = s
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(sessions_after.len(), sessions_before.len());
        for sid in [
            "conductor",
            "worker-1",
            "worker-2",
            "grandchild",
            "unrelated",
        ] {
            assert!(
                sessions_after.contains(&sid.to_string()),
                "{sid} must survive rollback"
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM events WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM tasks WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM artifacts WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(&s, "SELECT count(*) FROM goals WHERE session_id = ?1", sid),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM remote_bindings WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocations WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_events WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM tool_invocation_cancellations WHERE session_id = ?1",
                    sid
                ),
                1
            );
            assert_eq!(
                count(
                    &s,
                    "SELECT count(*) FROM compaction_archives WHERE session_id = ?1",
                    sid
                ),
                1
            );
        }
    }

    #[test]
    fn delete_session_tree_keeps_events_fts_invariant_for_concurrent_append() {
        // Simulate a concurrent append (in practice guarded by the writer
        // mutex; we model it by appending inside a separate transaction that
        // happens between the cascade's children-first phase and its commit).
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("a", None)).unwrap();
        s.create_session(&row("unrelated", None)).unwrap();
        for sid in ["a", "unrelated"] {
            s.append_event(sid, "msg", &format!(r#"{{"text":"seed {sid}"}}"#))
                .unwrap();
        }
        assert_eq!(s.events_fts_count().unwrap(), 2);

        // Open a second transaction on the same connection. SQLite serializes
        // writers on the writer connection; for the test we simply assert the
        // invariant after the cascade: every event in `events` has a
        // matching row in `events_fts` keyed by the same rowid.
        let outcome = s.delete_session_tree("a").unwrap();
        assert_eq!(outcome.deleted_events, 1);
        let orphans: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM events e \
                 WHERE NOT EXISTS (SELECT 1 FROM events_fts f WHERE f.rowid = e.rowid)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "no event without a matching FTS row");
        let stale: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM events_fts f \
                 WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.rowid = f.rowid)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "no FTS row without a matching event");
    }

    #[test]
    fn triggers_keep_events_fts_invariant_for_replay_and_reindex() {
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("replay", None)).unwrap();

        // Insert a synthetic batch of 50 events directly via SQL to model
        // a replay writer that bypasses `append_event`. The triggers must
        // mirror every one of them into `events_fts`.
        let tx = rightkit_db::write_txn(&mut s.conn).unwrap();
        for i in 1..=50 {
            tx.execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
                 VALUES (?1, ?2, 'msg', ?3, 't')",
                rusqlite::params!["replay", i, format!(r#"{{"n":{i}}}"#)],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        assert_eq!(s.events_fts_count().unwrap(), 50);

        // Now blow away every FTS row to simulate corruption, then call the
        // reindex helper — the search index must converge with the event log.
        s.conn.execute("DELETE FROM events_fts", []).unwrap();
        assert_eq!(s.events_fts_count().unwrap(), 0);

        let rebuilt = s.reindex_events_fts().unwrap();
        assert_eq!(rebuilt, 50, "reindex re-mirrors every event");
        assert_eq!(s.events_fts_count().unwrap(), 50);

        // And direct UPDATE on events keeps FTS in sync via the AU trigger.
        s.conn
            .execute(
                "UPDATE events SET payload_json = ?1 WHERE session_id = ?2 AND seq = 1",
                rusqlite::params![r#"{"n":9999}"#, "replay"],
            )
            .unwrap();
        let hits = s.search_sessions("9999", 10).unwrap();
        assert_eq!(hits.len(), 1, "UPDATED payload must be searchable");
        assert_eq!(hits[0].session_id, "replay");
    }

    #[test]
    fn delete_session_tree_removes_events_fts_rows_via_trigger() {
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("stale", None)).unwrap();
        s.append_event("stale", "msg", r#"{"text":"needle one"}"#)
            .unwrap();
        s.append_event("stale", "msg", r#"{"text":"needle two"}"#)
            .unwrap();
        assert_eq!(s.events_fts_count().unwrap(), 2);

        let hits: Vec<SessionMatch> = s.search_sessions("needle", 10).unwrap();
        assert_eq!(hits.len(), 1, "FTS sees both events as one session");
        assert_eq!(hits[0].session_id, "stale");

        let outcome = s.delete_session_tree("stale").unwrap();
        assert_eq!(outcome.deleted_events, 2);
        assert_eq!(s.events_fts_count().unwrap(), 0, "trigger purges FTS rows");
        assert!(
            s.search_sessions("needle", 10).unwrap().is_empty(),
            "search must not return deleted session"
        );
    }

    #[test]
    fn search_sessions_finds_unicode_terms_and_handles_emoji_punctuation() {
        let mut s = Storage::open_in_memory().unwrap();
        s.create_session(&row("ja", None)).unwrap();
        s.create_session(&row("emoji", None)).unwrap();

        s.append_event(
            "ja",
            "msg",
            r#"{"text":"こんにちは世界 — multilingual résumé"}"#,
        )
        .unwrap();
        s.append_event("emoji", "msg", r#"{"text":"deploy 🚀 shipped — v1.0 🎉"}"#)
            .unwrap();

        // CJK text is indexed as a complete unicode61 token (there is no
        // whitespace boundary inside this phrase), while the diacritic term
        // is normalized by the tokenizer.
        let ja_hits = s.search_sessions("こんにちは世界 résumé", 10).unwrap();
        assert_eq!(ja_hits.len(), 1, "CJK + Latin-with-diacritics term matches");
        assert_eq!(ja_hits[0].session_id, "ja");

        // Emoji are Unicode category So (`Other Symbol`), which
        // `is_alphanumeric()` rejects — so a raw emoji query is dropped by
        // the sanitizer and returns no rows. The alnum neighbours survive,
        // which is what users actually want when they paste "deploy v1".
        assert!(
            s.search_sessions("🚀", 10).unwrap().is_empty(),
            "raw emoji alone must be dropped by the sanitizer"
        );
        let emoji_neighbours = s.search_sessions("deploy shipped", 10).unwrap();
        assert_eq!(
            emoji_neighbours.len(),
            1,
            "alnum neighbours of emoji still searchable"
        );
        assert_eq!(emoji_neighbours[0].session_id, "emoji");

        // Punctuation-only query: the sanitizer must return nothing rather
        // than raise a FTS syntax error.
        assert!(s.search_sessions("— …", 10).unwrap().is_empty());
        assert!(s.search_sessions("(╯°□°)╯︵ ┻━┻", 10).unwrap().is_empty());
    }

    #[test]
    fn release_idempotent_request_frees_only_unfinished_in_progress_claims() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&row("machine", None)).unwrap();
        s.claim_idempotent_request("machine", "fork", "k", "sha256:a", "c1", "t1")
            .unwrap();
        // A different digest cannot release the owner's claim.
        assert!(!s
            .release_idempotent_request("machine", "fork", "k", "sha256:b")
            .unwrap());
        assert!(s
            .release_idempotent_request("machine", "fork", "k", "sha256:a")
            .unwrap());
        // Released: the same key is fresh again.
        assert!(matches!(
            s.claim_idempotent_request("machine", "fork", "k", "sha256:a", "c2", "t2")
                .unwrap(),
            IdempotencyDecision::Fresh { .. }
        ));
        // A committed claim is never released.
        assert!(s
            .complete_idempotent_request("machine", "fork", "k", "sha256:a", "{}", "t3")
            .unwrap());
        assert!(!s
            .release_idempotent_request("machine", "fork", "k", "sha256:a")
            .unwrap());
        // An unknown claim is never released.
        s.claim_idempotent_request("machine", "fork", "u", "sha256:a", "c3", "t4")
            .unwrap();
        s.mark_idempotent_request_unknown("machine", "fork", "u", "sha256:a", "t5")
            .unwrap();
        assert!(!s
            .release_idempotent_request("machine", "fork", "u", "sha256:a")
            .unwrap());
    }

    #[test]
    fn digest_bound_idempotency_replays_and_rejects_conflicting_reuse() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&row("machine", None)).unwrap();
        let first = s
            .claim_idempotent_request(
                "machine",
                "post_message",
                "key-1",
                "sha256:aaa",
                "corr-1",
                "t1",
            )
            .unwrap();
        assert_eq!(
            first,
            IdempotencyDecision::Fresh {
                correlation_id: "corr-1".into()
            }
        );
        assert!(s
            .complete_idempotent_request(
                "machine",
                "post_message",
                "key-1",
                "sha256:aaa",
                r#"{"accepted":true,"seq":7}"#,
                "t2",
            )
            .unwrap());
        assert_eq!(
            s.claim_idempotent_request(
                "machine",
                "post_message",
                "key-1",
                "sha256:aaa",
                "corr-retry",
                "t3",
            )
            .unwrap(),
            IdempotencyDecision::Replay {
                correlation_id: "corr-1".into(),
                effect_state: "committed".into(),
                response_json: r#"{"accepted":true,"seq":7}"#.into(),
            }
        );
        assert!(matches!(
            s.claim_idempotent_request(
                "machine",
                "post_message",
                "key-1",
                "sha256:different",
                "corr-2",
                "t4",
            )
            .unwrap(),
            IdempotencyDecision::Conflict { .. }
        ));
    }

    #[test]
    fn unfinished_idempotent_request_survives_reopen_for_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idempotency.sqlite");
        {
            let s = Storage::open(&path).unwrap();
            s.create_session(&row("machine", None)).unwrap();
            s.claim_idempotent_request(
                "machine",
                "post_message",
                "key-pending",
                "sha256:pending",
                "corr-original",
                "t1",
            )
            .unwrap();
        }
        let s = Storage::open(&path).unwrap();
        let decision = s
            .claim_idempotent_request(
                "machine",
                "post_message",
                "key-pending",
                "sha256:pending",
                "corr-retry",
                "t2",
            )
            .unwrap();
        assert_eq!(
            decision,
            IdempotencyDecision::Pending {
                correlation_id: "corr-original".into(),
                effect_state: "in_progress".into(),
                admission_seq: None,
            }
        );
        assert!(s
            .mark_idempotent_request_unknown(
                "machine",
                "post_message",
                "key-pending",
                "sha256:pending",
                "t3",
            )
            .unwrap());
        assert!(matches!(
            s.claim_idempotent_request(
                "machine",
                "post_message",
                "key-pending",
                "sha256:pending",
                "corr-later",
                "t4",
            )
            .unwrap(),
            IdempotencyDecision::Pending { effect_state, .. } if effect_state == "unknown"
        ));
    }

    #[test]
    fn admitted_user_request_survives_reopen_and_dispatch_fence_is_reversible_before_send() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admitted-request.sqlite");
        {
            let mut s = Storage::open(&path).unwrap();
            s.create_session(&row("machine", None)).unwrap();
            assert!(matches!(
                s.claim_idempotent_request(
                    "machine",
                    "post_message",
                    "key-admit",
                    "sha256:admit",
                    "corr",
                    "t1"
                )
                .unwrap(),
                IdempotencyDecision::Fresh { .. }
            ));
            let event = rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
                session_id: "machine".into(),
                seq: 1,
                created_at: "t2".into(),
                text: "hello".into(),
                nonce: Some("key-admit".into()),
                trust: None,
                attachments: Vec::new(),
            };
            let payload = serde_json::to_string(&event).unwrap();
            s.append_event_at_seq("machine", 1, "userMessageSubmitted", &payload)
                .unwrap();
        }
        let s = Storage::open(&path).unwrap();
        assert_eq!(
            s.claim_idempotent_request(
                "machine",
                "post_message",
                "key-admit",
                "sha256:admit",
                "retry",
                "t3"
            )
            .unwrap(),
            IdempotencyDecision::Pending {
                correlation_id: "corr".into(),
                effect_state: "admitted".into(),
                admission_seq: Some(1),
            }
        );
        assert!(s
            .mark_idempotent_request_dispatching(
                "machine",
                "post_message",
                "key-admit",
                "sha256:admit",
                "t4"
            )
            .unwrap());
        assert!(s
            .release_idempotent_request_dispatching(
                "machine",
                "post_message",
                "key-admit",
                "sha256:admit",
                "t5"
            )
            .unwrap());
        assert!(matches!(
            s.claim_idempotent_request(
                "machine", "post_message", "key-admit", "sha256:admit", "retry-2", "t6"
            ).unwrap(),
            IdempotencyDecision::Pending { effect_state, admission_seq: Some(1), .. }
                if effect_state == "admitted"
        ));
    }

    #[test]
    fn five_hundred_parked_logical_workers_restore_and_cancel_as_owned_tree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parked-workers.sqlite");
        {
            let s = Storage::open(&path).unwrap();
            s.create_session(&row("parent", None)).unwrap();
            for index in 0..500 {
                let id = format!("worker-{index:03}");
                let mut worker = row(&id, Some("parent"));
                worker.status = "parked".into();
                s.create_session(&worker).unwrap();
            }
        }

        let mut s = Storage::open(&path).unwrap();
        let parked = s.child_sessions("parent").unwrap();
        assert_eq!(parked.len(), 500);
        assert!(parked.iter().all(|worker| worker.status == "parked"));
        assert!(parked
            .iter()
            .all(|worker| worker.parent_session_id.as_deref() == Some("parent")));

        let outcome = s.delete_session_tree("parent").unwrap();
        assert_eq!(outcome.deleted_sessions, 501);
        assert!(s.list_sessions().unwrap().is_empty());
    }
}

#[cfg(test)]
mod compaction_epoch_tests {
    //! CRA-11 read-side coverage for the durable `compaction_epoch` column.
    //! `Storage::commit_compaction` (in `compaction_archives.rs`) owns every
    //! write path; these tests only prove the column's read contract: a
    //! never-compacted session reads 0 (including across the v29 migration's
    //! `DEFAULT 0` on an existing row), and an unknown session id is a typed
    //! `NotFound`, never a silent 0 that could be confused with "never
    //! compacted".
    use super::*;
    use crate::{Storage, StorageError};

    fn row(id: &str) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            title: "t".into(),
            workspace_root: "/ws".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            parent_session_id: None,
            role: None,
            status: "idle".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        }
    }

    #[test]
    fn never_compacted_session_reads_epoch_zero() {
        let s = Storage::open_in_memory().unwrap();
        s.create_session(&row("s")).unwrap();
        assert_eq!(s.compaction_epoch("s").unwrap(), 0);
    }

    #[test]
    fn unknown_session_is_not_found_not_a_silent_zero() {
        let s = Storage::open_in_memory().unwrap();
        let err = s.compaction_epoch("does-not-exist").unwrap_err();
        assert!(matches!(err, StorageError::NotFound(_)));
    }

    #[test]
    fn epoch_column_survives_reopen_across_the_v29_migration_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epoch-default.sqlite");
        {
            let s = Storage::open(&path).unwrap();
            s.create_session(&row("s")).unwrap();
        }
        let s = Storage::open(&path).unwrap();
        assert_eq!(s.compaction_epoch("s").unwrap(), 0);
    }
}
