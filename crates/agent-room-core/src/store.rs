use crate::protocol::{events_jsonl_bytes, ProtocolError, Room};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct StoreError {
    pub code: &'static str,
    pub detail: String,
}

impl StoreError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::new("sqlite_error", value.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::new("io_error", value.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::new("serialization_error", value.to_string())
    }
}

impl From<ProtocolError> for StoreError {
    fn from(value: ProtocolError) -> Self {
        Self::new(value.code, value.detail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReceipt {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReceipt {
    pub room_id: String,
    pub transcript_sha256: String,
    pub removed: Vec<String>,
}

pub struct RoomStore {
    run_dir: PathBuf,
    connection: Connection,
}

impl RoomStore {
    pub fn create(run_dir: &Path, room: &Room) -> Result<Self, StoreError> {
        std::fs::create_dir_all(run_dir)?;
        let connection = Connection::open(run_dir.join("room.sqlite3"))?;
        let mut store = Self {
            run_dir: run_dir.to_path_buf(),
            connection,
        };
        store.initialize()?;
        store.save(room)?;
        Ok(store)
    }

    pub fn open(run_dir: &Path) -> Result<Self, StoreError> {
        let db = run_dir.join("room.sqlite3");
        if !db.is_file() {
            return Err(StoreError::new(
                "room_store_missing",
                db.display().to_string(),
            ));
        }
        let connection = Connection::open(db)?;
        let mut store = Self {
            run_dir: run_dir.to_path_buf(),
            connection,
        };
        store.initialize()?;
        Ok(store)
    }

    fn initialize(&mut self) -> Result<(), StoreError> {
        self.connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS room (id INTEGER PRIMARY KEY CHECK(id=1), state_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS seats (seat_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS motions (motion_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (seq INTEGER PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS findings (finding_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS contests (contest_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS dispositions (disposition_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS receipts (receipt_id TEXT PRIMARY KEY, record_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS requests (request_key TEXT PRIMARY KEY, record_json TEXT NOT NULL);",
        )?;
        Ok(())
    }

    pub fn save(&mut self, room: &Room) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO room(id,state_json) VALUES(1,?1)
             ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json",
            params![serde_json::to_string(room)?],
        )?;
        for seat in room.seats.values() {
            transaction.execute(
                "INSERT OR REPLACE INTO seats(seat_id,record_json) VALUES(?1,?2)",
                params![seat.seat_id, serde_json::to_string(seat)?],
            )?;
        }
        for event in &room.events {
            let sequence = i64::try_from(event.seq).map_err(|_| {
                StoreError::new("sequence_overflow", "event sequence exceeds SQLite INTEGER")
            })?;
            transaction.execute(
                "INSERT OR IGNORE INTO messages(seq,record_json) VALUES(?1,?2)",
                params![sequence, serde_json::to_string(event)?],
            )?;
        }
        for finding in room.findings.values() {
            transaction.execute(
                "INSERT OR REPLACE INTO findings(finding_id,record_json) VALUES(?1,?2)",
                params![finding.finding_id, serde_json::to_string(finding)?],
            )?;
        }
        for contest in room.contests.values() {
            transaction.execute(
                "INSERT OR REPLACE INTO contests(contest_id,record_json) VALUES(?1,?2)",
                params![contest.contest_id, serde_json::to_string(contest)?],
            )?;
        }
        for disposition in room.dispositions.values() {
            transaction.execute(
                "INSERT OR REPLACE INTO dispositions(disposition_id,record_json) VALUES(?1,?2)",
                params![
                    disposition.disposition_id,
                    serde_json::to_string(disposition)?
                ],
            )?;
            for (index, receipt) in disposition.receipt_refs.iter().enumerate() {
                let receipt_id = format!("{}:{index}", disposition.disposition_id);
                transaction.execute(
                    "INSERT OR REPLACE INTO receipts(receipt_id,record_json) VALUES(?1,?2)",
                    params![receipt_id, serde_json::to_string(receipt)?],
                )?;
            }
        }
        for motion in room.motions.values() {
            transaction.execute(
                "INSERT OR REPLACE INTO motions(motion_id,record_json) VALUES(?1,?2)",
                params![motion.motion_id, serde_json::to_string(motion)?],
            )?;
        }
        for (request_key, record_json) in room.request_rows()? {
            transaction.execute(
                "INSERT OR REPLACE INTO requests(request_key,record_json) VALUES(?1,?2)",
                params![request_key, record_json],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load(&self) -> Result<Room, StoreError> {
        let raw: String =
            self.connection
                .query_row("SELECT state_json FROM room WHERE id=1", [], |row| {
                    row.get(0)
                })?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn table_names(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn row_count(&self, table: &str) -> Result<u64, StoreError> {
        if !matches!(
            table,
            "room"
                | "seats"
                | "motions"
                | "messages"
                | "findings"
                | "contests"
                | "dispositions"
                | "receipts"
                | "requests"
        ) {
            return Err(StoreError::new("unknown_table", table));
        }
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = self.connection.query_row(&sql, [], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StoreError::new("count_invalid", count.to_string()))
    }

    pub fn export_jsonl(&mut self, room: &Room) -> Result<ExportReceipt, StoreError> {
        if !room.sealed {
            return Err(StoreError::new("room_not_sealed", room.room_id.clone()));
        }
        self.save(room)?;
        let bytes = events_jsonl_bytes(&room.events)?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let path = self.run_dir.join("transcript.jsonl");
        let temporary = self.run_dir.join("transcript.jsonl.tmp");
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &path)?;
        let receipt = ExportReceipt {
            path,
            sha256,
            bytes: bytes.len() as u64,
        };
        std::fs::write(
            self.run_dir.join("transcript.receipt.json"),
            serde_json::to_vec_pretty(&receipt)?,
        )?;
        Ok(receipt)
    }

    pub fn export_finding_ledger(&mut self, room: &Room) -> Result<ExportReceipt, StoreError> {
        if !room.sealed {
            return Err(StoreError::new("room_not_sealed", room.room_id.clone()));
        }
        self.save(room)?;
        let mut receipts = std::collections::BTreeMap::<String, serde_json::Value>::new();
        let dispositions = room
            .dispositions
            .values()
            .map(|disposition| {
                let receipt_ids = disposition
                    .receipt_refs
                    .iter()
                    .map(|receipt| {
                        let kind = match receipt.kind {
                            crate::protocol::ReceiptKind::FileLine => "file",
                            crate::protocol::ReceiptKind::Artifact => "artifact",
                            crate::protocol::ReceiptKind::Measurement => "measurement",
                            crate::protocol::ReceiptKind::Web => "web",
                            crate::protocol::ReceiptKind::ReviewScope => "scope_motion",
                        };
                        let locator = receipt.line.map_or_else(
                            || receipt.locator.clone(),
                            |line| format!("{}:{line}", receipt.locator),
                        );
                        let canonical = serde_json::json!({
                            "kind": kind,
                            "locator": locator,
                            "digest": receipt.digest,
                        });
                        let receipt_id = format!(
                            "receipt-{:x}",
                            Sha256::digest(
                                serde_json::to_vec(&canonical).expect("receipt serializes")
                            )
                        );
                        receipts.entry(receipt_id.clone()).or_insert_with(|| {
                            serde_json::json!({
                                "receipt_id": receipt_id,
                                "kind": kind,
                                "locator": locator,
                                "digest": receipt.digest,
                            })
                        });
                        receipt_id
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "disposition_id": disposition.disposition_id,
                    "finding_id": disposition.finding_id,
                    "action": disposition.action,
                    "status": disposition.status,
                    "reason_code": disposition.reason_code,
                    "final_receipt_refs": receipt_ids,
                })
            })
            .collect::<Vec<_>>();
        let ledger = serde_json::json!({
            "schema_version": 1,
            "room_id": room.room_id,
            "findings": room.findings.values().collect::<Vec<_>>(),
            "dispositions": dispositions,
            "receipts": receipts.into_values().collect::<Vec<_>>(),
        });
        let bytes = serde_json::to_vec_pretty(&ledger)?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let path = self.run_dir.join("finding-ledger.json");
        let temporary = self.run_dir.join("finding-ledger.json.tmp");
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &path)?;
        let receipt = ExportReceipt {
            path,
            sha256,
            bytes: bytes.len() as u64,
        };
        std::fs::write(
            self.run_dir.join("finding-ledger.receipt.json"),
            serde_json::to_vec_pretty(&receipt)?,
        )?;
        Ok(receipt)
    }

    pub fn verify_export(path: &Path, expected: &str) -> Result<bool, StoreError> {
        let bytes = std::fs::read(path)?;
        Ok(format!("{:x}", Sha256::digest(bytes)) == expected)
    }

    pub fn prune_exported_run(run_dir: &Path) -> Result<PruneReceipt, StoreError> {
        let room = {
            let store = Self::open(run_dir)?;
            store.load()?
        };
        if !room.sealed {
            return Err(StoreError::new("room_not_sealed", room.room_id));
        }
        let transcript_path = run_dir.join("transcript.jsonl");
        let receipt_path = run_dir.join("transcript.receipt.json");
        let export: ExportReceipt = serde_json::from_slice(&std::fs::read(&receipt_path)?)?;
        if export.bytes != std::fs::metadata(&transcript_path)?.len()
            || !Self::verify_export(&transcript_path, &export.sha256)?
        {
            return Err(StoreError::new(
                "export_digest_invalid",
                transcript_path.display().to_string(),
            ));
        }

        let fixed = [
            "room.sqlite3",
            "room.sqlite3-wal",
            "room.sqlite3-shm",
            "runtime.json",
            ".serve-config.json",
            "service.stdout.log",
            "service.stderr.log",
            "claude-mcp.json",
            "minimax-mcp.json",
        ];
        let mut removed = Vec::new();
        for entry in std::fs::read_dir(run_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let is_credential = name.starts_with("seat-") && name.ends_with(".credentials.json");
            if fixed.contains(&name.as_str()) || is_credential {
                std::fs::remove_file(entry.path())?;
                removed.push(name);
            }
        }
        removed.sort();
        let receipt = PruneReceipt {
            room_id: room.room_id,
            transcript_sha256: export.sha256,
            removed,
        };
        let temporary = run_dir.join("prune.receipt.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&receipt)?)?;
        std::fs::rename(temporary, run_dir.join("prune.receipt.json"))?;
        Ok(receipt)
    }
}
