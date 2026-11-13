//! Partial reads of a stored generation for an incremental refresh (#635).
//!
//! A refresh that knows which files changed reads, through the store's path and edge indexes,
//! only the rows those files touch: their nodes, their outgoing edges and the files whose edges
//! point at them. It never materialises the whole graph, so its resident cost follows the size of
//! the change, not the size of the repository. Everything here is read-only; the delta is written
//! by `delta_store`.

use crate::api::BlueprintError;
use crate::engine::{graph_edge_from_store, graph_node_from_store, store_error};
use crate::graph::{FileFacts, FileReport, PrecisionTier};
use crate::store::{self, StoreError};
use rightkit_db::rusqlite::{self, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// Symbol ids and owning paths, keyed by symbol name.
pub(crate) type SymbolsByName = HashMap<String, Vec<(String, String)>>;

/// The sealed identity of the stored generation, read from the envelope only.
pub(crate) struct StoredEnvelope {
    pub generation_id: String,
    pub source_hash: String,
    pub complete: bool,
    pub truncation_reasons: Value,
    pub observation: Option<Value>,
}

pub(crate) struct RefreshView {
    conn: Connection,
}

fn db<T>(result: rusqlite::Result<T>) -> Result<T, BlueprintError> {
    result.map_err(|error| store_error(StoreError::Sqlite(error)))
}

fn first_path(evidence: &[Value]) -> Option<&str> {
    evidence
        .iter()
        .find_map(|item| item.get("path").and_then(Value::as_str))
}

impl RefreshView {
    pub(crate) fn open(db_path: &Path) -> Result<Self, BlueprintError> {
        if !db_path.exists() {
            return Err(BlueprintError::new(
                "blueprint_store_missing",
                "no persisted Blueprint generation exists",
            ));
        }
        let conn = store::open_store_read_only(db_path).map_err(store_error)?;
        // A streaming pass over a multi-GB store must not map it into the resident set.
        db(conn.pragma_update(None, "mmap_size", 0i64))?;
        Ok(Self { conn })
    }

    pub(crate) fn envelope(&self) -> Result<StoredEnvelope, BlueprintError> {
        let envelope = store::read_generation_envelope(&self.conn)
            .map_err(store_error)?
            .ok_or_else(|| {
                BlueprintError::new(
                    "blueprint_store_missing",
                    "no persisted Blueprint generation exists",
                )
            })?;
        let manifest = envelope.manifest.ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_corrupt",
                "persisted generation manifest is missing",
            )
        })?;
        let generation_id = manifest
            .get("generationId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BlueprintError::new(
                    "blueprint_store_corrupt",
                    "persisted generation identity is missing",
                )
            })?
            .to_owned();
        Ok(StoredEnvelope {
            generation_id,
            source_hash: manifest
                .get("sourceHash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            complete: manifest
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            truncation_reasons: manifest
                .get("truncationReasons")
                .cloned()
                .unwrap_or_else(|| json!([])),
            observation: envelope.source_observation,
        })
    }

    /// Stored node, edge and file counts, from table counts and never from rows.
    pub(crate) fn counts(&self) -> Result<(u64, u64, u64), BlueprintError> {
        let (nodes, edges) = store::stored_graph_rows(&self.conn).map_err(store_error)?;
        let files: i64 = db(self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)))?;
        Ok((nodes, edges, files.max(0) as u64))
    }

    /// Every stored file path with its content hash: the identity a change is measured against.
    pub(crate) fn file_hashes(&self) -> Result<Vec<(String, String)>, BlueprintError> {
        let mut statement = db(self.conn.prepare(
            "SELECT path, COALESCE(json_extract(evidence, '$[0].contentHash'), content_hash, '') FROM files",
        ))?;
        let rows = db(statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))))?;
        let mut hashes = Vec::new();
        for row in rows {
            hashes.push(db(row)?);
        }
        Ok(hashes)
    }

    /// Ids of every node a path owns: its file node, symbols and annotation nodes.
    pub(crate) fn node_ids(&self, path: &str) -> Result<Vec<String>, BlueprintError> {
        let mut ids = vec![format!("file:{path}")];
        let mut statement = db(self.conn.prepare(
            "SELECT COALESCE(node_id, 'file:' || path) FROM files WHERE path = ?1 \
             UNION SELECT id FROM symbols WHERE path = ?1 \
             UNION SELECT id FROM annotation_nodes WHERE json_extract(payload, '$.path') = ?1",
        ))?;
        let rows = db(statement.query_map([path], |row| row.get::<_, String>(0)))?;
        for row in rows {
            let id = db(row)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Paths whose edges point at a node of any of `paths`: the one-hop callers and importers a
    /// change can leave pointing at a symbol that moved or vanished.
    pub(crate) fn referencing_paths(
        &self,
        paths: &[String],
    ) -> Result<BTreeSet<String>, BlueprintError> {
        let own: HashSet<&str> = paths.iter().map(String::as_str).collect();
        let mut referencing = BTreeSet::new();
        let mut statement = db(self
            .conn
            .prepare("SELECT evidence FROM edges WHERE target = ?1"))?;
        for path in paths {
            for id in self.node_ids(path)? {
                let rows = db(statement.query_map([id.as_str()], |row| row.get::<_, String>(0)))?;
                for row in rows {
                    let evidence: Vec<Value> = serde_json::from_str(&db(row)?)
                        .map_err(|error| store_error(StoreError::Json(error)))?;
                    if let Some(source) = first_path(&evidence) {
                        if !source.is_empty() && !own.contains(source) {
                            referencing.insert(source.to_owned());
                        }
                    }
                }
            }
        }
        Ok(referencing)
    }

    /// Stored rows a rebuild of `path` rewrites: its nodes and the edges leaving them.
    pub(crate) fn rows_for_path(&self, path: &str) -> Result<u64, BlueprintError> {
        let rows: i64 = db(self.conn.query_row(
            "SELECT 1 + (SELECT COUNT(*) FROM symbols WHERE path = ?1) \
             + (SELECT COUNT(*) FROM edges WHERE source IN (SELECT id FROM symbols WHERE path = ?1)) \
             + (SELECT COUNT(*) FROM edges WHERE source = 'file:' || ?1)",
            [path],
            |row| row.get(0),
        ))?;
        Ok(rows.max(0) as u64)
    }

    /// The stored facts of one source path, rebuilt from its own rows only.
    pub(crate) fn facts(&self, path: &str) -> Result<Option<FileFacts>, BlueprintError> {
        let mut file = None;
        let mut nodes = Vec::new();
        for value in store::load_path_nodes(&self.conn, path).map_err(store_error)? {
            let node = graph_node_from_store(value)?;
            if node.kind == "file" {
                file.get_or_insert(node);
            } else {
                nodes.push(node);
            }
        }
        let Some(file) = file else {
            return Ok(None);
        };
        let mut edges = Vec::new();
        for id in std::iter::once(file.id.as_str()).chain(nodes.iter().map(|node| node.id.as_str()))
        {
            for value in store::load_node_edges(&self.conn, id).map_err(store_error)? {
                let edge = graph_edge_from_store(value)?;
                if edge
                    .evidence
                    .iter()
                    .any(|item| item.get("path").and_then(Value::as_str) == Some(path))
                {
                    edges.push(edge);
                }
            }
        }
        let report = store::load_path_file_report(&self.conn, path)
            .map_err(store_error)?
            .and_then(|value| serde_json::from_value::<FileReport>(value).ok())
            .unwrap_or_else(|| FileReport {
                path: path.to_owned(),
                language: None,
                provider: "native-rust".into(),
                precision: PrecisionTier::Lexical,
                parse_status: "unknown".into(),
                error_node_count: 0,
                error: None,
            });
        let Some(content_digest) = file
            .evidence
            .first()
            .and_then(|evidence| evidence.get("contentHash"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(None);
        };
        Ok(Some(FileFacts {
            file,
            nodes,
            edges,
            report,
            content_digest,
            size: 0,
        }))
    }

    /// Names of the call-resolvable symbols a path stores.
    pub(crate) fn symbol_names(&self, path: &str) -> Result<Vec<String>, BlueprintError> {
        let mut statement = db(self
            .conn
            .prepare("SELECT name FROM symbols WHERE path = ?1 AND kind = 'symbol'"))?;
        let rows = db(statement.query_map([path], |row| row.get::<_, String>(0)))?;
        let mut names = Vec::new();
        for row in rows {
            names.push(db(row)?);
        }
        Ok(names)
    }

    /// Call-resolvable symbols carrying any of `names`, streamed so only matches are held.
    pub(crate) fn symbols_named(
        &self,
        names: &HashSet<String>,
    ) -> Result<SymbolsByName, BlueprintError> {
        let mut found = SymbolsByName::new();
        if names.is_empty() {
            return Ok(found);
        }
        let mut statement = db(self
            .conn
            .prepare("SELECT name, id, path FROM symbols WHERE kind = 'symbol'"))?;
        let mut rows = db(statement.query([]))?;
        while let Some(row) = db(rows.next())? {
            let name: String = db(row.get(0))?;
            if !names.contains(&name) {
                continue;
            }
            found
                .entry(name)
                .or_default()
                .push((db(row.get(1))?, db(row.get(2))?));
        }
        Ok(found)
    }

    /// Paths whose stored edges a change can newly resolve or make ambiguous: calls to a name
    /// whose candidate count crossed a resolution boundary (`call_names`), and unresolved imports
    /// that now land on a created file (`created`). Streamed; only source paths are held.
    pub(crate) fn reference_dependents(
        &self,
        call_names: &HashSet<String>,
        created: &HashSet<String>,
        after_paths: &HashSet<String>,
    ) -> Result<BTreeSet<String>, BlueprintError> {
        let mut dependents = BTreeSet::new();
        if call_names.is_empty() && created.is_empty() {
            return Ok(dependents);
        }
        let mut statement = db(self.conn.prepare(
            "SELECT kind, target IS NULL, evidence FROM edges \
             WHERE kind IN ('CALLS', 'CALL', 'IMPORTS', 'IMPORT')",
        ))?;
        let mut rows = db(statement.query([]))?;
        while let Some(row) = db(rows.next())? {
            let kind: String = db(row.get(0))?;
            let is_call = matches!(kind.as_str(), "CALLS" | "CALL");
            let unresolved: bool = db(row.get(1))?;
            if is_call {
                if call_names.is_empty() {
                    continue;
                }
            } else if created.is_empty() || !unresolved {
                continue;
            }
            let evidence_text: String = db(row.get(2))?;
            let evidence: Vec<Value> = serde_json::from_str(&evidence_text)
                .map_err(|error| store_error(StoreError::Json(error)))?;
            let Some(source) = first_path(&evidence) else {
                continue;
            };
            let Some(reference) = evidence.iter().find_map(|item| {
                item.get("callName")
                    .or_else(|| item.get("specifier"))
                    .and_then(Value::as_str)
            }) else {
                continue;
            };
            let affected = if is_call {
                call_names.contains(reference)
            } else {
                crate::module_resolution::resolve_import_with(source, reference, |path| {
                    after_paths.contains(path)
                })
                .is_some_and(|target| created.contains(&target))
            };
            if affected {
                dependents.insert(source.to_owned());
            }
        }
        Ok(dependents)
    }
}
