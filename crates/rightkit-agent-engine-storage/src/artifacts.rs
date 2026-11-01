use crate::error::Result;
use crate::Storage;
use rightkit_agent_engine_protocol::ArtifactRow;
use rightkit_db::rusqlite::OptionalExtension;
use std::path::{Component, Path};

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
            producer: "rightkit-browser".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_artifact_is_hash_bound_untrusted_and_epoch_scoped() {
        let storage = Storage::open_in_memory().unwrap();
        let row = storage
            .record_browser_artifact(
                "s1",
                "bs-1",
                7,
                "screenshot",
                "browser/bs-1/7.png",
                "image/png",
                b"png",
            )
            .unwrap();
        assert_eq!(row.trust_label, "untrusted-web");
        assert!(row.provenance.contains("observation-epoch:7"));
        assert_eq!(storage.get_artifact(&row.id).unwrap().unwrap().id, row.id);
    }

    #[test]
    fn browser_artifact_refuses_escape_and_unknown_kind() {
        let storage = Storage::open_in_memory().unwrap();
        for (kind, path) in [
            ("download", "../secret"),
            ("download", "..\\secret"),
            ("executable", "browser/x"),
        ] {
            assert!(storage
                .record_browser_artifact(
                    "s1",
                    "bs-1",
                    1,
                    kind,
                    path,
                    "application/octet-stream",
                    b"x",
                )
                .is_err());
        }
    }

    #[test]
    fn browser_artifact_ids_do_not_collide_across_paths_or_kinds() {
        let storage = Storage::open_in_memory().unwrap();
        let first = storage
            .record_browser_artifact(
                "s1",
                "bs-1",
                1,
                "dom",
                "browser/a.json",
                "application/json",
                b"{}",
            )
            .unwrap();
        let second = storage
            .record_browser_artifact(
                "s1",
                "bs-1",
                1,
                "ax",
                "browser/b.json",
                "application/json",
                b"{}",
            )
            .unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(storage.list_artifacts("s1").unwrap().len(), 2);
    }
}
