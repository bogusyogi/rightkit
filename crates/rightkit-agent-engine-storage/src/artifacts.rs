use crate::error::Result;
use crate::Storage;
use rightkit_agent_engine_protocol::ArtifactRow;
use rightkit_db::rusqlite::OptionalExtension;
use std::path::{Component, Path};
use std::sync::OnceLock;

/// `producer` written on browser evidence artifacts unless the host sets its own.
pub const DEFAULT_BROWSER_ARTIFACT_PRODUCER: &str = "rightkit-browser";

static BROWSER_ARTIFACT_PRODUCER: OnceLock<String> = OnceLock::new();

/// Sets the `producer` value of browser artifacts the store records (for example `myapp-browser`).
/// Call once at start-up; returns `false` when one was already set. Rows already stored keep
/// the value they were written with.
pub fn set_browser_artifact_producer(producer: &str) -> bool {
    BROWSER_ARTIFACT_PRODUCER.set(producer.to_string()).is_ok()
}

/// The browser artifact producer in force.
pub fn browser_artifact_producer() -> &'static str {
    BROWSER_ARTIFACT_PRODUCER
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_BROWSER_ARTIFACT_PRODUCER)
}

impl Storage {
    /// Record browser evidence in the existing artifact store. The primitive
    /// argument surface is intentional: `artifacts` is a private module, so a
    /// public input type declared here could not be named by storage callers.
    #[allow(clippy::too_many_arguments)]
    pub fn record_browser_artifact(
        &self,
        session_id: &str,
        browser_session_id: &str,
        observation_epoch: u64,
        kind: &str,
        relative_path: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<ArtifactRow> {
        if !matches!(kind, "screenshot" | "download" | "dom" | "ax" | "audit") {
            return Err(crate::error::StorageError::Serde(format!(
                "unsupported browser artifact kind: {}",
                kind
            )));
        }
        let path = Path::new(relative_path);
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || relative_path.contains('\\')
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir
                        | Component::CurDir
                        | Component::RootDir
                        | Component::Prefix(_)
                )
            })
        {
            return Err(crate::error::StorageError::Serde(
                "browser artifact path must be a contained relative path".to_string(),
            ));
        }
        let digest = rightkit_fs::digest::sha256_bytes(bytes);
        let mut identity = rightkit_fs::digest::Sha256Hasher::new();
        identity.update(kind.as_bytes());
        identity.update(&[0]);
        identity.update(relative_path.as_bytes());
        identity.update(&[0]);
        identity.update(bytes);
        let identity = identity.finalize();
        let id = format!(
            "browser:{}:{}:{}:{}",
            session_id, browser_session_id, observation_epoch, identity
        );
        let row = ArtifactRow {
            id,
            path: relative_path.to_string(),
            kind: format!("browser-{kind}"),
            content_hash: format!("sha256:{digest}"),
            shippable: false,
            producer: browser_artifact_producer().to_string(),
            provenance: format!(
                "browser-session:{};observation-epoch:{}",
                browser_session_id, observation_epoch
            ),
            trust_label: "untrusted-web".to_string(),
            verdict: None,
            approvals: Vec::new(),
            meta: serde_json::json!({
                "schemaVersion": 1,
                "browserSessionId": browser_session_id,
                "observationEpoch": observation_epoch,
                "mime": mime,
                "bytes": bytes.len(),
            })
            .to_string(),
            created_at: rightkit_agent_engine_protocol::now_iso(),
        };
        self.upsert_artifact(session_id, &row)?;
        Ok(row)
    }

    /// Insert or replace an artifact. `ArtifactRow` carries no session id, so
    /// it is supplied explicitly and stored in the indexed `session_id` column
    /// used by [`Storage::get_artifact_by_path`] and [`Storage::list_artifacts`].
    pub fn upsert_artifact(&self, session_id: &str, row: &ArtifactRow) -> Result<()> {
        let row_json = serde_json::to_string(row)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO artifacts (id, session_id, path, trust_label, row_json) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (&row.id, session_id, &row.path, &row.trust_label, &row_json),
        )?;
        Ok(())
    }

    /// Fetch a single artifact by id.
    pub fn get_artifact(&self, id: &str) -> Result<Option<ArtifactRow>> {
        let json = self
            .conn
            .query_row(
                "SELECT row_json FROM artifacts WHERE id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    /// Fetch an artifact by its path within a session.
    pub fn get_artifact_by_path(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<Option<ArtifactRow>> {
        let json = self
            .conn
            .query_row(
                "SELECT row_json FROM artifacts WHERE session_id = ?1 AND path = ?2",
                (session_id, path),
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    /// List all artifacts for a session, ordered by id.
    pub fn list_artifacts(&self, session_id: &str) -> Result<Vec<ArtifactRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT row_json FROM artifacts WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let json = r?;
            out.push(serde_json::from_str(&json)?);
        }
        Ok(out)
    }

    /// List artifacts for a session filtered by trust label.
    pub fn list_artifacts_by_trust_label(
        &self,
        session_id: &str,
        trust_label: &str,
    ) -> Result<Vec<ArtifactRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT row_json FROM artifacts WHERE session_id = ?1 AND trust_label = ?2 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([session_id, trust_label], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let json = r?;
            out.push(serde_json::from_str(&json)?);
        }
        Ok(out)
    }
}
