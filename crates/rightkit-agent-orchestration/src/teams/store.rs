//! Shared SQLite-backed durability for [`super::board`], [`super::mailbox`],
//! and [`super::missionlog`]. One [`TeamCoordinationStore`] owns one database
//! file per team; peers on the same host share a team by opening the same
//! path.

use rightkit_db::rusqlite;
use std::path::{Path, PathBuf};

use rightkit_db::rusqlite::Connection;

use super::manifest_import::{import_manifest, ManifestImportError, TeamManifest};

#[derive(Debug, thiserror::Error)]
pub enum TeamCoordinationError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Db(#[from] rightkit_db::DbError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("team coordination: {0}")]
    Invalid(String),
    #[error(transparent)]
    Manifest(#[from] ManifestImportError),
}

/// Durable peer-team coordination store (task board + mailbox + mission log)
/// for one team, backed by a single SQLite database file.
#[derive(Debug, Clone)]
pub struct TeamCoordinationStore {
    path: PathBuf,
}

impl TeamCoordinationStore {
    /// Open (creating if absent) the coordination database at `path` and
    /// apply schema migrations. Safe to call from multiple peer processes
    /// pointed at the same path — SQLite serializes writers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TeamCoordinationError> {
        let store = Self {
            path: path.as_ref().to_path_buf(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persist one already-validated named fleet. Only a caller that holds a
    /// `TeamManifest` returned by `import_manifest` can reach this method;
    /// raw manifest JSON never enters durable coordination state.
    pub fn save_manifest(&self, manifest: &TeamManifest) -> Result<(), TeamCoordinationError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO team_manifests (team_id, manifest_json, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(team_id) DO UPDATE SET manifest_json = excluded.manifest_json, updated_at = excluded.updated_at",
            rusqlite::params![
                manifest.team_id(),
                serde_json::to_string(manifest)?,
                rightkit_agent_engine_protocol::time::now_iso(),
            ],
        )?;
        Ok(())
    }

    /// Load a persisted manifest through the same strict importer used for
    /// untrusted input. A malformed or stale database row fails closed.
    pub fn load_manifest(
        &self,
        team_id: &str,
    ) -> Result<Option<TeamManifest>, TeamCoordinationError> {
        use rightkit_db::rusqlite::OptionalExtension;

        let conn = self.connect()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT manifest_json FROM team_manifests WHERE team_id = ?1",
                rusqlite::params![team_id],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|manifest| import_manifest(&manifest))
            .transpose()
            .map_err(TeamCoordinationError::from)
    }

    pub(crate) fn connect(&self) -> Result<Connection, TeamCoordinationError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        Ok(rightkit_db::DbOptions::new(&self.path).open_writer()?)
    }

    fn migrate(&self) -> Result<(), TeamCoordinationError> {
        let conn = self.connect()?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS task_board (
                item_id TEXT PRIMARY KEY,
                team_id TEXT NOT NULL,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                claimed_by TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_task_board_team ON task_board(team_id);

            CREATE TABLE IF NOT EXISTS mailbox (
                message_id TEXT PRIMARY KEY,
                team_id TEXT NOT NULL,
                from_peer TEXT NOT NULL,
                to_peer TEXT,
                created_at TEXT NOT NULL,
                consumed_at TEXT,
                body_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mailbox_team_to ON mailbox(team_id, to_peer);

            CREATE TABLE IF NOT EXISTS mailbox_consumptions (
                message_id TEXT NOT NULL,
                peer_id TEXT NOT NULL,
                consumed_at TEXT NOT NULL,
                PRIMARY KEY (message_id, peer_id)
            );
            CREATE INDEX IF NOT EXISTS idx_mailbox_consumptions_message
                ON mailbox_consumptions(message_id);

            CREATE TABLE IF NOT EXISTS mission_log (
                entry_id TEXT PRIMARY KEY,
                team_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                peer_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                kind TEXT NOT NULL,
                detail_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mission_log_team_seq ON mission_log(team_id, seq);

            CREATE TABLE IF NOT EXISTS team_manifests (
                team_id TEXT PRIMARY KEY,
                manifest_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }
}
