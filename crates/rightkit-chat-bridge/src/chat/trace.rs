//! Per-message observations shared by the CLI, engine and harness hooks on this computer.
//! Each writer commits before entering a potentially blocking operation. A diagnostic reader
//! can therefore see a stage even while its originating process is waiting or after it died.

use super::{Store, envelope::now_ms, store};
use rightkit_db::rusqlite::{Connection, OptionalExtension, params};
use rightkit_db::{DbOptions, MigrateOptions, Migration};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SCHEMA: &str = "
CREATE TABLE messages (
    id TEXT NOT NULL UNIQUE, outbound INTEGER, peer TEXT, incoming_owner TEXT
);
CREATE TABLE events (
    id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE, stage TEXT NOT NULL,
    phase TEXT NOT NULL, outcome TEXT NOT NULL, at_ms INTEGER NOT NULL,
    elapsed_ms INTEGER, span TEXT NOT NULL
);
CREATE INDEX events_by_id ON events(id);
CREATE INDEX latest_outbound ON messages(outbound DESC);
CREATE TRIGGER expire_messages AFTER INSERT ON messages BEGIN
    DELETE FROM messages WHERE rowid <= NEW.rowid - 2000;
END;
CREATE TRIGGER expire_events AFTER INSERT ON events BEGIN
    DELETE FROM events WHERE rowid = (
        SELECT rowid FROM events WHERE id = NEW.id ORDER BY rowid DESC LIMIT 1 OFFSET 256
    );
END;";

const MIGRATIONS: &[Migration] = &[Migration::sql(1, "chat_trace", SCHEMA)];

fn sql_ms(ms: u64) -> rightkit_db::rusqlite::Result<i64> {
    i64::try_from(ms).map_err(|e| rightkit_db::rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

fn read_ms(column: usize, ms: i64) -> rightkit_db::rusqlite::Result<u64> {
    u64::try_from(ms).map_err(|_| rightkit_db::rusqlite::Error::IntegralValueOutOfRange(column, ms))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Event {
    pub stage: String,
    pub phase: String,
    pub outcome: String,
    pub at_ms: u64,
    pub elapsed_ms: Option<u64>,
    pub span: String,
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn token(s: &str) -> bool {
    s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Never print terminal controls, peer prose, paths or arbitrarily deep event lists.
/// Treat an incompatible peer as unavailable to the trace, independently of delivery.
pub fn validate_remote(events: &[Event]) -> Result<(), String> {
    let valid = events.len() <= 256
        && events.iter().all(|e| {
            !e.stage.is_empty()
                && token(&e.stage)
                && token(&e.outcome)
                && token(&e.span)
                && matches!(
                    e.phase.as_str(),
                    "start" | "finish" | "waiting" | "resumed" | "mark"
                )
        });
    if valid {
        Ok(())
    } else {
        Err("invalid trace response".into())
    }
}

thread_local! {
    static WRITER: RefCell<Option<(PathBuf, Connection)>> = const { RefCell::new(None) };
}

fn options(store: &Store) -> Result<DbOptions, String> {
    let path = store.root().join("trace.sqlite3");
    for file in [
        "trace.sqlite3",
        "trace.sqlite3-wal",
        "trace.sqlite3-shm",
        "trace.sqlite3-journal",
    ] {
        store::refuse_link(&store.root().join(file)).map_err(|e| e.to_string())?;
    }
    let mut options = DbOptions::new(path);
    options.busy_timeout = Duration::from_millis(100);
    options.foreign_keys = true;
    Ok(options)
}

fn write(
    store: &Store,
    id: &str,
    action: impl FnOnce(&Connection) -> rightkit_db::rusqlite::Result<()>,
) {
    if !valid_id(id) {
        return;
    }
    let result = WRITER.with(|slot| -> Result<(), String> {
        let mut slot = slot.borrow_mut();
        if !slot.as_ref().is_some_and(|(root, _)| root == store.root()) {
            store::secure_chain(store.root()).map_err(|e| e.to_string())?;
            let (db, _) = options(store)?
                .open_writer_migrated(MIGRATIONS, &MigrateOptions::default())
                .map_err(|e| e.to_string())?;
            *slot = Some((store.root().to_owned(), db));
        }
        let (_, db) = slot.as_mut().expect("writer initialized");
        let tx = db
            .transaction_with_behavior(rightkit_db::rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO messages(id) VALUES (?) ON CONFLICT(id) DO NOTHING",
            [id],
        )
        .map_err(|e| e.to_string())?;
        action(&tx).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    });
    if result.is_err() {
        // Diagnostic storage must never reject or retry a user's message.
        super::sendlog::event(
            rightkit_logs::Level::Warn,
            "chat.trace.write_failed",
            serde_json::json!({}),
        );
    }
}

pub fn begin(store: &Store, id: &str) {
    write(store, id, |db| {
        db.execute(
            "UPDATE messages SET outbound = ?1 WHERE id = ?2 AND outbound IS NULL",
            params![sql_ms(now_ms())?, id],
        )?;
        Ok(())
    });
}

pub fn remote(store: &Store, id: &str, link: &str) {
    write(store, id, |db| {
        db.execute(
            "UPDATE messages SET peer = ?1 WHERE id = ?2",
            params![link, id],
        )?;
        Ok(())
    });
}

/// Bind a fresh inbound trace to its authenticated mesh caller, before recipient discovery.
/// A second peer reusing an existing message id cannot claim that message's observations.
pub fn allow_peer(store: &Store, id: &str, owner: &str) {
    write(store, id, |db| {
        db.execute(
            "UPDATE messages SET incoming_owner=?2 WHERE id=?1
            AND incoming_owner IS NULL AND outbound IS NULL
            AND NOT EXISTS(SELECT 1 FROM events WHERE id=?1)",
            params![id, owner],
        )?;
        Ok(())
    });
}

pub fn allows_peer(store: &Store, id: &str, owner: &str) -> bool {
    let Ok(Some(db)) = reader(store) else {
        return false;
    };
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND incoming_owner=?2)",
        params![id, owner],
        |row| row.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn reader(store: &Store) -> Result<Option<Connection>, String> {
    let options = options(store)?;
    if !options.path.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    // Read only, including when an older installation has no trace database.
    Connection::open_with_flags(
        options.path,
        rightkit_db::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map(Some)
    .map_err(|e| e.to_string())
}

pub fn latest(store: &Store) -> Result<Option<String>, String> {
    let Some(db) = reader(store)? else {
        return Ok(None);
    };
    db.query_row("SELECT id FROM messages WHERE outbound IS NOT NULL ORDER BY outbound DESC, rowid DESC LIMIT 1", [], |row| row.get(0))
        .optional().map_err(|e| e.to_string())
}

pub fn peer(store: &Store, id: &str) -> Result<Option<String>, String> {
    let Some(db) = reader(store)? else {
        return Ok(None);
    };
    db.query_row(
        "SELECT peer FROM messages WHERE id = ? AND peer IS NOT NULL",
        [id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn record(
    store: &Store,
    id: &str,
    stage: &str,
    phase: &str,
    outcome: &str,
    span: &str,
    elapsed: Option<u64>,
) {
    let outcome = if token(outcome) {
        outcome
    } else {
        "unrecognized_outcome"
    };
    let at = now_ms();
    write(store, id, |db| {
        db.execute("INSERT INTO events(id,stage,phase,outcome,at_ms,elapsed_ms,span) VALUES(?,?,?,?,?,?,?)",
            params![id, stage, phase, outcome, sql_ms(at)?, elapsed.map(sql_ms).transpose()?, span])?;
        Ok(())
    });
}

pub struct Span {
    store: Store,
    id: String,
    stage: &'static str,
    token: String,
    started: Instant,
    finished: bool,
}

pub fn start(store: &Store, id: &str, stage: &'static str) -> Span {
    let token = rightkit_logs::identity::random_uuid();
    let started = Instant::now();
    record(store, id, stage, "start", "", &token, None);
    Span {
        store: store.clone(),
        id: id.into(),
        stage,
        token,
        started,
        finished: false,
    }
}

impl Span {
    pub fn finish(mut self, outcome: &str) {
        self.close(outcome);
    }

    fn close(&mut self, outcome: &str) {
        self.finished = true;
        let elapsed = self
            .started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        record(
            &self.store,
            &self.id,
            self.stage,
            "finish",
            outcome,
            &self.token,
            Some(elapsed),
        );
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            self.close("abandoned");
        }
    }
}

pub fn mark(store: &Store, id: &str, stage: &'static str, outcome: &str) {
    record(store, id, stage, "mark", outcome, "", None);
}

// A second process can observe pickup before the enqueue call returns. Keep both observations,
// once per queue and message, and never manufacture a negative or zero-length wait on a race.
fn queue_event(store: &Store, id: &str, stage: &str, phase: &str, outcome: &str) {
    let at = now_ms();
    write(store, id, |db| {
        db.execute(
            "INSERT INTO events(id,stage,phase,outcome,at_ms,elapsed_ms,span)
            SELECT ?1,?2,?3,?4,?5,NULL,?2 WHERE NOT EXISTS(
                SELECT 1 FROM events WHERE id=?1 AND stage=?2 AND phase=?3
            )",
            params![id, stage, phase, outcome, sql_ms(at)?],
        )?;
        Ok(())
    });
}

pub fn waiting(store: &Store, id: &str, stage: &'static str) {
    queue_event(store, id, stage, "waiting", "");
}

pub fn resumed(store: &Store, id: &str, stage: &'static str, outcome: &str) {
    queue_event(store, id, stage, "resumed", outcome);
}

pub fn read_local(store: &Store, id: &str) -> Result<Vec<Event>, String> {
    if !valid_id(id) {
        return Err("expected a complete message UUID".into());
    }
    let Some(db) = reader(store)? else {
        return Ok(Vec::new());
    };
    let mut stmt = db.prepare("SELECT stage,phase,outcome,at_ms,elapsed_ms,span FROM events WHERE id=? ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let mut events: Vec<Event> = stmt
        .query_map([id], |r| {
            Ok(Event {
                stage: r.get(0)?,
                phase: r.get(1)?,
                outcome: r.get(2)?,
                at_ms: read_ms(3, r.get(3)?)?,
                elapsed_ms: r
                    .get::<_, Option<i64>>(4)?
                    .map(|ms| read_ms(4, ms))
                    .transpose()?,
                span: r.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    // All arithmetic uses timestamps taken on THIS host. Never compare the sender's clock with
    // the receiver's. Clock rollback (or an acknowledgement/pickup race) makes duration unknown.
    let observed_at = now_ms();
    for index in 0..events.len() {
        let event = &events[index];
        let duration = match event.phase.as_str() {
            "resumed" => events
                .iter()
                .find(|e| e.span == event.span && e.phase == "waiting")
                .and_then(|start| event.at_ms.checked_sub(start.at_ms)),
            "waiting" | "start" => {
                let close = if event.phase == "waiting" {
                    "resumed"
                } else {
                    "finish"
                };
                if events
                    .iter()
                    .any(|e| e.span == event.span && e.phase == close)
                {
                    None
                } else {
                    observed_at.checked_sub(event.at_ms)
                }
            }
            _ => continue,
        };
        events[index].elapsed_ms = duration;
    }
    Ok(events)
}
