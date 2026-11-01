//! The daemon's sole operational Ledger owner.
//!
//! CLI and MCP are clients of this owner. Lazy index repair is a cache effect
//! under an enrolled source grant, never permission to edit source documents.
//! Resolution tickets retain the original source expectations and task grant;
//! neither a cursor nor an agent-supplied root can increase read authority.

use super::{
    doc_spine,
    limits::WorkBudget,
    policy::SourcePolicy,
    query,
    resolve::{self, ResolveRequest},
    skill_documents, LedgerDb,
};
use crate::authorization::{self, AuthorizationRequest};
use rightkit_context_protocol::ReadPathV1;
use rightkit_db::rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::Duration;

const TICKET_TTL_MS: i64 = 10 * 60 * 1000;
const OWNER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ledger_owner_roots (
    repository_root TEXT PRIMARY KEY, generation INTEGER NOT NULL,
    policy_digest TEXT NOT NULL, synced_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ledger_erasure_fences (
    repository_root TEXT NOT NULL, path_digest TEXT NOT NULL,
    erased_at_ms INTEGER NOT NULL, PRIMARY KEY(repository_root,path_digest)
);
"#;
/// Ticket store schema for the durable catalog. Resolution tickets are
/// runtime state, not index projection: they live beside the catalog's
/// erasure/exclusion records so retrieval on the read connection never opens
/// a write transaction against the index file. Tickets are issued and
/// validated only through the catalog.
const TICKET_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ledger_resolution_tickets (
    ticket_hash TEXT PRIMARY KEY, repository_root TEXT NOT NULL,
    caller_digest TEXT NOT NULL, doc_id TEXT NOT NULL,
    request_json TEXT NOT NULL, grant_id TEXT, expires_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ledger_tickets_root ON ledger_resolution_tickets(repository_root,expires_at_ms);
"#;

static DAEMON_OWNER: OnceLock<Result<Arc<LedgerService>, String>> = OnceLock::new();

pub(crate) fn active_owner() -> Result<Arc<LedgerService>, String> {
    DAEMON_OWNER
        .get()
        .ok_or("membrane_unavailable:hub_inactive")?
        .clone()
}

/// Request-scoped owner; opens canonical storage without installing residency.
pub(crate) fn open_explicit_owner() -> Result<Arc<LedgerService>, String> {
    LedgerDb::open_default()
        .and_then(LedgerService::new)
        .map(Arc::new)
}

#[derive(Clone)]
pub(crate) struct Caller {
    pub root: String,
    registry_root: String,
    pub repository_id: String,
    pub scope_id: String,
    descriptor: Option<Value>,
    level: Option<String>,
}
impl Caller {
    /// Native federation has already canonicalized the root. Its repository
    /// identifier still must match, and the source grant must exist in the
    /// installation registry; a request string is not source enrollment.
    pub(crate) fn enrolled(root: &Path, repository: &str) -> Result<Self, String> {
        let canonical = resolve::normalized_root(root).map_err(|e| e.to_string())?;
        if rightkit_context_federation::root::canonical_repository_id(root) != repository {
            return Err("ledger_repository_binding_denied".into());
        }
        let registry = authorization::load_installation_registry().map_err(|e| e.to_string())?;
        let binding = registry
            .bindings()
            .iter()
            .find(|binding| {
                resolve::normalized_root(Path::new(&binding.root))
                    .ok()
                    .as_deref()
                    == Some(&canonical)
            })
            .ok_or("ledger_root_not_enrolled")?;
        Ok(Self {
            root: canonical,
            registry_root: binding.root.clone(),
            repository_id: binding.repository_id.clone(),
            scope_id: binding.scope_id.clone(),
            descriptor: binding.scope_descriptor.clone(),
            level: None,
        })
    }
    pub(crate) fn authorize(&self, action: &str) -> Result<(), String> {
        authorization::authorize(&AuthorizationRequest {
            caller_root: &self.registry_root,
            caller_repository_id: &self.repository_id,
            caller_scope_id: &self.scope_id,
            caller_scope_descriptor: self.descriptor.as_ref(),
            target_repository: &self.repository_id,
            task_grant_level: self.level.as_deref(),
            action,
        })
        .map(|_| ())
        .map_err(|e| format!("{}:{}", e.code(), e))
    }
    pub(crate) fn envelope(&self) -> Value {
        let mut value = json!({"root":self.registry_root,"repositoryId":self.repository_id,"scopeId":self.scope_id});
        if let Some(descriptor) = &self.descriptor {
            value["scopeDescriptor"] = descriptor.clone();
        }
        value
    }
    fn digest(&self) -> String {
        resolve::digest(
            format!("{}\0{}\0{}", self.root, self.repository_id, self.scope_id).as_bytes(),
        )
    }
}

pub(crate) struct LedgerService {
    db: LedgerDb,
    /// Second WAL connection to the same index file used by every read-only
    /// retrieval op. Published-index reads observe the last committed
    /// projection while maintenance holds a long write transaction — they
    /// never queue behind `operation` or wait out a sync pass. `None` for
    /// in-memory owners, which cannot open a second connection.
    read_db: Option<LedgerDb>,
    catalog: crate::catalog::ContextCatalog,
    operation: Mutex<()>,
    /// TEMP scope and erasure tables belong to one SQLite connection. Serialize
    /// read transactions so concurrent lanes cannot overwrite each other's
    /// per-request scope between installation and query execution.
    read_operation: Mutex<()>,
}
struct ResetProgress<'a>(&'a LedgerDb);
impl Drop for ResetProgress<'_> {
    fn drop(&mut self) {
        let _ = self.0.lock().progress_handler(0, None::<fn() -> bool>);
    }
}
impl LedgerService {
    fn new(db: LedgerDb) -> Result<Self, String> {
        let catalog = crate::catalog::ContextCatalog::open(
            crate::catalog::default_catalog_path().map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Self::with_catalog(db, catalog)
    }
    fn with_catalog(db: LedgerDb, catalog: crate::catalog::ContextCatalog) -> Result<Self, String> {
        // Owner DDL is a write transaction; a concurrent maintenance pass may
        // hold the index writer for a whole corpus walk, so the batches run
        // only when their objects are actually missing (fresh index).
        if !Self::owner_objects_ready(&db)? {
            db.lock()
                .execute_batch(super::diagnostics::SCHEMA)
                .map_err(|e| e.to_string())?;
            db.lock()
                .execute_batch(OWNER_SCHEMA)
                .map_err(|e| e.to_string())?;
        }
        let read_db = db.path().map(LedgerDb::open_reader).transpose()?;
        Ok(Self {
            db,
            read_db,
            catalog,
            operation: Mutex::new(()),
            read_operation: Mutex::new(()),
        })
    }

    /// True when the owner-layer objects (`diagnostics::SCHEMA` and
    /// `OWNER_SCHEMA`) already exist. Read-only so an open during maintenance
    /// never queues behind the writer for a no-op DDL batch.
    fn owner_objects_ready(db: &LedgerDb) -> Result<bool, String> {
        db.lock()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN (
                    'ledger_document_manifests','idx_ledger_manifest_document',
                    'ledger_erasure_fences')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count == 3)
            .map_err(|error| error.to_string())
    }

    fn read_db(&self) -> &LedgerDb {
        self.read_db.as_ref().unwrap_or(&self.db)
    }

    fn run<T>(
        &self,
        caller: &Caller,
        action: &str,
        budget: &WorkBudget,
        work: impl FnOnce(&LedgerDb) -> Result<T, String>,
    ) -> Result<T, String> {
        caller.authorize(action)?;
        let _operation = loop {
            budget.check()?;
            match self.operation.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(_)) => return Err("ledger_owner_unavailable".into()),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        let observed = budget.clone();
        let _ = self
            .db
            .lock()
            .progress_handler(1000, Some(move || observed.interrupted()));
        let _reset = ResetProgress(&self.db);
        super::erasure::synchronize(&self.catalog, &self.db, &caller.root, budget)?;
        let result = work(&self.db);
        budget.check()?;
        // Revocation is checked again before anything leaves the owner.
        caller.authorize(action)?;
        result
    }

    /// Read-only retrieval lane: published-index reads on the dedicated WAL
    /// connection. Unlike `run`, this never takes the `operation` mutex, so a
    /// maintenance sync holding the writer for a whole corpus pass cannot
    /// starve status/recall/skill reads — each op observes the last committed
    /// publication and degrades typed on real gaps rather than reporting a
    /// blanket availability floor. Erasure still binds: durable catalog
    /// exclusions are mirrored into the connection's TEMP view by
    /// `synchronize_read` before any query work, so a catalog-recorded
    /// erasure cannot resurrect through a stale fence cache.
    fn run_read<T>(
        &self,
        caller: &Caller,
        action: &str,
        budget: &WorkBudget,
        work: impl FnOnce(&LedgerDb) -> Result<T, String>,
    ) -> Result<T, String> {
        caller.authorize(action)?;
        let _read_operation = loop {
            budget.check()?;
            match self.read_operation.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(_)) => return Err("ledger_owner_unavailable".into()),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        let db = self.read_db();
        let observed = budget.clone();
        let _ = db
            .lock()
            .progress_handler(1000, Some(move || observed.interrupted()));
        let _reset = ResetProgress(db);
        super::erasure::synchronize_read(&self.catalog, db, &caller.root, budget)?;
        let result = work(db);
        budget.check()?;
        // Revocation is checked again before anything leaves the owner.
        caller.authorize(action)?;
        result
    }

    /// Persisted projection coverage for `root`: true once a maintenance sync
    /// has published a `ledger_owner_roots` row. Retrieval reads this cheaply
    /// instead of re-walking the worktree — absence means the projection was
    /// never built, which callers see as the `ledger_index_unpublished`
    /// omission rather than silently empty results.
    fn index_published(db: &LedgerDb, root: &str) -> bool {
        db.lock()
            .query_row(
                "SELECT 1 FROM ledger_owner_roots WHERE repository_root=?1",
                [root],
                |_| Ok(()),
            )
            .optional()
            .unwrap_or(None)
            .is_some()
    }

    fn sync_locked(
        db: &LedgerDb,
        caller: &Caller,
        budget: &WorkBudget,
    ) -> Result<doc_spine::DocSyncReport, String> {
        let report = doc_spine::sync_bounded(db, Path::new(&caller.root), budget)?;
        db.lock()
            .execute(
                "INSERT INTO ledger_owner_roots VALUES (?1,?2,?3,?4)
            ON CONFLICT(repository_root) DO UPDATE SET generation=excluded.generation,
            policy_digest=excluded.policy_digest,synced_at_ms=excluded.synced_at_ms",
                params![
                    caller.root,
                    report.index_generation,
                    report.policy_digest,
                    crate::time::now_millis() as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        Ok(report)
    }

    /// Authorized maintenance: reconcile the caller's enrolled root into the
    /// persisted projection. This is the same `sync_locked` index/update
    /// mechanism the explicit "sync" operation uses — invoked by the resident
    /// engine's reconcile pass under background authority and by direct
    /// maintenance calls, never by retrieval.
    pub(crate) fn maintain(
        &self,
        caller: &Caller,
        budget: &WorkBudget,
    ) -> Result<doc_spine::DocSyncReport, String> {
        self.run(caller, "context", budget, |db| {
            Self::sync_locked(db, caller, budget)
        })
    }

    /// Transcript lane maintenance (upstream #431): ingest the sources named by the user-enabled
    /// manifest, or purge the lane when it is off. Not tied to a repository root, so it takes no
    /// `Caller`; the manifest is the authorization (see `transcripts`). Shares the writer lock
    /// with `run` and stops on the same budget.
    pub(crate) fn maintain_transcripts(
        &self,
        budget: &WorkBudget,
    ) -> Result<super::transcripts::SyncReport, String> {
        let _operation = loop {
            budget.check()?;
            match self.operation.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(_)) => return Err("ledger_owner_unavailable".into()),
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        let observed = budget.clone();
        let _ = self
            .db
            .lock()
            .progress_handler(1000, Some(move || observed.interrupted()));
        let _reset = ResetProgress(&self.db);
        super::transcripts::maintain(
            &self.db,
            &self.catalog,
            &super::transcripts::manifest_path(),
            budget,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search(
        &self,
        caller: &Caller,
        task: &str,
        k: usize,
        literal: bool,
        ranges: Option<Vec<ReadPathV1>>,
        grant_id: Option<&str>,
        budget: &WorkBudget,
    ) -> Result<(query::QueryResult, Vec<String>), String> {
        self.run_read(caller, "context", budget, |db| {
            validate_task_grant(grant_id, caller, None, None)?;
            // Retrieval queries persisted projections only — full-root
            // reconciliation is maintenance work owned by the explicit "sync"
            // operation and the resident reconcile pass, never a read path.
            // Selected sources are still validated against live bytes,
            // revision and span hash inside `query::search`/`issue_ticket`,
            // so edited or deleted content degrades to typed omissions rather
            // than leaking stale text.
            let mut result = query::search(
                db,
                &query::QueryScope {
                    root: caller.root.clone(),
                    ranges,
                },
                task,
                k,
                literal,
                budget,
            )?;
            if !Self::index_published(db, &caller.root) {
                result.omissions.push("ledger_index_unpublished".into());
                result.complete = false;
            }
            let mut tickets = Vec::new();
            for hit in &result.hits {
                budget.check()?;
                tickets.push(issue_ticket(&self.catalog, caller, hit, grant_id)?);
            }
            validate_task_grant(grant_id, caller, None, None)?;
            Ok((result, tickets))
        })
    }

    /// Search portable skill documents through the existing scoped query path
    /// and issue a resolution ticket per emitted hit (LDG-032).
    ///
    /// `ranges` carries the caller's grant narrowing exactly as `search` does;
    /// the adapter intersects it with the registered skill-document set so a
    /// grant can narrow the lane but never widen it. Each returned pair is the
    /// complete `membrane_source_read` binding Pull needs for candidate
    /// materialization — hit plus owner-issued ticket.
    pub(crate) fn skill_documents(
        &self,
        caller: &Caller,
        task: &str,
        k: usize,
        ranges: Option<Vec<ReadPathV1>>,
        grant_id: Option<&str>,
        budget: &WorkBudget,
    ) -> Result<
        (
            query::QueryResult,
            Vec<skill_documents::TicketedSkillDocumentV1>,
        ),
        String,
    > {
        self.run_read(caller, "context", budget, |db| {
            validate_task_grant(grant_id, caller, None, None)?;
            let outcome = skill_documents::search(db, &caller.root, task, k, ranges, budget)?;
            let mut skills = Vec::new();
            for skill in outcome.skills {
                budget.check()?;
                let ticket = issue_ticket(&self.catalog, caller, &skill.hit, grant_id)?;
                skills.push(skill_documents::TicketedSkillDocumentV1 {
                    skill_id: skill.skill_id,
                    title: skill.title,
                    hit: skill.hit,
                    ticket,
                });
            }
            validate_task_grant(grant_id, caller, None, None)?;
            Ok((outcome.result, skills))
        })
    }

    /// Verify a Blueprint candidate against current confined source bytes and
    /// issue its normal caller-bound Ledger ticket. The ticket binds the
    /// graph's XXH128 identity and the independently computed SHA-256 used by
    /// source recovery; neither graph metadata nor a resolver string is proof.
    pub(crate) fn bind_blueprint_source(
        &self,
        context: &rightkit_context_provider_sdk::ProviderContext,
        relative: &str,
        expected_xxh: &str,
        budget: &WorkBudget,
    ) -> Result<(String, String), String> {
        let grant_task = blueprint_task_binding(context);
        let caller = Caller::enrolled(Path::new(&context.repository_root), &context.repository_id)?;
        caller.authorize("source_read")?;
        let (ranges, grant_id) = match &context.scope_grant {
            None => (None, None),
            Some(grant) => {
                validate_task_grant(
                    Some(&grant.id),
                    &caller,
                    Some(&grant.task_id),
                    Some(&context.session_id),
                )?;
                if grant.repository_root != context.repository_root
                    || grant.repository_id != context.repository_id
                    || grant.session_id != context.session_id
                    || !grant
                        .permitted_edge_types
                        .iter()
                        .any(|edge| edge == "source_read")
                {
                    return Err("ledger_scope_grant_binding_mismatch".into());
                }
                let mut allowed_ranges = Vec::new();
                for value in &grant.read_paths {
                    let (path, lines) =
                        value.rsplit_once(':').ok_or("ledger_range_grant_invalid")?;
                    let (start, end) = lines.split_once('-').ok_or("ledger_range_grant_invalid")?;
                    let start: u32 = start.parse().map_err(|_| "ledger_range_grant_invalid")?;
                    let end: u32 = end.parse().map_err(|_| "ledger_range_grant_invalid")?;
                    if start == 0 || end < start {
                        return Err("ledger_range_grant_invalid".into());
                    }
                    if path == relative && start == 1 {
                        allowed_ranges.push(end);
                    }
                }
                if allowed_ranges.is_empty() {
                    return Err("ledger_scope_ranges_unavailable".into());
                }
                (Some(allowed_ranges), Some(grant.id.clone()))
            }
        };
        // Parse through the canonical source-reference validator before any
        // filesystem access; the candidate path itself remains repo-relative.
        let source_ref = format!("{}{}", super::identifier::WORKTREE_DOC_PREFIX, relative);
        let parsed = super::identifier::WorktreeDocRef::parse(&source_ref)
            .map_err(|_| "ledger_path_denied")?;
        self.run_read(&caller, "source_read", budget, |db| {
            permitted_path(db, &caller.root, parsed.relative_path(), budget)?;
            let bytes = resolve::confined_bytes(Path::new(&caller.root), parsed.relative_path()).map_err(|e| e.to_string())?;
            budget.charge_bytes(bytes.len())?;
            if rightkit_repo_graph::identity::content_digest(&bytes) != expected_xxh { return Err("ledger_source_stale".into()); }
            if let Some(ends) = ranges.as_ref() {
                let text = std::str::from_utf8(&bytes).map_err(|_| "ledger_unsupported_encoding")?;
                let lines = text.lines().count().max(1) as u32;
                if !ends.iter().any(|end| *end >= lines) { return Err("ledger_scope_ranges_unavailable".into()); }
            }
            let sha = format!("sha256:{}", resolve::digest(&bytes));
        let request = ResolveRequest { doc_id: None, node_id: None, source_ref: source_ref.clone(), anchor_id: "blueprint.file".into(),
                expected_content_hash: sha.clone(), expected_revision: Some(format!("blueprint-bind:{}:{}:{}", context.session_id, grant_task, expected_xxh)),
                expected_span_hash: Some(expected_xxh.into()), ledger_generation: None, continuation_cursor: None, max_bytes: resolve::MAX_READ_BYTES };
            let ticket = issue_ticket_request(&self.catalog, &caller, &request, &format!("blueprint:{}", relative), grant_id.as_deref())?;
            caller.authorize("source_read")?;
            validate_task_grant(grant_id.as_deref(), &caller, Some(grant_task), Some(&context.session_id))?;
            budget.check()?;
            let arguments = json!({"operation":"source_read","repository":caller.repository_id,
                "caller":caller.envelope(),"sessionId":context.session_id,"taskId":grant_task,"sourceRef":request.source_ref,
                "anchorId":request.anchor_id,"expectedContentHash":request.expected_content_hash,
                "expectedRevision":request.expected_revision,"expectedSpanHash":request.expected_span_hash,
                "ledgerTicket":ticket,"maxBytes":resolve::MAX_READ_BYTES});
            Ok((json!({"tool":"membrane_source_read","arguments":arguments}).to_string(), sha))
        })
    }
}

fn blueprint_task_binding(context: &rightkit_context_provider_sdk::ProviderContext) -> &str {
    context
        .scope_grant
        .as_ref()
        .map(|grant| grant.task_id.as_str())
        .unwrap_or(&context.task)
}

pub(crate) fn validate_task_grant(
    id: Option<&str>,
    caller: &Caller,
    task: Option<&str>,
    session: Option<&str>,
) -> Result<(), String> {
    let Some(id) = id else {
        return Ok(());
    };
    let catalog = crate::catalog::ContextCatalog::open(
        crate::catalog::default_catalog_path().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let grant = crate::catalog::lookup_grant(&catalog, id)
        .map_err(|e| e.to_string())?
        .ok_or("ledger_scope_grant_missing")?;
    let canonical =
        rightkit_context_federation::root::canonical_repository_id(Path::new(&caller.root));
    if !grant.permits()
        || !grant
            .repository_ids
            .iter()
            .any(|id| id == &caller.repository_id || id == &canonical)
        || !grant
            .permitted_edge_types
            .iter()
            .any(|edge| edge == "source_read")
        || task.is_some_and(|task| grant.task_id != task)
        || session.is_some_and(|session| grant.session_id != session)
    {
        return Err("ledger_scope_grant_invalid".into());
    }
    Ok(())
}

pub(crate) fn permitted_path(
    db: &LedgerDb,
    root: &str,
    path: &str,
    budget: &WorkBudget,
) -> Result<(), String> {
    {
        let conn = db.lock();
        super::erasure::ensure_read_exclusions(&conn)?;
        let erased: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM ledger_erasure_fences WHERE repository_root=?1 AND path_digest=?2)
            OR EXISTS(SELECT 1 FROM ledger_read_exclusions WHERE path_digest=?2)",
            params![root,resolve::digest(path.as_bytes())],|r|r.get(0)).map_err(|e| e.to_string())?;
        if erased {
            return Err("ledger_source_erased".into());
        }
    }
    let mut policy = SourcePolicy::new(Path::new(root))?;
    if !policy.allows(path, false, budget)? {
        return Err("ledger_source_ineligible".into());
    }
    policy.revalidate(budget)
}

/// Resolution tickets live in the durable catalog, not the index file. A
/// ticket write is runtime state, not projection data: keeping it off the
/// index means a search lane on the read connection never needs a write
/// transaction there, so retrieval stays responsive while maintenance holds
/// the index writer. The catalog's own lock serializes its brief writes.
fn issue_ticket(
    catalog: &crate::catalog::ContextCatalog,
    caller: &Caller,
    hit: &query::LedgerHit,
    grant_id: Option<&str>,
) -> Result<String, String> {
    issue_ticket_request(
        catalog,
        caller,
        &hit.resolve_request(),
        &hit.doc_id,
        grant_id,
    )
}
fn issue_ticket_request(
    catalog: &crate::catalog::ContextCatalog,
    caller: &Caller,
    request: &ResolveRequest,
    doc_id: &str,
    grant_id: Option<&str>,
) -> Result<String, String> {
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|_| "ledger_ticket_entropy_unavailable")?;
    let ticket = format!("ledger-ticket:{}", rightkit_fs::digest::hex(&random));
    let now = crate::time::now_millis() as i64;
    let conn = catalog.lock();
    conn.execute_batch(TICKET_SCHEMA)
        .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM ledger_resolution_tickets WHERE expires_at_ms<=?1",
        [now],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM ledger_resolution_tickets WHERE ticket_hash IN (
        SELECT ticket_hash FROM ledger_resolution_tickets WHERE repository_root=?1 ORDER BY expires_at_ms DESC LIMIT -1 OFFSET 480)",
        [&caller.root]).map_err(|e|e.to_string())?;
    conn.execute(
        "INSERT INTO ledger_resolution_tickets VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            resolve::digest(ticket.as_bytes()),
            caller.root,
            caller.digest(),
            doc_id,
            serde_json::to_string(request).map_err(|e| e.to_string())?,
            grant_id,
            now + TICKET_TTL_MS
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(ticket)
}
