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
