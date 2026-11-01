//! Final-form SQLite schema for Blueprint.
//!
//! A store is created once, in its final shape. There are no upgrade steps: a
//! store carrying any other schema version is rejected, never rewritten.

use rightkit_db::rusqlite::{self, Connection, OptionalExtension};
use rightkit_db::{DbError, MigrateOptions, Migration, VersionStorage};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("sqlite migration failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported schema version {0}")]
    UnsupportedVersion(i64),
    #[error("invalid schema version: {0}")]
    InvalidVersion(String),
    #[error("migration {from}->{to} failed: {detail}")]
    Failed { from: u32, to: u32, detail: String },
}

pub fn schema_version(conn: &Connection) -> Result<u32, MigrationError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match value {
        None => Ok(0),
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| MigrationError::InvalidVersion(v.clone()))
            .and_then(|n| {
                if n < 0 {
                    Err(MigrationError::InvalidVersion(v))
                } else if n > SCHEMA_VERSION as i64 {
                    Err(MigrationError::UnsupportedVersion(n))
                } else {
                    Ok(n as u32)
                }
            }),
    }
}

const SCHEMA_SQL: &str = r#"CREATE TABLE files(path TEXT PRIMARY KEY, content_hash TEXT, language TEXT, provider TEXT, parse_status TEXT, error_node_count INTEGER, generation_id TEXT NOT NULL, node_id TEXT, labels TEXT, name TEXT, qualified_name TEXT, confidence REAL, evidence TEXT, extra TEXT, node_ordinal INTEGER);
CREATE INDEX idx_files_generation ON files(generation_id);
CREATE INDEX idx_files_node_ordinal ON files(node_ordinal);
CREATE TABLE symbols(id TEXT PRIMARY KEY, kind TEXT NOT NULL, labels TEXT NOT NULL, name TEXT NOT NULL, qualified_name TEXT NOT NULL, path TEXT NOT NULL, confidence REAL, evidence TEXT NOT NULL, generation_id TEXT NOT NULL, extra TEXT, node_ordinal INTEGER);
CREATE INDEX idx_symbols_path ON symbols(path); CREATE INDEX idx_symbols_generation ON symbols(generation_id); CREATE INDEX idx_symbols_generation_name ON symbols(generation_id,name); CREATE INDEX idx_symbols_generation_qualified ON symbols(generation_id,qualified_name);
CREATE TABLE edges(id TEXT PRIMARY KEY, kind TEXT NOT NULL, source TEXT NOT NULL, target TEXT, confidence REAL, resolved INTEGER NOT NULL DEFAULT 1, specifier TEXT, evidence TEXT NOT NULL, generation_id TEXT NOT NULL, confidence_tier TEXT, extra TEXT);
CREATE INDEX idx_edges_source ON edges(source); CREATE INDEX idx_edges_target ON edges(target); CREATE INDEX idx_edges_kind ON edges(kind); CREATE INDEX idx_edges_generation ON edges(generation_id); CREATE INDEX idx_edges_tier ON edges(confidence_tier);
CREATE INDEX idx_edges_gen_source_kind ON edges(generation_id,source,kind); CREATE INDEX idx_edges_gen_target_kind ON edges(generation_id,target,kind);
CREATE TABLE vectors(node_id TEXT PRIMARY KEY, dim INTEGER NOT NULL, emb BLOB NOT NULL, model TEXT NOT NULL, generation_id TEXT NOT NULL); CREATE INDEX idx_vectors_generation ON vectors(generation_id); CREATE INDEX idx_vectors_model ON vectors(model);
CREATE TABLE generation(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE file_state(path TEXT PRIMARY KEY,content_digest TEXT NOT NULL,size INTEGER NOT NULL,mtime_ms REAL,file_identity TEXT,last_event_seq INTEGER,applied_clock INTEGER NOT NULL);
CREATE TABLE fact_owner(fact_id TEXT NOT NULL,fact_kind TEXT NOT NULL CHECK(fact_kind IN ('node','edge')),source_path TEXT NOT NULL,source_digest TEXT NOT NULL,provider_id TEXT NOT NULL,provider_version TEXT NOT NULL,freshness_domain TEXT NOT NULL CHECK(freshness_domain IN ('structural','doc','semantic')),fact_kind_detail TEXT,generation_id TEXT,repo_root TEXT,PRIMARY KEY(fact_id,fact_kind,provider_id));
CREATE INDEX idx_fact_owner_path ON fact_owner(source_path); CREATE INDEX idx_fact_owner_domain ON fact_owner(freshness_domain); CREATE INDEX idx_fact_owner_generation ON fact_owner(generation_id); CREATE INDEX idx_fact_owner_repo_root ON fact_owner(repo_root); CREATE INDEX idx_fact_owner_provider_path ON fact_owner(provider_id,source_path);
CREATE TABLE dependency_index(source_path TEXT NOT NULL,dependent_path TEXT NOT NULL,reason TEXT NOT NULL CHECK(reason IN ('import','call','route','schema','config','manifest')),PRIMARY KEY(source_path,dependent_path,reason)); CREATE INDEX idx_dep_source ON dependency_index(source_path);
CREATE TABLE generation_leaf(path TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind IN ('file','dir')),digest TEXT NOT NULL,parent_path TEXT,name TEXT); CREATE INDEX idx_generation_leaf_parent ON generation_leaf(parent_path,name);
CREATE TABLE watch_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE event_journal(seq INTEGER PRIMARY KEY AUTOINCREMENT,observed_ms INTEGER NOT NULL,event_kind TEXT NOT NULL CHECK(event_kind IN ('create','modify','delete','rename')),path TEXT NOT NULL,rename_to TEXT,source_clock INTEGER NOT NULL,applied INTEGER NOT NULL DEFAULT 0 CHECK(applied IN (0,1,2)),applied_clock INTEGER,UNIQUE(observed_ms,event_kind,path,rename_to,source_clock)); CREATE INDEX idx_event_journal_applied ON event_journal(applied,seq);
CREATE TABLE generation_receipt(receipt_id TEXT PRIMARY KEY,created_ms INTEGER NOT NULL,repo_root TEXT NOT NULL,generation_id TEXT,source_clock INTEGER NOT NULL,applied_clock INTEGER NOT NULL,event_gap INTEGER NOT NULL,barrier_result TEXT NOT NULL CHECK(barrier_result IN ('caught_up','gap_blocked','timeout')),details_json TEXT NOT NULL); CREATE INDEX idx_generation_receipt_created ON generation_receipt(created_ms);
CREATE TABLE artifact_state(artifact TEXT PRIMARY KEY,generation_id TEXT NOT NULL,fingerprint TEXT NOT NULL,updated_ms INTEGER NOT NULL);
CREATE VIRTUAL TABLE symbol_search USING fts5(id UNINDEXED,generation_id UNINDEXED,name,qualified_name,path);
CREATE TABLE symbol_terms(generation_id TEXT NOT NULL,token TEXT NOT NULL,symbol_id TEXT NOT NULL,PRIMARY KEY(generation_id,token,symbol_id)) WITHOUT ROWID; CREATE INDEX idx_symbol_terms_symbol ON symbol_terms(symbol_id);
CREATE TABLE documents(id TEXT PRIMARY KEY,path TEXT NOT NULL,content_hash TEXT,lifecycle_status TEXT,lifecycle_superseded_by TEXT,lifecycle_superseded_on TEXT,generated_at TEXT,generation_id TEXT NOT NULL); CREATE INDEX idx_documents_generation ON documents(generation_id); CREATE INDEX idx_documents_path ON documents(path);
CREATE TABLE claims(id TEXT PRIMARY KEY,document_id TEXT NOT NULL,source TEXT,line INTEGER,text TEXT NOT NULL,status TEXT,sha1 TEXT,generation_id TEXT NOT NULL,FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE CASCADE); CREATE INDEX idx_claims_generation ON claims(generation_id); CREATE INDEX idx_claims_document ON claims(document_id);
CREATE TABLE claim_code_edges(id TEXT PRIMARY KEY,claim_id TEXT NOT NULL,kind TEXT NOT NULL,source TEXT NOT NULL,target TEXT NOT NULL,confidence REAL,confidence_class TEXT,reason TEXT,evidence_doc_path TEXT,evidence_doc_line INTEGER,evidence_doc_sha1 TEXT,evidence_code_path TEXT,evidence_code_exists INTEGER,evidence_code_node_id TEXT,evidence_code_content_hash TEXT,generation_id TEXT NOT NULL,FOREIGN KEY(claim_id) REFERENCES claims(id) ON DELETE CASCADE); CREATE INDEX idx_claim_code_edges_generation ON claim_code_edges(generation_id); CREATE INDEX idx_claim_code_edges_claim ON claim_code_edges(claim_id); CREATE INDEX idx_claim_code_edges_kind ON claim_code_edges(kind);
CREATE TABLE document_supersession(id TEXT PRIMARY KEY,source_kind TEXT NOT NULL,source_doc TEXT,source_line INTEGER,source_text TEXT,source_external INTEGER NOT NULL DEFAULT 0,target_doc TEXT,target_external INTEGER NOT NULL DEFAULT 0,target_match TEXT,superseded_on TEXT,generation_id TEXT NOT NULL); CREATE INDEX idx_document_supersession_generation ON document_supersession(generation_id);
CREATE TABLE annotation_nodes(id TEXT PRIMARY KEY,node_ordinal INTEGER NOT NULL,payload TEXT NOT NULL,generation_id TEXT NOT NULL); CREATE INDEX idx_annotation_nodes_generation_ordinal ON annotation_nodes(generation_id,node_ordinal);
CREATE TABLE provider_ranks(provider_id TEXT PRIMARY KEY,rank INTEGER NOT NULL UNIQUE);
CREATE TABLE node_provider(node_id TEXT PRIMARY KEY,provider_id TEXT NOT NULL,source_path TEXT NOT NULL,provider_version TEXT NOT NULL DEFAULT 'unknown'); CREATE INDEX idx_node_provider_provider_path ON node_provider(provider_id,source_path);
CREATE TABLE named_snapshot(name TEXT PRIMARY KEY,repo_root TEXT NOT NULL,generation_id TEXT NOT NULL,manifest_digest TEXT NOT NULL,identity_json TEXT NOT NULL,created_ms INTEGER NOT NULL); CREATE INDEX idx_named_snapshot_generation ON named_snapshot(generation_id);
CREATE TABLE task_seed_vectors(model_fingerprint TEXT NOT NULL,prompt_sha256 TEXT NOT NULL,dim INTEGER NOT NULL CHECK(dim>0),embedding BLOB NOT NULL,PRIMARY KEY(model_fingerprint,prompt_sha256)) WITHOUT ROWID;"#;

const MIGRATIONS: &[Migration] = &[Migration::sql(1, "schema", SCHEMA_SQL)];

/// Create the final schema in a fresh store; a store already at the current
/// version is left as is. Creation is one transaction.
pub fn migrate(conn: &mut Connection) -> Result<u32, MigrationError> {
    let options = MigrateOptions {
        version_storage: VersionStorage::Table {
            table: "meta".into(),
            key: "schema_version".into(),
        },
        backup: false,
        optimize_after: false,
        ..MigrateOptions::default()
    };
    rightkit_db::migrate(conn, None, MIGRATIONS, &options).map_err(migration_error)?;
    Ok(SCHEMA_VERSION)
}

fn migration_error(error: DbError) -> MigrationError {
    match error {
        DbError::Sqlite(source) => MigrationError::Sqlite(source),
        DbError::NewerDatabase { current, .. } => MigrationError::UnsupportedVersion(current),
        DbError::MigrationFailed {
            version, source, ..
        } => MigrationError::Failed {
            from: version as u32 - 1,
            to: version as u32,
            detail: source.to_string(),
        },
        DbError::InvalidMigrations(detail) => MigrationError::InvalidVersion(detail),
        other => MigrationError::Failed {
            from: 0,
            to: 0,
            detail: other.to_string(),
        },
    }
}
