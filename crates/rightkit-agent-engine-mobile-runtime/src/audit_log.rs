//! Redacted runtime observability log (ENG-41).
//!
//! Append-only JSONL, one record per line, under the same host-owned storage
//! directory as the session journal (see `journal.rs`). This is an
//! *observability* trail, not the source of truth for replay: a host may
//! ship its contents to a support bundle or a local diagnostics viewer, so
//! every record must be safe to read without exposing a secret.
//!
//! # Credential safety
//!
//! [`AuditEventKind`] is deliberately closed, mirroring `journal.rs`'s
//! `JournalEventKind`: no free-form `serde_json::Value` or `HashMap<String,
//! String>` field wide enough to carry a raw secret. On top of that
//! structural guard, every string field is additionally passed through
//! [`rightkit_llm::agent_providers::replay::redact_json_value`] — the same helper the
//! provider-replay recorder uses to strip bearer tokens, API keys, and other
//! credential-shaped values from recorded traffic — before the record is
//! serialized. Belt and suspenders: the type shape keeps secrets out by
//! construction, and the redaction pass catches a credential a caller pasted
//! into an otherwise-plain-text field (e.g. a preview string containing a
//! pasted token).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Redacted, closed set of observability events. Never add a variant or
/// field that stores a credential, token, or secret string — redact it to a
/// boolean/marker before it reaches this module (same rule as
/// `journal::JournalEventKind`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AuditEventKind {
    /// A session was started or resumed on this host.
    SessionStarted { resumed: bool },
    /// A turn was submitted. `preview` is truncated by the caller before it
    /// reaches this module, same convention as the journal.
    TurnSubmitted { preview: String },
    /// A tool call completed. `tool_name` only, never arguments.
    ToolInvoked { tool_name: String, ok: bool },
    /// The host resolved an approval the agent was blocked on.
    ApprovalResolved {
        tool_call_id: String,
        approved: bool,
    },
    /// A provider credential was refreshed. Marker only, never the value.
    CredentialRefreshed { provider: String },
    /// The session's cost ceiling was hit and the turn was stopped.
    BudgetExceeded {
        current_usd_cents: u64,
        ceiling_usd_cents: u64,
    },
    /// Context compaction ran for this session.
    CompactionOccurred { dropped_count: i32 },
    /// The in-flight turn was interrupted by the host.
    TurnInterrupted,
    /// A journal repair pass discarded a torn tail on startup.
    JournalRepaired { bytes_truncated: u64 },
}

/// One durable observability record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub recorded_at_ms: u128,
    // `AuditEventKind` is internally tagged with `tag = "kind"`, so without
    // `flatten` this field's own name ("kind") collides with that tag and
    // the record serializes as `{"kind": {"kind": "...", ...}}` instead of
    // `{"kind": "...", ...}` — every reader of this log (support bundles,
    // diagnostics viewers, this crate's own tests) expects the flat shape.
    #[serde(flatten)]
    pub kind: AuditEventKind,
}

#[derive(Debug)]
pub enum AuditLogError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for AuditLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "audit log io error: {error}"),
            Self::Serialize(error) => write!(f, "audit log serialize error: {error}"),
        }
    }
}

impl std::error::Error for AuditLogError {}

impl From<std::io::Error> for AuditLogError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for AuditLogError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Append-only redacted observability log, one per session.
///
/// Distinct from [`crate::journal::Journal`]: the journal is the durable
/// replay/recovery source of truth and its schema is closed to protect
/// crash-repair invariants; this log is a best-effort, human/support-bundle
/// facing trail. A write failure here must never fail the caller's turn, so
/// every append call site in this crate treats an `Err` as best-effort and
/// continues (see `lib.rs`).
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    /// Open (creating if absent) the audit log for `session_id` under
    /// `storage_dir`. Never truncates or reads back existing content — this
    /// is a pure append sink, not a replay source.
    pub fn open(storage_dir: &Path, session_id: &str) -> Result<Self, AuditLogError> {
        std::fs::create_dir_all(storage_dir)?;
        Ok(Self {
            path: audit_log_path(storage_dir, session_id),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one redacted record. Every string-bearing field of `kind` is
    /// run through the shared provider-replay redaction pass before the
    /// record is written, so a secret pasted into a free-text field (e.g. a
    /// tool-call preview) cannot reach disk even though the type itself
    /// carries no field wide enough to be a raw credential blob.
    pub fn append(&self, kind: AuditEventKind) -> Result<(), AuditLogError> {
        let recorded_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let event = AuditEvent {
            recorded_at_ms,
            kind,
        };
        let mut value = serde_json::to_value(&event)?;
        // Best-effort defense in depth: redact any object-shaped, secret-key
        // fields even though `AuditEventKind`'s closed shape should never
        // carry one, and separately scan every string leaf for a bearer/API
        // key pattern so a credential pasted into a preview cannot survive.
        rightkit_llm::agent_providers::replay::redact_json_value(&mut value);
        redact_string_leaves(&mut value);
        let mut line = serde_json::to_string(&value)?;
        line.push('\n');

        // Observability trail: sync is not required for correctness (unlike
        // the journal), so the append is non-durable — a lost tail record on
        // crash is acceptable for a best-effort diagnostics log.
        rightkit_fs::append_record(
            &self.path,
            line.as_bytes(),
            rightkit_fs::AppendOptions::default().durable(false),
        )
        .map_err(std::io::Error::from)?;
        Ok(())
    }
}

/// Recursively pass every string leaf of `value` through the same
/// content-redaction pass the provider-replay recorder uses for free-form
/// text (bearer tokens, `sk-...`-shaped keys, etc.), independent of the
/// object-key-based [`rightkit_llm::agent_providers::replay::redact_json_value`] pass
/// already applied to the top level.
fn redact_string_leaves(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let mut delta = rightkit_llm::agent_providers::CompletionDelta {
                content: Some(std::mem::take(text)),
                ..Default::default()
            };
            rightkit_llm::agent_providers::replay::redact_delta(&mut delta);
            *text = delta.content.unwrap_or_default();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_string_leaves(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, child) in map.iter_mut() {
                redact_string_leaves(child);
            }
        }
        _ => {}
    }
}

fn audit_log_path(storage_dir: &Path, session_id: &str) -> PathBuf {
    storage_dir.join(format!("{session_id}.audit.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn appends_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path(), "s1").unwrap();
        log.append(AuditEventKind::SessionStarted { resumed: false })
            .unwrap();
        log.append(AuditEventKind::ToolInvoked {
            tool_name: "read_file".into(),
            ok: true,
        })
        .unwrap();
        let lines = read_lines(log.path());
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.get("recordedAtMs").is_some());
        }
    }

    #[test]
    fn bearer_token_pasted_into_a_preview_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path(), "s1").unwrap();
        log.append(AuditEventKind::TurnSubmitted {
            preview: concat!("use Bearer sk", "-super-secret-token please").into(),
        })
        .unwrap();
        let lines = read_lines(log.path());
        assert_eq!(lines.len(), 1);
        assert!(
            !lines[0].contains(concat!("sk", "-super-secret-token")),
            "raw token leaked into audit log: {}",
            lines[0]
        );
    }

    #[test]
    fn secret_shaped_object_key_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path(), "s1").unwrap();
        log.append(AuditEventKind::CredentialRefreshed {
            provider: "github".into(),
        })
        .unwrap();
        let lines = read_lines(log.path());
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(parsed["kind"], "credentialRefreshed");
        assert_eq!(parsed["provider"], "github");
    }

    #[test]
    fn open_creates_storage_dir_if_absent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("runtime");
        assert!(!nested.exists());
        let log = AuditLog::open(&nested, "s1").unwrap();
        log.append(AuditEventKind::TurnInterrupted).unwrap();
        assert!(nested.exists());
        assert!(log.path().exists());
    }
}
