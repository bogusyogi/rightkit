//! The discovery record a loopback host publishes, in CodeRight's current shape.
//!
//! CodeRight's daemon writes one JSON line to `<state root>/daemon.port` (owner-only, atomic) and
//! prints the same line on stdout at startup; the window, the CLI and the SSH-tunnel clients read
//! it. The field names are PascalCase and are kept exactly (`engine/bins/coderight/src/startup.rs`
//! `StartupInfo`, read by `daemon-client/src/daemon_lifecycle.rs` `DaemonPortFile`):
//!
//! ```text
//! {"Port":5057,"Token":"...","DatabasePath":"...","Pid":42,"BootEpochMs":1725000000000,
//!  "ProtocolVersion":1,"SchemaId":"coderight.daemon.v1","Runtime":"rust","Generation":1}
//! ```
//!
//! `DatabasePath` is required by the existing reader, so it is always written (the owner gives
//! the value; it means nothing to this crate). `SchemaId`, `Runtime` and `Generation` are the
//! owner's labels. The pid and the process birth time (`BootEpochMs`) are what let a reader tell
//! a live host from a recycled pid ([`LoopbackRecord::process_matches`], the same two-second
//! tolerance CodeRight's reader uses).

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use rightkit_child_process::AdoptedIdentity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopbackRecord {
    #[serde(rename = "Port")]
    pub port: u16,
    #[serde(rename = "Token")]
    pub token: String,
    #[serde(rename = "DatabasePath")]
    pub database_path: String,
    #[serde(rename = "Pid", default)]
    pub pid: u32,
    #[serde(rename = "BootEpochMs", default)]
    pub boot_epoch_ms: u128,
    #[serde(rename = "ProtocolVersion", default)]
    pub protocol_version: u16,
    #[serde(rename = "SchemaId", default)]
    pub schema_id: String,
    #[serde(rename = "Runtime", default)]
    pub runtime: String,
    #[serde(rename = "Generation", default)]
    pub generation: u64,
}

/// The owner's labels for the record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordLabels {
    pub database_path: String,
    pub protocol_version: u16,
    pub schema_id: String,
    pub runtime: String,
    pub generation: u64,
}

impl LoopbackRecord {
    /// The record of this process, listening on `port` with `token`.
    pub fn for_this_process(port: u16, token: &str, labels: &RecordLabels) -> Self {
        let boot = crate::discovery::own_start_ms()
            .map(u128::from)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            });
        Self {
            port,
            token: token.to_string(),
            database_path: labels.database_path.clone(),
            pid: std::process::id(),
            boot_epoch_ms: boot,
            protocol_version: labels.protocol_version,
            schema_id: labels.schema_id.clone(),
            runtime: labels.runtime.clone(),
            generation: labels.generation,
        }
    }

    /// The single-line form (what is written to the file and printed at startup).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse the file contents. Blank, malformed and wrong-typed text is `None`; a legacy record
    /// without the liveness fields parses with them zeroed.
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str(trimmed).ok()
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Whether `pid` is a live process that started when the record says (within two seconds).
    /// A recycled pid names a different start time and fails; so does a record without a pid or
    /// a birth time.
    pub fn process_matches(&self) -> bool {
        self.boot_epoch_ms != 0
            && NonZeroU32::new(self.pid)
                .zip(u64::try_from(self.boot_epoch_ms).ok())
                .is_some_and(|(pid, recorded_ms)| {
                    AdoptedIdentity::from_recorded_unix_ms(
                        pid,
                        recorded_ms,
                        Duration::from_millis(2_000),
                    )
                    .ok()
                    .flatten()
                    .is_some()
                })
    }

    /// Write the record, atomically and owner-only (`rightkit_fs::atomic_write_private`). The
    /// directory must already exist; the owner hardens it, as CodeRight does for its state root.
    pub fn publish(&self, path: &Path) -> std::io::Result<()> {
        crate::record::write_atomic(path, self.to_line().as_bytes())
    }

    /// Remove the file only if it still holds this record (same pid, port and token), so a
    /// successor's record is never deleted by a host that is late to exit.
    pub fn remove_if_ours(&self, path: &Path) {
        let ours = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| Self::parse(&text))
            .is_some_and(|found| {
                found.pid == self.pid && found.port == self.port && found.token == self.token
            });
        if ours {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> RecordLabels {
        RecordLabels {
            database_path: "C:/data/app.db".into(),
            protocol_version: 1,
            schema_id: "coderight.daemon.v1".into(),
            runtime: "rust".into(),
            generation: 1,
        }
    }

    #[test]
    fn the_line_is_the_coderight_pascal_case_shape() {
        let record = LoopbackRecord {
            port: 5057,
            token: "tok-123".into(),
            database_path: "db".into(),
            pid: 42,
            boot_epoch_ms: 1_725_000_000_000,
            protocol_version: 1,
            schema_id: "coderight.daemon.v1".into(),
            runtime: "rust".into(),
            generation: 1,
        };
        let line = record.to_line();
        assert_eq!(
            line,
            r#"{"Port":5057,"Token":"tok-123","DatabasePath":"db","Pid":42,"BootEpochMs":1725000000000,"ProtocolVersion":1,"SchemaId":"coderight.daemon.v1","Runtime":"rust","Generation":1}"#
        );
        assert_eq!(LoopbackRecord::parse(&line), Some(record.clone()));
        assert_eq!(LoopbackRecord::parse(&format!("{line}\n")), Some(record));
    }

    #[test]
    fn the_existing_readers_inputs_still_parse() {
        // The shape CodeRight's own reader accepts: only Port, Token and DatabasePath required.
        let legacy =
            LoopbackRecord::parse(r#"{"Port":5057,"Token":"tok","DatabasePath":"db"}"#).unwrap();
        assert_eq!(
            (legacy.pid, legacy.boot_epoch_ms, legacy.protocol_version),
            (0, 0, 0)
        );
        assert!(!legacy.process_matches());
        assert_eq!(LoopbackRecord::parse(""), None);
        assert_eq!(LoopbackRecord::parse("   \n"), None);
        assert_eq!(LoopbackRecord::parse("{not json"), None);
        assert_eq!(
            LoopbackRecord::parse(r#"{"Port":"x","Token":"t","DatabasePath":"d"}"#),
            None
        );
        assert_eq!(
            LoopbackRecord::parse(r#"{"Token":"t","DatabasePath":"d"}"#),
            None
        );
    }

    #[test]
    fn a_recycled_pid_is_not_this_host() {
        let mut record = LoopbackRecord::for_this_process(1, "token-token-token", &labels());
        assert_eq!(record.pid, std::process::id());
        assert!(
            record.process_matches(),
            "this process started when it says"
        );
        record.boot_epoch_ms += 60_000;
        assert!(!record.process_matches(), "same pid, another birth time");
        record.boot_epoch_ms = 0;
        assert!(!record.process_matches());
        record.boot_epoch_ms = 5;
        record.pid = 0;
        assert!(!record.process_matches());
    }

    #[test]
    fn the_file_is_written_and_removed_only_by_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.port");
        let mine = LoopbackRecord::for_this_process(4000, "token-token-token", &labels());
        mine.publish(&path).unwrap();
        assert_eq!(
            LoopbackRecord::parse(&std::fs::read_to_string(&path).unwrap()),
            Some(mine.clone())
        );
        // A successor replaced it: a late exit must not delete the successor's record.
        let mut successor = mine.clone();
        successor.port = 4001;
        successor.publish(&path).unwrap();
        mine.remove_if_ours(&path);
        assert!(path.exists());
        successor.remove_if_ours(&path);
        assert!(!path.exists());
    }
}
