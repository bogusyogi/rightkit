//! Named-manifest retention for published document nodes.
//!
//! A manifest is a bounded rebuildable projection, not a change feed or durable
//! truth.
use super::{index, resolve};
use rightkit_db::rusqlite::{self, params, Transaction};
use serde::Serialize;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ledger_document_manifests (
 manifest_id TEXT PRIMARY KEY, repository_root TEXT NOT NULL, doc_id TEXT NOT NULL,
 source_revision TEXT NOT NULL, content_hash TEXT NOT NULL, projection_version TEXT NOT NULL,
 nodes_json TEXT NOT NULL, ledger_generation INTEGER NOT NULL, created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ledger_manifest_document ON ledger_document_manifests(repository_root,doc_id,created_at_ms);
"#;
const MANIFEST_VERSION: &str = "ledger.manifest.v1";
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const RETAINED_MANIFESTS: i64 = 4;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ManifestNode {
    node_id: String,
    anchor: String,
    parent: Option<String>,
    kind: String,
    start: i64,
    end: i64,
    span_hash: String,
}
fn manifest_id(root: &str, doc: &str, hash: &str, projection: &str, nodes: &str) -> String {
    format!(
        "ledger.manifest:{}",
        resolve::digest(
            format!("{MANIFEST_VERSION}\0{root}\0{doc}\0{hash}\0{projection}\0{nodes}").as_bytes()
        )
    )
}

/// Called inside the exact transaction that publishes this document's nodes.
/// Returns `Ok(false)` when the document's projected nodes exceed the manifest
/// byte budget: manifest retention is bounded rebuildable history, not the
/// index itself, so the document keeps its publication and stays searchable
/// while manifest readers degrade to typed `ledger_baseline_unavailable`.
/// Only genuine storage/serialization failures propagate as errors.
pub(crate) fn record_manifest_tx(tx: &Transaction<'_>, doc_id: &str) -> rusqlite::Result<bool> {
    tx.execute_batch(SCHEMA)?;
    let (root,revision,hash,generation):(String,String,String,i64)=tx.query_row(
        "SELECT repository_root,revision,content_hash,index_generation FROM ledger_doc_artifacts WHERE doc_id=?1",
        [doc_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let nodes = {
        let mut statement=tx.prepare("SELECT node_id,anchor_id,parent_id,node_kind,source_start_byte,source_end_byte,span_hash
            FROM ledger_nodes WHERE doc_id=?1 AND ledger_generation=?2 ORDER BY ordinal,node_id LIMIT 32769")?;
        let rows = statement
            .query_map(params![doc_id, generation], |r| {
                Ok(ManifestNode {
                    node_id: r.get(0)?,
                    anchor: r.get(1)?,
                    parent: r.get(2)?,
                    kind: r.get(3)?,
                    start: r.get(4)?,
                    end: r.get(5)?,
                    span_hash: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let nodes = serde_json::to_string(&nodes)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    if nodes.len() > MAX_MANIFEST_BYTES {
        crate::lifecycle_log::emit(
            serde_json::json!({"event":"ledger_manifest_oversized","docId":doc_id,"manifestBytes":nodes.len(),"budgetBytes":MAX_MANIFEST_BYTES}),
        );
        return Ok(false);
    }
    let id = manifest_id(
        &root,
        doc_id,
        &hash,
        index::PROJECTION_SCHEMA_VERSION,
        &nodes,
    );
    tx.execute(
        "INSERT OR IGNORE INTO ledger_document_manifests VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            id,
            root,
            doc_id,
            revision,
            hash,
            index::PROJECTION_SCHEMA_VERSION,
            nodes,
            generation,
            crate::time::now_millis() as i64
        ],
    )?;
    tx.execute(
        "DELETE FROM ledger_document_manifests WHERE manifest_id IN (
        SELECT manifest_id FROM ledger_document_manifests WHERE repository_root=?1 AND doc_id=?2
        ORDER BY created_at_ms DESC,manifest_id LIMIT -1 OFFSET ?3)",
        params![root, doc_id, RETAINED_MANIFESTS],
    )?;
    Ok(true)
}
