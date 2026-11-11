//! Membrane durable-memory and context operations.
//!
//! One-shot commands run in-process against the stores and never load the embedding model. The
//! resident (`membrane serve`) answers the same questions through the `op_*` functions below, so
//! both paths share one implementation. `membrane memory <command>` forwards to the
//! durable-memory command set defined here.

use crate::scope::normalize_scope;
use crate::{CheckpointV1, MemDb, MemoryStore};
use clap::{Parser, Subcommand};
use rightkit_db::rusqlite;
use rightkit_fs::digest::{sha256_bytes, Sha256Hasher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::fmt;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

thread_local! {
    static CAPTURED_IO: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

pub(crate) fn read_request_stdin() -> Result<Vec<u8>, String> {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(input)
}

/// Run `f` with this thread's stdout and stderr captured instead of written, so the resident can
/// return what a one-shot routine prints as the data of an answer.
pub(crate) fn capture_output<T>(f: impl FnOnce() -> T) -> (T, String, String) {
    CAPTURED_IO.with(|slot| *slot.borrow_mut() = Some((String::new(), String::new())));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let (stdout, stderr) = CAPTURED_IO.with(|slot| slot.borrow_mut().take().unwrap_or_default());
    match result {
        Ok(value) => (value, stdout, stderr),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub(crate) fn emit_stdout(args: fmt::Arguments<'_>) {
    emit_stdout_fragment(args, true);
}

fn emit_stdout_fragment(args: fmt::Arguments<'_>, newline: bool) {
    CAPTURED_IO.with(|slot| {
        if let Some((stdout, _)) = slot.borrow_mut().as_mut() {
            const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
            if stdout.len() < MAX_CAPTURE_BYTES {
                stdout.push_str(&args.to_string());
                if newline {
                    stdout.push('\n');
                }
                if stdout.len() > MAX_CAPTURE_BYTES {
                    let mut end = MAX_CAPTURE_BYTES;
                    while !stdout.is_char_boundary(end) {
                        end -= 1;
                    }
                    stdout.truncate(end);
                }
            }
        } else {
            if newline {
                std::println!("{}", args);
            } else {
                std::print!("{}", args);
            }
        }
    });
}

pub(crate) fn emit_stderr(args: fmt::Arguments<'_>) {
    CAPTURED_IO.with(|slot| {
        if let Some((_, stderr)) = slot.borrow_mut().as_mut() {
            const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;
            if stderr.len() < MAX_CAPTURE_BYTES {
                stderr.push_str(&args.to_string());
                stderr.push('\n');
                if stderr.len() > MAX_CAPTURE_BYTES {
                    let mut end = MAX_CAPTURE_BYTES;
                    while !stderr.is_char_boundary(end) {
                        end -= 1;
                    }
                    stderr.truncate(end);
                }
            }
        } else {
            std::eprintln!("{}", args);
        }
    });
}

macro_rules! println {
    () => {{ crate::cli::emit_stdout(format_args!("")) }};
    ($($arg:tt)*) => {{ crate::cli::emit_stdout(format_args!($($arg)*)) }}
}
macro_rules! print {
    ($($arg:tt)*) => {{ crate::cli::emit_stdout_fragment(format_args!($($arg)*), false) }}
}
macro_rules! eprintln {
    () => {{ crate::cli::emit_stderr(format_args!("")) }};
    ($($arg:tt)*) => {{ crate::cli::emit_stderr(format_args!($($arg)*)) }}
}
use std::time::{Duration, UNIX_EPOCH};

fn insight_admission_requests(
    issues: &[rightkit_adapt::insights::sealed_issue::SealedInsightIssueV1],
    installation_id: &str,
) -> Vec<rightkit_adapt::gates::CortexAdmissionEnvelope> {
    issues
        .iter()
        .map(|issue| issue.cortex_admission_envelope(installation_id))
        .collect()
}

/// Resolve WORKSPACE root (where `tools/skills` lives) for `skill-read`, independent of current
/// repo: explicit `--root`, else `$WORKSPACE_ROOT`, else deployed Membrane binary location.
fn skill_workspace_root(root: Option<String>) -> PathBuf {
    if let Some(r) = root.filter(|r| !r.trim().is_empty()) {
        return PathBuf::from(r);
    }
    if let Some(r) = crate::qa_env::workspace_root().filter(|r| !r.is_empty()) {
        return PathBuf::from(r);
    }
    if let Ok(exe) = std::env::current_exe() {
        // <workspace>/tools/bin/membrane.exe -> up 3 = <workspace>
        if let Some(ws) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            if ws.join("tools/skills").is_dir() {
                return ws.to_path_buf();
            }
        }
    }
    PathBuf::from(".")
}

fn skill_event_client() -> &'static str {
    match std::env::var("RIGHTKIT_CONTEXT_CLIENT")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "claude" => "claude",
        "claudemm" => "claudemm",
        "codex" => "codex",
        "manual" => "manual",
        "smoke" => "smoke",
        _ => "unknown",
    }
}

/// Best-effort, content-free evidence that a skill body was actually returned. Catalog display and
/// metadata verification do not call this function; only successful disk/engine body reads do.
fn emit_skill_resolved(ws: &Path, name: &str, body_hash: &str, source: &str, bytes: usize) {
    let target = crate::qa_env::skill_event_path()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| ws.join("tools/.cache/metrics/membrane-skill-events.jsonl"));
    let row = serde_json::json!({
        "schema_version": 1,
        "ts": crate::time::now_iso(),
        "event": "skill_resolved",
        "skill_id": format!("skills:{name}"),
        "resource_class": "skill_md",
        "body_sha256": body_hash,
        "source": source,
        "bytes_returned": bytes,
        "client": skill_event_client(),
    });
    let Ok(line) = serde_json::to_string(&row) else {
        return;
    };
    if let Some(parent) = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let _ = rightkit_fs::append_record(
        &target,
        format!("{line}\n").as_bytes(),
        rightkit_fs::AppendOptions::default().durable(false),
    );
}

fn write_skill_body(name: &str, body: &str) -> Result<(), String> {
    if CAPTURED_IO.with(|slot| slot.borrow().is_some()) {
        emit_stdout_fragment(format_args!("{}", body), false);
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle
        .write_all(body.as_bytes())
        .map_err(|error| format!("skill-read {name}: write stdout: {error}"))?;
    handle
        .flush()
        .map_err(|error| format!("skill-read {name}: flush stdout: {error}"))
}

/// `skill-read <name>`: resolve and print a workspace skill body. DB-less; runs before DB
/// resolution. Traversal- and escape-guarded.
fn run_skill_read(
    name: String,
    root: Option<String>,
    max_chars: usize,
    privacy_values: Option<PathBuf>,
) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || name.contains('/')
        || name.contains('\\')
        || name.starts_with('.')
        || name.contains("..")
    {
        return Err(format!("invalid skill name: {name:?}"));
    }
    if max_chars == 0 || max_chars > 262_144 {
        return Err("skill-read max_chars must be between 1 and 262144".to_owned());
    }
    let privacy_values = match privacy_values {
        Some(path) => serde_json::from_str::<std::collections::BTreeMap<String, String>>(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!("skill-read privacy values {}: {error}", path.display())
            })?,
        )
        .map_err(|error| format!("skill-read privacy values must be a string map: {error}"))?,
        None => std::collections::BTreeMap::new(),
    };
    // DISK-FIRST, ENGINE-FALLBACK. Disk = the git-tracked authoring source (always current on a
    // machine that has it). The engine `skills` table is the PORTABILITY store — it travels with
    // the DB, so a session/machine WITHOUT the skills directory (no checkout, no symlink) still
    // loads skills. This ordering has no staleness window: authoring edits are picked up
    // immediately where the files exist; the engine row (refreshed by `cortex reindex`) serves
    // everywhere else.
    let ws = skill_workspace_root(root);
    let skills_dir = ws.join("tools/skills");
    let path = skills_dir.join(&name).join("SKILL.md");
    if let (Ok(resolved), Ok(skills_canon)) = (
        std::fs::canonicalize(&path),
        std::fs::canonicalize(&skills_dir),
    ) {
        if !resolved.starts_with(&skills_canon) {
            return Err(format!("skill-read {name}: path escapes the skills root"));
        }
        let body =
            std::fs::read_to_string(&resolved).map_err(|e| format!("skill-read {name}: {e}"))?;
        let stored_hash = sha256_bytes(body.as_bytes());
        let (resolved, restored, unresolved) =
            crate::store::restore_skill_privacy_placeholders(&body, &privacy_values);
        let resolved_chars = resolved.chars().count();
        let bounded = resolved.chars().take(max_chars).collect::<String>();
        let truncated = resolved_chars > max_chars;
        let resolved_hash = sha256_bytes(bounded.as_bytes());
        let mut causes = Vec::new();
        if truncated {
            causes.push("content_truncated");
        }
        if unresolved > 0 {
            causes.push("privacy_value_unavailable");
        }
        let exact = causes.is_empty();
        let receipt = serde_json::json!({
            "schemaVersion": 1,
            "name": name,
            "source": "disk",
            "storedBodyHash": stored_hash,
            "resolvedBodyHash": resolved_hash,
            "restoredPrivacyValues": restored,
            "unresolvedPrivacyValues": unresolved,
            "completeness": {
                "schemaVersion": 1,
                "state": if exact { "exact" } else { "lower_bound" },
                "causes": causes,
                "consideredCount": if truncated { max_chars + 1 } else { resolved_chars },
                "returnedCount": bounded.chars().count(),
                "droppedCount": usize::from(truncated),
                "countsExact": exact,
            }
        });
        write_skill_body(&name, &bounded)?;
        eprintln!("{receipt}");
        emit_skill_resolved(&ws, &name, &resolved_hash, "disk", bounded.len());
        return Ok(());
    }
    // No disk copy — serve from the engine store.
    {
        let db = resolve_db(None);
        if let Ok(memdb) = crate::MemDb::open(&db) {
            if let Ok(store) = MemoryStore::try_open(memdb) {
                if store.skill_from_db(&name).is_some() {
                    let resolved = store.skill_read_bounded(&name, max_chars, &privacy_values)?;
                    let receipt = serde_json::json!({
                        "schemaVersion": resolved.schema_version,
                        "name": &resolved.name,
                        "source": "engine",
                        "storedBodyHash": &resolved.stored_body_hash,
                        "resolvedBodyHash": &resolved.resolved_body_hash,
                        "restoredPrivacyValues": resolved.restored_privacy_values,
                        "unresolvedPrivacyValues": resolved.unresolved_privacy_values,
                        "completeness": &resolved.completeness,
                    });
                    write_skill_body(&name, &resolved.body)?;
                    eprintln!("{receipt}");
                    emit_skill_resolved(
                        &ws,
                        &name,
                        &resolved.resolved_body_hash,
                        "engine",
                        resolved.body.len(),
                    );
                    return Ok(());
                }
            }
        }
    }
    Err(format!(
        "skill-read {name}: not on disk under {skills_dir:?} and not in the engine store \
         (run `membrane cli reindex` on the authoring machine to ingest skills)"
    ))
}

/// Product state root (C1): `Orthic Labs/Membrane/state` under the per-user data directory.
/// `RIGHTKIT_CONTEXT_STATE_ROOT` overrides it for tests and isolated runs.
pub fn state_root() -> PathBuf {
    crate::paths::state_root()
}

/// The one Cortex store path: the debug-only `CORTEX_DB` override (compiled out of release
/// builds), else the store under the state root (A6: the layout does not move). Push, the hook
/// and pull existence checks, the in-process readers and the resident all resolve it here.
pub fn cortex_db_path() -> PathBuf {
    crate::qa_env::cortex_db()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| state_root().join("tools/.cache/memory/cortex-engine.db"))
}

/// DB resolution: the `--db` flag, else the one Cortex store path.
fn resolve_db(flag: Option<String>) -> String {
    flag.unwrap_or_else(|| cortex_db_path().to_string_lossy().into_owned())
}

#[derive(Debug)]
pub(crate) struct DeployedRuntime {
    semantic_adjudicator_trust: PathBuf,
}

pub(crate) fn current_deployed_runtime() -> Option<DeployedRuntime> {
    Some(DeployedRuntime {
        semantic_adjudicator_trust: state_root()
            .join("tools/lib/memory/adapt-semantic-adjudicator-trust.json"),
    })
}

/// Take the owner lock for the length of one maintenance write. A serving resident holds it for
/// its whole life, so a one-shot writer gets a plain message instead of writing underneath it.
fn acquire_owner_lock() -> Result<crate::service::EngineOwner, String> {
    let state = state_root();
    std::fs::create_dir_all(&state)
        .map_err(|error| format!("create {}: {error}", state.display()))?;
    let runtime = crate::service::Runtime {
        workspace_root: state,
        db: cortex_db_path(),
        stable_current: None,
        version_root: None,
    };
    // Another one-shot writer holds the lock only for one write, so wait for it. A resident holds
    // it for its whole life: after the wait the write is refused with the plain message.
    match crate::service::EngineOwner::acquire_waiting(&runtime, std::time::Duration::from_secs(5))
    {
        Err(error) if error.starts_with("membrane_engine_already_owned") => Err(
            "membrane is serving: this write must go through the resident, or quit the host app first"
                .to_owned(),
        ),
        other => other,
    }
}

/// Store one piece of text as a durable memory (`membrane memory push`). This is the in-process
/// path: it takes the owner lock briefly and never loads the embedding model, so the row carries
/// a hash vector until the resident re-embeds it. The resident answers the same request through
/// `store.remember` with its own embedder.
pub fn push_memory(text: &str) -> Result<Value, String> {
    push_memory_request(&serde_json::json!({"content": text}))
}

/// The scope a memory write names, in the form it is stored under: validated as the other write
/// paths validate it, then a path becomes its slug and a slug gets its drive letter normalized.
pub fn canonical_write_scope(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    crate::scope::validate_write_scope(raw)?;
    Ok(scope_slug(raw))
}

/// A caller's memory name: 1 to 256 characters, no control characters. It becomes the id suffix.
pub fn validate_memory_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 256 || name.chars().any(char::is_control) {
        return Err("memory name must be 1 to 256 characters without control characters".into());
    }
    Ok(())
}

/// `memory push` in process, with the placement the CLI took: `{content, name?, scope?, tier?,
/// keywords?}`. The answer is `{id, scope, tier}`.
pub fn push_memory_request(args: &Value) -> Result<Value, String> {
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if content.is_empty() {
        return Err("refusing to push empty text".into());
    }
    if let Some(influence_class) = trust_scan_content(content) {
        return Err(format!("memory content quarantined: {influence_class}"));
    }
    let scope = match args.get("scope").and_then(Value::as_str) {
        Some(scope) => Some(canonical_write_scope(scope)?),
        None => None,
    };
    let tier = match args.get("tier").and_then(Value::as_str) {
        Some(tier) => Some(
            memory_tier_from_name(&tier.to_lowercase())
                .ok_or_else(|| format!("unknown tier `{tier}`"))?,
        ),
        None => None,
    };
    let name = args.get("name").and_then(Value::as_str).map(str::trim);
    if let Some(name) = name {
        validate_memory_name(name)?;
    }
    let keywords: Vec<String> = args
        .get("keywords")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let _owner = acquire_owner_lock()?;
    let db = resolve_db(None);
    if let Some(parent) = Path::new(&db).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let store = MemoryStore::try_open_with_embedder(
        MemDb::open(&db).map_err(|error| error.to_string())?,
        std::sync::Arc::new(rightkit_memory_core::HashEmbedder::new()),
    )?;
    let entry = store.remember_with(content, keywords, name, scope.as_deref(), tier)?;
    Ok(serde_json::json!({
        "id": entry.id,
        "scope": entry.scope_id,
        "tier": memory_tier_name(entry.tier),
        "keywords": entry.keywords,
    }))
}

/// Resident op `pull`: `{cwd?, prompt?, budget?, client?, session?}`.
pub fn op_pull(args: &Value) -> Result<Value, String> {
    crate::hook::pull_value(args)
}

/// The Blueprint verbs `membrane blueprint` forwards as given: the read verbs, plus `build` and
/// `refresh`, which publish a new generation for an enrolled repository (#552). Any other query
/// is searched as free text.
const BLUEPRINT_VERBS: &[&str] = &[
    "status",
    "search",
    "resolve",
    "recall",
    "expand",
    "impact",
    "path",
    "architecture",
    "documentTruth",
    "changes",
    "build",
    "refresh",
];

pub fn blueprint_args(query: &[String]) -> Vec<String> {
    match query.first() {
        Some(first) if BLUEPRINT_VERBS.contains(&first.as_str()) => query.to_vec(),
        _ => vec!["search".into(), query.join(" ")],
    }
}

/// Run a Blueprint query against `cwd` and return its JSON result. A read verb answers from the
/// last published generation; `build` and `refresh` publish a new one.
pub fn blueprint_query_value(query: &[String], cwd: Option<&Path>) -> Result<Value, String> {
    if query.is_empty() {
        return Err("blueprint requires a query".into());
    }
    let mut args = blueprint_args(query);
    if let Some(cwd) = cwd {
        args.push("--repo-root".into());
        args.push(cwd.to_string_lossy().into_owned());
    }
    let (result, stdout, _) = capture_output(|| crate::blueprint_one_shot::run_cli(&args));
    result?;
    serde_json::from_str(stdout.trim())
        .map_err(|error| format!("decode Blueprint response: {error}"))
}

/// True for the verbs that publish a new graph generation.
pub fn blueprint_verb_writes(query: &[String]) -> bool {
    matches!(
        query.first().map(String::as_str),
        Some("build") | Some("refresh")
    )
}

/// One-shot `build` or `refresh` with no resident running. Takes the owner lock for the whole
/// publish so there is one writer (INV-008); a serving resident refuses it. Read verbs never
/// come here.
pub fn blueprint_write_one_shot(query: &[String], cwd: Option<&Path>) -> Result<Value, String> {
    let _owner = acquire_owner_lock()?;
    blueprint_query_value(query, cwd)
}

/// Resident op `blueprint`: `{query: [..], cwd?}`.
pub fn op_blueprint(args: &Value) -> Result<Value, String> {
    let query = args
        .get("query")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let cwd = args.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    blueprint_query_value(&query, cwd.as_deref())
}

/// Most results one `memory search` returns.
pub const MEMORY_SEARCH_MAX_LIMIT: usize = 100;

pub(crate) fn memory_tier_from_name(name: &str) -> Option<rightkit_memory_core::MemoryTier> {
    match name {
        "working" => Some(rightkit_memory_core::MemoryTier::Working),
        "episodic" => Some(rightkit_memory_core::MemoryTier::Episodic),
        "semantic" => Some(rightkit_memory_core::MemoryTier::Semantic),
        _ => None,
    }
}

pub(crate) fn memory_tier_name(tier: rightkit_memory_core::MemoryTier) -> &'static str {
    match tier {
        rightkit_memory_core::MemoryTier::Working => "working",
        rightkit_memory_core::MemoryTier::Episodic => "episodic",
        rightkit_memory_core::MemoryTier::Semantic => "semantic",
    }
}

/// `memory_search` args `{query, tiers?: [working|episodic|semantic], scope?: string, limit?: n}`
/// answered from `store`. The resident and the in-process path both run this. It uses the lexical
/// arm, so it never needs the embedding model. Results come from a candidate pool, and the tier
/// filter keeps the matching ones from it. A memory that ranks below the pool is not returned.
pub fn op_memory_search(store: &MemoryStore, args: &Value) -> Result<Value, String> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if query.is_empty() {
        return Err("memory search requires a query".into());
    }
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(6);
    if limit == 0 || limit > MEMORY_SEARCH_MAX_LIMIT as u64 {
        return Err(format!(
            "memory search limit must be 1 to {MEMORY_SEARCH_MAX_LIMIT}"
        ));
    }
    let limit = limit as usize;
    let mut tier_names: Vec<&str> = Vec::new();
    let mut tiers: Vec<rightkit_memory_core::MemoryTier> = Vec::new();
    for name in args
        .get("tiers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let name = name.as_str().ok_or("tiers must be strings")?;
        let tier = memory_tier_from_name(name).ok_or_else(|| format!("unknown tier `{name}`"))?;
        tier_names.push(memory_tier_name(tier));
        tiers.push(tier);
    }
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty());
    // `scopes` is a caller-supplied chain, searched exactly as given (no ancestors added); it
    // wins over `scope`, which expands to its own ancestors and `global`.
    let chain: Vec<String> = args
        .get("scopes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(scope_slug)
                .collect()
        })
        .unwrap_or_default();
    let scopes = if chain.is_empty() {
        scope
            .map(|scope| crate::scope_chain(&normalize_scope(scope), &store.scopes()))
            .unwrap_or_default()
    } else {
        chain.clone()
    };
    let pool = (limit * 8).clamp(64, 1_000);
    let hits = store.recall_scored_with_arm_at(
        query,
        pool,
        &scopes,
        rightkit_memory_core::retriever::RetrievalArm::LexicalOnly,
        crate::time::now_millis() as i64,
        false,
    );
    let matched: Vec<(rightkit_memory_core::MemoryEntry, f32)> = hits
        .into_iter()
        .filter(|(entry, _)| tiers.is_empty() || tiers.contains(&entry.tier))
        .collect();
    let more = matched.len() > limit;
    let items: Vec<Value> = matched
        .iter()
        .take(limit)
        .map(|(entry, score)| {
            serde_json::json!({
                "id": entry.id,
                "tier": memory_tier_name(entry.tier),
                "scope": entry.scope_id,
                "score": score,
                "chars": entry.content.chars().count(),
                "content": entry.content,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "query": query,
        "tiers": tier_names,
        "scope": scope,
        "scopes": chain,
        "limit": limit,
        "pool": pool,
        "more": more,
        "items": items,
    }))
}

/// A scope as the store spells it: a path becomes its slug, anything else is drive-normalized.
pub fn scope_slug(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains(':') || raw.contains('\\') || raw.contains('/') {
        crate::scope::path_to_scope(raw)
    } else {
        normalize_scope(raw)
    }
}

/// `memory_delete` args `{id}` answered from `store`: `{id, deleted}`. `deleted` is false when no
/// memory had the id.
pub fn op_memory_delete(store: &MemoryStore, args: &Value) -> Result<Value, String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if id.is_empty() {
        return Err("memory delete requires an id".into());
    }
    let deleted = store.try_delete_observed(id, &cli_event_context())?;
    Ok(serde_json::json!({"id": id, "deleted": deleted}))
}

/// `memory search` in process, for when no resident answers. Opens the store read-only in effect
/// (the lexical owner) and never loads the model.
pub fn search_memory_in_process(db: &Path, args: &Value) -> Result<Value, String> {
    // A read never creates or migrates the store: open it read-only.
    let store = MemoryStore::try_open_lexical(
        MemDb::open_read_only(db).map_err(|error| error.to_string())?,
    )?;
    op_memory_search(&store, args)
}

/// `memory delete` in process, for when no resident answers. Takes the owner lock, so it is
/// refused while a resident holds the store.
pub fn delete_memory_in_process(db: &Path, args: &Value) -> Result<Value, String> {
    let _owner = acquire_owner_lock()?;
    let store = MemoryStore::try_open_with_embedder(
        MemDb::open(db).map_err(|error| error.to_string())?,
        std::sync::Arc::new(rightkit_memory_core::HashEmbedder::new()),
    )?;
    op_memory_delete(&store, args)
}

fn open_memory_lexical(db: &Path) -> Result<MemoryStore, String> {
    MemoryStore::try_open_lexical(MemDb::open(db).map_err(|error| error.to_string())?)
}

/// One `memories` row as the CLI prints it: `tier` keeps the stored spelling.
fn memory_row_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let tier: String = row.get(1)?;
    let keywords: Option<String> = row.get(3)?;
    Ok(serde_json::json!({
        "id": row.get::<_, String>(0)?,
        "tier": tier.trim_matches('"'),
        "content": row.get::<_, String>(2)?,
        "keywords": keywords
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "scope": row.get::<_, String>(4)?,
        "created_at": row.get::<_, Option<String>>(5)?,
        "access": row.get::<_, Option<i64>>(6)?.unwrap_or(0),
    }))
}

const MEMORY_ROW_SQL: &str =
    "SELECT id, tier, content, keywords, scope_id, created_at, access_count FROM memories";

/// One memory by id, with its full content (`memory get --json`). A read: unlike
/// `memory get` without `--json`, it records no use. `None` when no memory has the id.
pub fn get_memory_in_process(db: &Path, id: &str) -> Result<Option<Value>, String> {
    use rightkit_db::rusqlite::OptionalExtension as _;
    let store = open_memory_lexical(db)?;
    let conn = store.db().lock();
    conn.query_row(
        &format!("{MEMORY_ROW_SQL} WHERE id = ?1"),
        rusqlite::params![id],
        memory_row_json,
    )
    .optional()
    .map_err(|error| format!("read memory {id}: {error}"))
}

/// `memory list --json` over a scope chain: the memories of `scopes` in the order given (no
/// scopes: every memory), each with its content, at most `limit` in all. The answer is
/// `{scopes, limit, items, completeness}`; `completeness` says whether the cut dropped rows.
pub fn list_memories_in_process(
    db: &Path,
    scopes: &[String],
    limit: usize,
) -> Result<Value, String> {
    if limit == 0 {
        return Err("memory list limit must be greater than zero".into());
    }
    let store = open_memory_lexical(db)?;
    let slugs: Vec<String> = scopes.iter().map(|scope| scope_slug(scope)).collect();
    let targets: Vec<Option<&str>> = if slugs.is_empty() {
        vec![None]
    } else {
        slugs.iter().map(|scope| Some(scope.as_str())).collect()
    };
    let mut ids: Vec<String> = Vec::new();
    let mut truncated = false;
    for target in targets {
        let remaining = limit - ids.len();
        if remaining == 0 {
            // The limit is spent: one probe says whether a later scope held anything more.
            if !store.try_list_bounded(target, 1)?.items.is_empty() {
                truncated = true;
                break;
            }
            continue;
        }
        let page = store.try_list_bounded(target, remaining)?;
        if !page.completeness.is_exact() {
            truncated = true;
        }
        ids.extend(page.items.into_iter().map(|(id, ..)| id));
    }
    let mut items = Vec::with_capacity(ids.len());
    {
        use rightkit_db::rusqlite::OptionalExtension as _;
        let conn = store.db().lock();
        for id in &ids {
            let row = conn
                .query_row(
                    "SELECT id, tier, content, keywords, scope_id, created_at, access_count, \
                     inject_count FROM memories WHERE id = ?1",
                    rusqlite::params![id],
                    |row| {
                        let mut value = memory_row_json(row)?;
                        let inject = row.get::<_, Option<i64>>(7)?.unwrap_or(0);
                        let chars = value["content"].as_str().map_or(0, |c| c.chars().count());
                        value["chars"] = serde_json::json!(chars);
                        value["inject"] = serde_json::json!(inject);
                        Ok(value)
                    },
                )
                .optional()
                .map_err(|error| format!("read memory {id}: {error}"))?;
            if let Some(row) = row {
                items.push(row);
            }
        }
    }
    let completeness = if truncated {
        crate::store::CortexCompletenessV1::lower_bound(
            "ceiling_truncated",
            items.len().saturating_add(1),
            items.len(),
            1,
        )
    } else {
        crate::store::CortexCompletenessV1::exact(items.len(), items.len(), 0)
    };
    Ok(serde_json::json!({
        "scopes": slugs,
        "limit": limit,
        "items": items,
        "completeness": completeness,
    }))
}

/// `memory scopes --json`: every scope that holds a memory, with its count, in name order.
pub fn memory_scopes_in_process(db: &Path) -> Result<Value, String> {
    let store = open_memory_lexical(db)?;
    let conn = store.db().lock();
    let mut statement = conn
        .prepare("SELECT scope_id, COUNT(*) FROM memories GROUP BY scope_id ORDER BY scope_id")
        .map_err(|error| format!("list scopes: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(serde_json::json!({
                "scope": row.get::<_, String>(0)?,
                "count": row.get::<_, i64>(1)?,
            }))
        })
        .map_err(|error| format!("list scopes: {error}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("list scopes: {error}"))?;
    Ok(serde_json::json!({"scopes": rows}))
}

/// `memory info --json`: how much the store holds and how it was used, read from disk. `activity`
/// is the last `activity_limit` recalls. `embedModels` counts the rows per embedding model that
/// wrote them. The live embedder's dimension is the resident's to report, so it is not here.
pub fn memory_info_in_process(db: &Path, activity_limit: usize) -> Result<Value, String> {
    let store = open_memory_lexical(db)?;
    let (total, tiers, models, scopes) = {
        let conn = store.db().lock();
        let count = |sql: &str| -> Result<Vec<(String, i64)>, String> {
            let mut statement = conn
                .prepare(sql)
                .map_err(|error| format!("memory info: {error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get::<_, i64>(1)?,
                    ))
                })
                .map_err(|error| format!("memory info: {error}"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|error| format!("memory info: {error}"))?;
            Ok(rows)
        };
        let tiers = count("SELECT tier, COUNT(*) FROM memories GROUP BY tier ORDER BY tier")?;
        let models =
            count("SELECT embed_model, COUNT(*) FROM memories GROUP BY embed_model ORDER BY 1")?;
        let scopes = count("SELECT scope_id, COUNT(*) FROM memories GROUP BY scope_id")?;
        let total: i64 = tiers.iter().map(|(_, rows)| rows).sum();
        (total, tiers, models, scopes)
    };
    let pairs = |rows: Vec<(String, i64)>, key: &str| -> Vec<Value> {
        rows.into_iter()
            .map(|(name, count)| {
                let mut row = serde_json::Map::new();
                row.insert(
                    key.to_owned(),
                    Value::String(name.trim_matches('"').to_owned()),
                );
                row.insert("count".to_owned(), Value::from(count));
                Value::Object(row)
            })
            .collect()
    };
    Ok(serde_json::json!({
        "total": total,
        "scopeCount": scopes.len(),
        "tiers": pairs(tiers, "tier"),
        "embedModels": pairs(models, "model"),
        "activity": store.activity_json(activity_limit),
    }))
}

/// Run one Adapt or observation body in process, when no resident answers. A write opens the
/// store under the owner lock, so it is refused while a resident holds it; a read opens it
/// lexically. The answer is the route's own `(status, body)`.
fn answer_in_process(
    db: &Path,
    write: bool,
    route: fn(&MemoryStore, &str) -> (u16, String),
    body: &str,
) -> Result<(u16, String), String> {
    if write {
        let _owner = acquire_owner_lock()?;
        let store = MemoryStore::try_open_with_embedder(
            MemDb::open(db).map_err(|error| error.to_string())?,
            std::sync::Arc::new(rightkit_memory_core::HashEmbedder::new()),
        )?;
        return Ok(route(&store, body));
    }
    let store = MemoryStore::try_open_lexical(MemDb::open(db).map_err(|error| error.to_string())?)?;
    Ok(route(&store, body))
}

/// An Adapt command body (the operator route) answered in process.
pub fn adapt_operator_in_process(
    db: &Path,
    write: bool,
    body: &str,
) -> Result<(u16, String), String> {
    answer_in_process(db, write, crate::adapt_service::operator_response, body)
}

/// An observation body (`adapt observe`, `transcript append`) answered in process.
pub fn adapt_observation_in_process(
    db: &Path,
    write: bool,
    body: &str,
) -> Result<(u16, String), String> {
    answer_in_process(db, write, crate::serve::observation_body_response, body)
}

/// The transcript tip (`session_transcript_tip`) answered in process; a read.
pub fn transcript_tip_in_process(db: &Path, body: &str) -> Result<(u16, String), String> {
    answer_in_process(
        db,
        false,
        crate::adapt_observations::session_transcript::tip_response,
        body,
    )
}

/// What `resident.json` says about the resident, read without contacting it. A file whose `ts`
/// is more than 30 s old belongs to a resident that is gone.
fn resident_record() -> Value {
    let path = state_root().join("resident.json");
    let Some(record) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return serde_json::json!({"state": "absent"});
    };
    let ts = record.get("ts").and_then(Value::as_u64).unwrap_or(0);
    let ts_secs = if ts > 100_000_000_000 { ts / 1000 } else { ts };
    let age = (crate::time::now_millis() / 1000) as u64;
    let age = age.saturating_sub(ts_secs);
    serde_json::json!({
        "state": if age <= 30 { "unreachable" } else { "stale" },
        "pid": record.get("pid"),
        "endpoint": record.get("endpoint"),
        "generation": record.get("generation"),
        "started": record.get("started"),
        "ageSeconds": age,
    })
}

/// `membrane status` for the in-process path. The resident answers `status` with its own richer
/// object; this one only reports what can be read from disk.
pub fn status_value() -> Value {
    let db = cortex_db_path();
    let mut status = build_info();
    status["schema"] = "membrane.status.v1".into();
    status["mode"] = "in_process".into();
    status["stateRoot"] = state_root().to_string_lossy().into_owned().into();
    status["db"] = serde_json::json!({"path": db, "present": db.is_file()});
    status["resident"] = resident_record();
    status["model"] = "off (in-process path)".into();
    status
}

#[derive(Parser)]
#[command(name = "membrane", version, about = "Membrane durable-memory commands")]
struct Cli {
    #[arg(long, global = true)]
    db: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum InstallationCmd {
    /// Rotate a quarantined cloned installation to a fresh UUID while preserving lineage.
    Rotate {
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand)]
enum CheckpointCmd {
    /// Persist a typed checkpoint JSON payload in the local A0/session lane.
    Save {
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Load one unexpired checkpoint at a fixed Unix epoch millisecond instant.
    Load {
        id: String,
        #[arg(long)]
        as_of_ms: Option<i64>,
    },
    /// List currently resumable checkpoints within an exact scope.
    List {
        #[arg(long)]
        scope: String,
        #[arg(long)]
        as_of_ms: Option<i64>,
    },
    /// Retire a checkpoint without deleting its audit row.
    Done { id: String },
    /// Emit a normal-admission proposal; never writes durable knowledge directly.
    Promote {
        id: String,
        #[arg(long)]
        as_of_ms: Option<i64>,
    },
}

#[derive(Subcommand)]
enum HygieneCmd {
    /// Inventory configured SQLite storage without opening it for write or creating it.
    Storage,
    /// Audit memory, context, provenance, link, embedding, and feedback hygiene.
    Audit,
    /// Emit a reversible cleanup plan. This command never mutates storage.
    Clean {
        #[arg(long)]
        plan: bool,
    },
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AdaptCmd {
    /// Inspect live Adapt progress. Requires the active installed daemon.
    Status {
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// Inspect persisted report-only Insight issues without changing lifecycle.
    InspectIssues {
        #[arg(long, default_value = "workspace")]
        scope: String,
        #[arg(long, default_value_t = 16)]
        limit: usize,
    },
    /// Evaluate a bounded candidate comparison; never applies its winner.
    Compare {
        #[arg(long)]
        input: PathBuf,
    },
    /// Evaluate learned guard rollout eligibility; never grants host permission.
    GuardEligibility {
        #[arg(long)]
        input: PathBuf,
    },
    /// Execute one sealed model-proposal lifecycle transition. This never
    /// writes durable knowledge; Apply remains separate admission authority.
    ProposalPlan {
        #[arg(long)]
        store: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Normalize transcripts & mine deterministic Taste/Insights evidence.
    Mine {
        #[arg(required = true)]
        transcripts: Vec<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "workspace")]
        scope: String,
        #[arg(long, default_value_t = 2)]
        min_recurrence: u32,
    },
    /// Mine committed CodeRight session events from the absorbed stream —
    /// the native Lane-C source. Reads `absorbed_events` rows per session,
    /// normalizes `coderight_event_v1` payloads through the same pipeline as
    /// file transcripts, and emits the same response/taste_review shape.
    /// Requires the resident store; routes through the installed daemon.
    MineSession {
        #[arg(required = true)]
        session_ids: Vec<String>,
        #[arg(long, default_value = "workspace")]
        scope: String,
        #[arg(long, default_value_t = 2)]
        min_recurrence: u32,
    },
    /// Review mined Insight issues from a MineResponse JSON document.
    Review {
        #[arg(long)]
        input: PathBuf,
        #[arg(long = "issue")]
        issue_ids: Vec<String>,
    },
    /// Build an evidence-bound pending Taste manifest from native mine output.
    ReviewTaste {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        canonical_pool_sha256: Option<String>,
        #[arg(long)]
        created_at: String,
    },
    /// Bind complete independent semantic decisions to a pending Taste manifest.
    AdjudicateTaste {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        decisions: PathBuf,
        #[arg(long)]
        validated_at: String,
    },
    /// Validate & atomically admit accepted Taste records through Cortex.
    Apply {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Verify sealed Insight issues & atomically request Cortex admission.
    ApplyInsights {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        installation_id: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Recall active, applicable Taste records from Cortex.
    Recall {
        query: String,
        #[arg(short, default_value_t = 6)]
        k: usize,
        #[arg(long, default_value = "global")]
        scope: String,
        #[arg(long = "dimension", value_name = "KEY=VALUE")]
        dimensions: Vec<String>,
    },
    /// Aggregate mitigation outcomes from a ReportRequest JSON document.
    Report {
        #[arg(long)]
        input: PathBuf,
    },
    /// Score detectors from a BenchmarkRequest JSON document.
    Benchmark {
        #[arg(long)]
        input: PathBuf,
    },
    /// Validate issue/episode integrity from a DoctorRequest JSON document.
    Doctor {
        #[arg(long)]
        input: Option<PathBuf>,
    },
    /// Analyze supplied trusted-host billing observations; does not discover provider bills.
    ContextCost {
        #[arg(long)]
        input: PathBuf,
    },
}

#[derive(clap::Args)]
struct PutArgs {
    name: String,
    #[arg(long, default_value = "proposed")]
    scope: String,
    /// Working | Episodic | Semantic
    #[arg(long, default_value = "Working")]
    tier: String,
    #[arg(long)]
    file: Option<String>,
    #[arg(long, default_value = "memory")]
    artifact_family: String,
    #[arg(long, default_value = "manual")]
    producer: String,
    #[arg(long, default_value = "memory")]
    record_type: String,
    #[arg(long, default_value = "A0")]
    authority: String,
    #[arg(long, default_value = "data_only")]
    influence_class: String,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    trace: Option<String>,
    #[arg(long)]
    effective_from_ms: Option<i64>,
    #[arg(long)]
    effective_until_ms: Option<i64>,
    #[arg(long)]
    expires_at_ms: Option<i64>,
    #[arg(long)]
    review_after_ms: Option<i64>,
    #[arg(long)]
    priority_class: Option<String>,
    #[arg(long)]
    confidence: Option<f64>,
    #[arg(long)]
    confidence_basis: Option<String>,
    #[arg(long)]
    supersedes: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inspect or recover this installation's opaque machine identity.
    Installation {
        #[command(subcommand)]
        command: InstallationCmd,
    },
    /// Machine-local checkpoint verbs; no checkpoint enters ordinary semantic recall.
    Checkpoint {
        #[command(subcommand)]
        command: CheckpointCmd,
    },
    /// Inspect hygiene or emit a non-mutating cleanup plan.
    Hygiene {
        #[command(subcommand)]
        command: HygieneCmd,
    },
    /// Explain one memory's content-free lifecycle, provenance, and use metadata.
    Explain { id: String },
    /// Recall against the store.
    Recall {
        query: String,
        #[arg(short, default_value_t = 6)]
        k: usize,
        #[arg(long)]
        scope: Option<String>,
        /// Fixed Unix epoch milliseconds for replay/audit admission.
        #[arg(long)]
        as_of_ms: Option<i64>,
        /// Include expired rows for audit only; superseded and non-active rows remain excluded.
        #[arg(long)]
        include_expired: bool,
        #[arg(long, value_enum, default_value = "hybrid")]
        arm: RetrievalArmArg,
    },
    /// Evaluation-only batch recall. Reads JSONL queries and emits ranked IDs without logging.
    Replay {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(short, default_value_t = 20)]
        k: usize,
    },
    /// Run one dream consolidation pass now (L8 curation).
    Curate {
        /// Override the date (YYYY-MM-DD). Defaults to today (UTC).
        #[arg(long)]
        today: Option<String>,
    },
    /// List memories held outside active recall by reversible curation.
    QuarantineList,
    /// Restore one quarantined memory into active recall.
    QuarantineRestore { id: String },
    /// Fetch a memory's FULL content by id and record the fetch as a use — the effectiveness
    /// signal behind the injected previews (`full: cortex get <id>`).
    Get { id: String },
    /// Record per-candidate recall feedback (the feedback rail). `--outcome`
    /// used|ignored|contradicted; `--source` observed_action|cited_verdict|advisory (default
    /// advisory — only verified sources rank; a cited_verdict needs `--verdict-ref`).
    Feedback {
        #[arg(long)]
        trace: String,
        #[arg(long)]
        candidate: String,
        #[arg(long)]
        sha: String,
        #[arg(long)]
        outcome: String,
        #[arg(long, default_value = "advisory")]
        source: String,
        #[arg(long)]
        verdict_ref: Option<String>,
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// Close unmatched production deliveries as provisional unknown after one bounded observation window.
    CloseUnknown {
        #[arg(long)]
        observed_since: String,
        #[arg(long)]
        observed_through: String,
        #[arg(long)]
        max_deliveries: usize,
    },
    /// Preregister an immutable causal-learning experiment from JSON.
    LearningRegister {
        #[arg(long)]
        input: PathBuf,
    },
    /// Evaluate trusted controlled evidence and apply only a qualified update.
    LearningQualify { experiment: String },
    /// Read one immutable learning-promotion receipt.
    LearningReceipt { experiment: String },
    /// Ingest git-tracked skill bodies into the engine `skills` table WITHOUT the full re-embed
    /// that `reindex` does. Cheap (no embedding) — run after `git pull` (daily-sync) so the other
    /// machine's engine store stays current with authored skills.
    IngestSkills {
        /// Override the workspace root (else derived from the deployed binary location).
        #[arg(long)]
        root: Option<String>,
    },
    /// Resolve a WORKSPACE skill body by name (the `resolver` handle the skills provider emits).
    /// Reads `<workspace>/tools/skills/<name>/SKILL.md` regardless of the current repo — this is
    /// what lets a session in any repo LOAD a workspace skill it discovered, without a filesystem
    /// symlink. Traversal-guarded; prints the body to stdout and its bodyHash to stderr.
    SkillRead {
        name: String,
        /// Override the workspace root (else derived from the deployed binary location).
        #[arg(long)]
        root: Option<String>,
        /// Hard character ceiling for returned body text.
        #[arg(long, default_value_t = 65_536)]
        max_chars: usize,
        /// JSON object of caller-held privacy values keyed by placeholder name.
        #[arg(long)]
        privacy_values: Option<PathBuf>,
    },
    /// DB-first write path: upsert a memory through the engine (embeds + persists immediately).
    /// Content from --file, or stdin when omitted.
    Put(Box<PutArgs>),
    /// Delete a memory by id (row + in-RAM entry).
    Delete { id: String },
    /// Browse the store: id, tier, chars, access/inject counts.
    List {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value_t = 1_000)]
        limit: usize,
    },
    /// List active rows due for review (read-only; never mutates authority). Surfaces all
    /// CTX-010 triggers: an elapsed time clock (`expired`/`review_after_elapsed`), an in-place
    /// content edit outside governed supersession (`version_changed`), and a verified
    /// `contradicted` recall outcome against a still-active row (`outcome_contradicted`).
    ReviewDue {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Drain installation-local background-review proposals into the governed proposal queue (proposal-first; never admits).
    DrainBackgroundProposals {
        repository: String,
        path: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Emit the embedding-neighbor graph; --anonymous removes memory IDs and scopes.
    Graph {
        #[arg(long)]
        anonymous: bool,
        #[arg(long, default_value_t = 3)]
        neighbors: usize,
        #[arg(long, default_value_t = 0.55)]
        min_cosine: f32,
    },
    /// Re-embed EVERY row from DB content (embedder swaps / hash-vector repair). No files needed.
    Reindex,
    /// Permanently erase one memory by id (governed hard erase; terminal, not suppression).
    Erase { id: String },
    /// Write a digest-sealed Cortex backup envelope to FILE.
    Backup { output: PathBuf },
    /// Restore a Cortex backup envelope from FILE (transactional; refuses tampered envelopes).
    Restore { input: PathBuf },
    /// Export the whole store as a canonical-scoped markdown tree (audit + sync medium).
    /// The DB stays authoritative; these files are generated FROM it.
    ExportMd { dir: String },
    /// Export the whole store to `cortex-export/` unless another directory is supplied.
    Export {
        #[arg(default_value = "cortex-export")]
        dir: PathBuf,
    },
    /// Export a deterministic review queue; content is omitted unless explicitly requested.
    VaultExport {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "json")]
        format: String,
        #[arg(long)]
        include_content: bool,
    },
    /// Import a canonical Cortex Markdown export from `cortex-export/` by default.
    Import {
        #[arg(default_value = "cortex-export")]
        dir: PathBuf,
    },
}

fn open(db: &str) -> Result<MemoryStore, String> {
    if let Some(store) = crate::service::resident_store_for_db(Path::new(db)) {
        return Ok(store);
    }
    MemoryStore::try_open(MemDb::open(db).map_err(|e| e.to_string())?)
}

fn query_ids(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<String>, String> {
    let mut statement = conn.prepare(sql).map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(|error| error.to_string())
}

fn storage_file_record(
    path: &Path,
    kind: &str,
    classification: Option<&str>,
    reason: &str,
) -> serde_json::Value {
    if let Some(error) = storage_path_safety(path) {
        return serde_json::json!({
            "kind": kind,
            "path": path,
            "exists": null,
            "status": "unavailable",
            "classification": null,
            "reason": "path safety could not be established",
            "confidence": "high",
            "error": error,
        });
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                serde_json::json!({"device":metadata.dev(),"inode":metadata.ino()})
            };
            #[cfg(not(unix))]
            let identity = serde_json::Value::Null;
            serde_json::json!({"kind":kind,"path":path,"exists":true,"bytes":metadata.len(),"modified_unix_ms":modified_ms,"file_identity":identity,"classification":classification,"reason":reason,"confidence":"low"})
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            serde_json::json!({"kind":kind,"path":path,"exists":true,"status":"unavailable","classification":null,"reason":"symlink inspection is refused","confidence":"high","error":{"code":"symlink_refused"}})
        }
        Ok(_) => {
            serde_json::json!({"kind":kind,"path":path,"exists":true,"status":"unavailable","classification":null,"reason":"path exists but is not a regular file","confidence":"high","error":{"code":"non_regular_path"}})
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            serde_json::json!({"kind":kind,"path":path,"exists":false,"classification":"stale","reason":"file is absent","confidence":"high"})
        }
        Err(error) => {
            serde_json::json!({"kind":kind,"path":path,"exists":null,"classification":null,"reason":"metadata unavailable","confidence":"low","error":{"code":"metadata_unavailable","message":error.to_string()}})
        }
    }
}

fn storage_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn storage_safe_regular_file(path: &Path) -> bool {
    storage_path_safety(path).is_none() && storage_regular_file(path)
}

fn storage_path_safety(path: &Path) -> Option<serde_json::Value> {
    for component in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let Ok(metadata) = std::fs::symlink_metadata(component) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Some(
                serde_json::json!({"status":"unavailable","code":"symlink_component_refused","path":component}),
            );
        }
        #[cfg(windows)]
        let is_reparse_point = {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        };
        #[cfg(windows)]
        if is_reparse_point {
            return Some(
                serde_json::json!({"status":"unavailable","code":"reparse_component_refused","path":component}),
            );
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            Some(serde_json::json!({"status":"unavailable","code":"non_regular_path","path":path}))
        }
        _ => None,
    }
}

fn sqlite_readonly_uri(path: &Path) -> String {
    // SQLite URI paths use percent escapes for URI delimiters. Existing files are required before
    // this is used, so no inspection path can create a database.
    let raw = path.to_string_lossy().replace('\\', "/");
    let encoded = raw
        .replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
        .replace(' ', "%20");
    let prefix = if encoded.as_bytes().get(1) == Some(&b':') {
        "file:/"
    } else {
        "file:"
    };
    format!("{prefix}{encoded}?mode=ro")
}

fn storage_key_digest(
    conn: &rusqlite::Connection,
    table: &str,
    key_columns: &[&str],
) -> Result<serde_json::Value, String> {
    let count: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get(0)
        })
        .map_err(|e| e.to_string())?;
    let quoted = key_columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    let mut statement = conn
        .prepare(&format!(
            "SELECT {quoted} FROM \"{table}\" ORDER BY {quoted}"
        ))
        .map_err(|e| e.to_string())?;
    let mut digest = Sha256Hasher::new();
    let mut rows = statement.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        for index in 0..key_columns.len() {
            use rightkit_db::rusqlite::types::ValueRef;
            match row.get_ref(index).map_err(|e| e.to_string())? {
                ValueRef::Null => digest.update(&[0]),
                ValueRef::Integer(value) => {
                    digest.update(&[1]);
                    digest.update(&value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    digest.update(&[2]);
                    digest.update(&value.to_bits().to_le_bytes());
                }
                ValueRef::Text(value) => {
                    digest.update(&[3]);
                    digest.update(&(value.len() as u64).to_le_bytes());
                    digest.update(value);
                }
                ValueRef::Blob(value) => {
                    digest.update(&[4]);
                    digest.update(&(value.len() as u64).to_le_bytes());
                    digest.update(value);
                }
            }
        }
        digest.update(&[255]);
    }
    Ok(
        serde_json::json!({"table":table,"row_count":count,"key_set_sha256":format!("sha256:{}", digest.finalize())}),
    )
}

fn storage_hygiene_report(db_path: &str) -> Result<serde_json::Value, String> {
    let main = PathBuf::from(db_path);
    if !main.is_absolute() {
        return Err(
            serde_json::json!({"status":"error","code":"absolute_path_required"}).to_string(),
        );
    }
    let wal = PathBuf::from(format!("{}-wal", main.display()));
    let shm = PathBuf::from(format!("{}-shm", main.display()));
    let main_exists = storage_safe_regular_file(&main);
    let wal_exists = storage_safe_regular_file(&wal);
    let shm_exists = storage_safe_regular_file(&shm);
    let process_paths = [&main, &wal, &shm]
        .into_iter()
        .filter(|path| storage_safe_regular_file(path))
        .cloned()
        .collect::<Vec<_>>();
    let process_evidence =
        storage_process_open_handle_evidence(&process_paths, None, Duration::from_secs(2));
    let wal_is_open = storage_process_observes_path(&process_evidence, &wal);
    let shm_is_open = storage_process_observes_path(&process_evidence, &shm);
    let mut files = vec![storage_file_record(
        &main,
        "main",
        if main_exists {
            Some("active")
        } else {
            Some("stale")
        },
        if main_exists {
            "configured CLI database binding identifies this main file; process ownership is not inferred"
        } else {
            "configured main database is absent"
        },
    )];
    files.push(storage_file_record(
        &wal,
        "wal",
        if !main_exists && wal_exists {
            Some("orphan_candidate")
        } else if wal_exists && wal_is_open {
            Some("active")
        } else if wal_exists {
            Some("recoverable")
        } else {
            Some("stale")
        },
        if !main_exists && wal_exists {
            "WAL exists without matching main database"
        } else if wal_exists && wal_is_open {
            "WAL has a directly observed open handle; process ownership is not inferred"
        } else if wal_exists {
            "WAL may contain recoverable committed frames; liveness is not inferred"
        } else {
            "WAL is absent"
        },
    ));
    files.push(storage_file_record(
        &shm,
        "shm",
        if !main_exists && shm_exists {
            Some("orphan_candidate")
        } else if shm_exists && shm_is_open {
            Some("active")
        } else {
            Some("stale")
        },
        if !main_exists && shm_exists {
            "SHM exists without matching main database"
        } else if shm_exists && shm_is_open {
            "SHM has a directly observed open handle; process ownership is not inferred"
        } else if shm_exists {
            "shared-memory sidecar exists but liveness is unproven"
        } else {
            "SHM is absent"
        },
    ));
    let mut probe = serde_json::json!({"status":"unavailable","code":"main_database_absent"});
    if let Some(error) = storage_path_safety(&main) {
        probe = error;
    } else if main_exists {
        probe = match rightkit_db::open_read_only(sqlite_readonly_uri(&main)) {
            Ok(conn) => {
                let snapshot_error = conn.execute_batch("BEGIN").err();
                let user_version = rightkit_db::user_version(&conn);
                let schema_version: Result<i64, _> =
                    conn.query_row("PRAGMA schema_version", [], |row| row.get(0));
                let journal_mode = rightkit_db::journal_mode(&conn);
                let integrity: Result<String, _> =
                    conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
                let mut probe_errors = Vec::new();
                if let Some(error) = snapshot_error {
                    probe_errors.push(serde_json::json!({"field":"snapshot","code":"sqlite_snapshot_failed","message":error.to_string()}));
                }
                if let Err(error) = &user_version {
                    probe_errors.push(serde_json::json!({"field":"user_version","code":"sqlite_probe_failed","message":error.to_string()}));
                }
                if let Err(error) = &schema_version {
                    probe_errors.push(serde_json::json!({"field":"schema_version","code":"sqlite_probe_failed","message":error.to_string()}));
                }
                if let Err(error) = &journal_mode {
                    probe_errors.push(serde_json::json!({"field":"journal_mode","code":"sqlite_probe_failed","message":error.to_string()}));
                }
                if let Err(error) = &integrity {
                    probe_errors.push(serde_json::json!({"field":"integrity_check","code":"sqlite_probe_failed","message":error.to_string()}));
                }
                let mut tables = Vec::new();
                for (table, key) in [
                    ("memories", &["id"][..]),
                    ("links", &["src_id", "dst_slug"][..]),
                    ("context_feedback", &["trace_id", "candidate_id"][..]),
                    ("memory_event_log", &["event_id"][..]),
                ] {
                    match table_exists(&conn, table) {
                        Ok(true) => match storage_key_digest(&conn, table, key) { Ok(value) => tables.push(value), Err(error) => tables.push(serde_json::json!({"table":table,"status":"unavailable","error":{"code":"table_probe_failed","message":error}})) },
                        Ok(false) => tables.push(serde_json::json!({"table":table,"status":"unavailable","code":"table_absent"})),
                        Err(error) => tables.push(serde_json::json!({"table":table,"status":"unavailable","error":{"code":"table_lookup_failed","message":error}})),
                    }
                }
                if let Err(error) = conn.execute_batch("COMMIT") {
                    probe_errors.push(serde_json::json!({"field":"snapshot","code":"sqlite_snapshot_close_failed","message":error.to_string()}));
                }
                serde_json::json!({"status":if probe_errors.is_empty() { "ok" } else { "error" },"snapshot":if probe_errors.iter().any(|error| error["field"] == "snapshot") { serde_json::Value::Null } else { serde_json::json!("read_transaction") },"user_version":user_version.ok(),"schema_version":schema_version.ok(),"journal_mode":journal_mode.ok(),"integrity_check":integrity.ok(),"known_tables":tables,"errors":probe_errors})
            }
            Err(error) => {
                serde_json::json!({"status":"error","error":{"code":"sqlite_readonly_open_failed","message":error.to_string()}})
            }
        };
    }
    Ok(
        serde_json::json!({"schema":"cortex.hygiene-storage.v1","configured_path":main,"configured_binding_evidence":"explicit --db or deployed runtime binding","classification_enum":["active","recoverable","stale","orphan_candidate","quarantined"],"classification_availability":{"quarantined":{"status":"unavailable","code":"quarantine_receipt_unavailable"}},"read_only":true,"files":files,"sqlite":probe,"process_open_handle_evidence":process_evidence}),
    )
}

fn storage_process_observes_path(evidence: &serde_json::Value, path: &Path) -> bool {
    let expected = path.to_string_lossy();
    evidence["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|observation| observation["path"].as_str() == Some(expected.as_ref()))
}

#[cfg(unix)]
fn parse_lsof_observations(raw: &str) -> Vec<serde_json::Value> {
    let mut pid = None;
    let mut command = None;
    let mut fd = None;
    let mut observations = Vec::new();
    for record in raw.split('\n').filter(|record| !record.is_empty()) {
        let (tag, value) = record.split_at(1);
        match tag {
            "p" => pid = value.parse::<u32>().ok(),
            "c" => command = Some(value.to_owned()),
            "f" => fd = Some(value.to_owned()),
            "n" => {
                if let (Some(pid), Some(command), Some(fd)) = (pid, command.as_ref(), fd.as_ref()) {
                    observations.push(
                        serde_json::json!({"pid":pid,"command":command,"fd":fd,"path":value}),
                    );
                }
            }
            _ => {}
        }
    }
    observations.sort_by_key(|value| value.to_string());
    observations.dedup_by(|left, right| left == right);
    observations
}

fn storage_process_open_handle_evidence(
    paths: &[PathBuf],
    fixture: Option<&str>,
    timeout: Duration,
) -> serde_json::Value {
    if let Some(fixture) = fixture {
        return serde_json::from_str(fixture).unwrap_or_else(|error| serde_json::json!({"status":"error","code":"process_probe_fixture_invalid","message":error.to_string(),"ownership_inferred":false}));
    }
    if paths.is_empty() {
        return serde_json::json!({"status":"unavailable","code":"no_existing_safe_paths","ownership_inferred":false});
    }
    // `lsof` is optional & its result establishes only an observed open handle, never owner
    // authority. It is intentionally not required for a storage inspection to succeed.
    #[cfg(not(unix))]
    {
        let _ = (paths, timeout);
        serde_json::json!({"status":"unavailable","code":"process_probe_unsupported_platform","ownership_inferred":false})
    }
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        let args: Vec<&OsStr> = ["-Fpcn", "--"]
            .into_iter()
            .map(OsStr::new)
            .chain(paths.iter().map(|p| p.as_os_str()))
            .collect();
        let options = rightkit_child_process::RunOptions::new(timeout)
            .stdout_cap(16 * 1024 * 1024)
            .stderr_cap(16 * 1024 * 1024);
        // A missed deadline is reported as `Ok(None)`, the same as before the move to RightKit.
        let outcome = rightkit_child_process::run("lsof", args, &options)
            .map(|output| (!output.timed_out).then_some(output));
        match outcome {
            Err(error) => {
                serde_json::json!({"status":"unavailable","code":"process_probe_unavailable","message":error.to_string(),"ownership_inferred":false})
            }
            Ok(None) => {
                serde_json::json!({"status":"unavailable","code":"process_probe_timeout","timeout_ms":timeout.as_millis(),"ownership_inferred":false})
            }
            Ok(Some(output)) if output.status.success() => {
                serde_json::json!({"status":"observed","code":"lsof","observations":parse_lsof_observations(&String::from_utf8_lossy(&output.stdout.bytes)),"ownership_inferred":false})
            }
            Ok(Some(output)) => {
                serde_json::json!({"status":"unavailable","code":"lsof_no_open_handle_or_unavailable","exit_code":output.status.code(),"ownership_inferred":false})
            }
        }
    }
}

fn hygiene_report(db_path: &str) -> Result<serde_json::Value, String> {
    let db = MemDb::open(db_path).map_err(|error| error.to_string())?;
    let conn = db.lock();
    let duplicate_groups = query_ids(
        &conn,
        "SELECT group_concat(id, ',') FROM memories GROUP BY lower(trim(content)) HAVING COUNT(*) > 1 ORDER BY MIN(id)",
    )?;
    let invalid_scopes = query_ids(
        &conn,
        "SELECT id FROM memories WHERE trim(scope_id)='' ORDER BY id",
    )?;
    let missing_provenance = query_ids(
        &conn,
        "SELECT id FROM memories WHERE COALESCE(source_ids,'[]')='[]' AND COALESCE(producer,'manual') IN ('','manual') ORDER BY id",
    )?;
    let stale_embeddings = query_ids(
        &conn,
        "SELECT id FROM memories WHERE embedding_q IS NULL OR length(embedding_q)=0 OR COALESCE(embed_model,'')='' ORDER BY id",
    )?;
    let orphaned_links = query_ids(
        &conn,
        "SELECT src_id || '->' || dst_slug FROM links WHERE src_id NOT IN (SELECT id FROM memories) ORDER BY src_id,dst_slug",
    )?;
    let unexercised_feedback = query_ids(
        &conn,
        "SELECT trace_id || ':' || candidate_id FROM context_feedback WHERE verified=0 ORDER BY trace_id,candidate_id",
    )?;
    let global_core_chars: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(length(content)),0) FROM memories WHERE scope_id='global' AND lifecycle_state='active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    drop(conn);

    let mut expired_working_context = Vec::new();
    let mut unresolved_contradictions = Vec::new();
    if db.event_db_path().is_some() {
        // CTX-001: the event ledger is opened, migrated and WAL-configured by
        // `MemDb::open`; the audit reuses that one authority's handle instead
        // of opening a second ad-hoc connection that would bypass the
        // pragmas and the migration ladder.
        let events = db.lock_events();
        if table_exists(&events, "membrane_working_context")? {
            expired_working_context = query_ids(
                &events,
                "SELECT context_id FROM membrane_working_context WHERE state='active' AND expires_at<=strftime('%Y-%m-%dT%H:%M:%fZ','now') ORDER BY context_id",
            )?;
        }
        if table_exists(&events, "membrane_temporal_fact")? {
            unresolved_contradictions = query_ids(
                &events,
                "SELECT scope_id || ':' || subject || ':' || predicate FROM membrane_temporal_fact WHERE valid_until IS NULL AND veracity='supported' GROUP BY scope_id,subject,predicate HAVING COUNT(*)>1 ORDER BY scope_id,subject,predicate",
            )?;
        }
    }

    let findings = serde_json::json!([
        {"code":"duplicate_records","count":duplicate_groups.len(),"targets":duplicate_groups},
        {"code":"expired_working_context","count":expired_working_context.len(),"targets":expired_working_context},
        {"code":"unresolved_contradictions","count":unresolved_contradictions.len(),"targets":unresolved_contradictions},
        {"code":"orphaned_links","count":orphaned_links.len(),"targets":orphaned_links},
        {"code":"invalid_scopes","count":invalid_scopes.len(),"targets":invalid_scopes},
        {"code":"stale_embeddings","count":stale_embeddings.len(),"targets":stale_embeddings},
        {"code":"unexercised_feedback","count":unexercised_feedback.len(),"targets":unexercised_feedback},
        {"code":"missing_provenance","count":missing_provenance.len(),"targets":missing_provenance},
        {"code":"oversized_global_core","count":i64::from(global_core_chars > 262_144),"chars":global_core_chars,"limit_chars":262144}
    ]);
    let issue_count: usize = findings
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|finding| finding.get("count").and_then(serde_json::Value::as_u64))
        .map(|count| count as usize)
        .sum();
    Ok(serde_json::json!({
        "schema":"membrane.hygiene-audit.v1",
        "status": if issue_count == 0 { "clean" } else { "issues" },
        "issue_count": issue_count,
        "findings": findings
    }))
}

pub fn explain_memory(db_path: &str, id: &str) -> Result<serde_json::Value, String> {
    let db = MemDb::open(db_path).map_err(|error| error.to_string())?;
    let conn = db.lock();
    conn.query_row(
        "SELECT id,tier,scope_id,score,created_at,updated_at,access_count,inject_count,content_hash,embed_model,source_ids,artifact_family,producer,record_type,authority,influence_class,lifecycle_state,effective_from_ms,effective_until_ms,expires_at_ms,review_after_ms,superseded_by,priority_class,confidence,confidence_basis FROM memories WHERE id=?1",
        [id],
        |row| Ok(serde_json::json!({
            "schema":"membrane.memory-explain.v1",
            "completeness": crate::store::CortexCompletenessV1::exact(1, 1, 0),
            "id":row.get::<_,String>(0)?, "tier":row.get::<_,String>(1)?, "scope_id":row.get::<_,String>(2)?, "score":row.get::<_,f64>(3)?,
            "created_at":row.get::<_,String>(4)?, "updated_at":row.get::<_,String>(5)?, "access_count":row.get::<_,i64>(6)?, "inject_count":row.get::<_,i64>(7)?,
            "content_hash":row.get::<_,Option<String>>(8)?, "embed_model":row.get::<_,Option<String>>(9)?, "source_ids":row.get::<_,String>(10)?,
            "artifact_family":row.get::<_,String>(11)?, "producer":row.get::<_,String>(12)?, "record_type":row.get::<_,String>(13)?, "authority":row.get::<_,String>(14)?, "influence_class":row.get::<_,String>(15)?,
            "lifecycle":{"state":row.get::<_,String>(16)?,"effective_from_ms":row.get::<_,Option<i64>>(17)?,"effective_until_ms":row.get::<_,Option<i64>>(18)?,"expires_at_ms":row.get::<_,Option<i64>>(19)?,"review_after_ms":row.get::<_,Option<i64>>(20)?,"superseded_by":row.get::<_,Option<String>>(21)?,"priority_class":row.get::<_,String>(22)?},
            "confidence":row.get::<_,Option<f64>>(23)?, "confidence_basis":row.get::<_,Option<String>>(24)?
        })),
    )
    .map_err(|error| if matches!(error, rusqlite::Error::QueryReturnedNoRows) { format!("memory not found: {id}") } else { error.to_string() })
}

fn parse_exported_memory(path: &Path) -> Result<(String, String, String), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read import {}: {error}", path.display()))?;
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| format!("import missing frontmatter: {}", path.display()))?;
    let (frontmatter, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| format!("import has unclosed frontmatter: {}", path.display()))?;
    let mut id = None;
    let mut scope = None;
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim() {
            "id" => id = Some(value.trim().to_string()),
            "scope" => scope = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let id = id.ok_or_else(|| format!("import missing id: {}", path.display()))?;
    let scope = scope.ok_or_else(|| format!("import missing scope: {}", path.display()))?;
    let expected_prefix = format!("{scope}/");
    if scope.trim().is_empty()
        || !id.starts_with(&expected_prefix)
        || id[expected_prefix.len()..].contains('/')
    {
        return Err(format!(
            "import identity/scope mismatch: {}",
            path.display()
        ));
    }
    let content = body.trim().to_string();
    if content.is_empty() {
        return Err(format!("import has empty content: {}", path.display()));
    }
    Ok((id, scope, content))
}

fn import_markdown_tree(store: &MemoryStore, dir: &Path) -> Result<usize, String> {
    let root = dir
        .canonicalize()
        .map_err(|error| format!("resolve import {}: {error}", dir.display()))?;
    if !root.is_dir() {
        return Err(format!("import path is not a directory: {}", dir.display()));
    }
    let mut files = Vec::new();
    for scope_entry in std::fs::read_dir(&root).map_err(|error| error.to_string())? {
        let scope_entry = scope_entry.map_err(|error| error.to_string())?;
        if scope_entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_symlink()
            || !scope_entry.path().is_dir()
        {
            continue;
        }
        for entry in std::fs::read_dir(scope_entry.path()).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let file_type = entry.file_type().map_err(|error| error.to_string())?;
            if !file_type.is_file()
                || file_type.is_symlink()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("md")
            {
                continue;
            }
            let canonical = entry
                .path()
                .canonicalize()
                .map_err(|error| error.to_string())?;
            if !canonical.starts_with(&root) {
                return Err(format!(
                    "import path escapes root: {}",
                    entry.path().display()
                ));
            }
            files.push(canonical);
        }
    }
    files.sort();
    let mut imported = 0;
    for path in files {
        let (id, scope, content) = parse_exported_memory(&path)?;
        let name = id
            .rsplit('/')
            .next()
            .ok_or_else(|| "import id invalid".to_string())?;
        store.try_put(
            name,
            &content,
            &scope,
            rightkit_memory_core::MemoryTier::Semantic,
        )?;
        imported += 1;
    }
    Ok(imported)
}

fn cli_event_context() -> crate::store::MemoryEventContext {
    crate::store::MemoryEventContext::from_environment("cli")
}

fn put_event_context(
    session: Option<&str>,
    trace: Option<&str>,
) -> crate::store::MemoryEventContext {
    let mut context = cli_event_context();
    if let Some(session) = session {
        context = context.with_session(session);
    }
    if let Some(trace) = trace {
        context = context.with_trace(trace).with_turn(trace);
    }
    context
}

fn trust_scan_content(content: &str) -> Option<&'static str> {
    let lower = content.to_ascii_lowercase();
    for phrase in [
        "ignore previous instructions",
        "ignore all previous instructions",
        "system message:",
        "developer override",
        "reveal the prompt",
    ] {
        if lower.contains(phrase) {
            return Some("untrusted_instruction");
        }
    }
    for key in [
        "api_key",
        "apikey",
        "secret",
        "password",
        "access_token",
        "refresh_token",
    ] {
        for line in lower.lines() {
            if let Some(pos) = line.find(key) {
                let tail = &line[pos + key.len()..];
                let tail = tail.trim_start();
                if tail
                    .strip_prefix('=')
                    .or_else(|| tail.strip_prefix(':'))
                    .map(secret_value_looks_credible)
                    .unwrap_or(false)
                {
                    return Some("secret_like");
                }
            }
        }
    }
    None
}

fn put_digest(body: &str) -> String {
    sha256_bytes(body.as_bytes())
}

fn quarantine_untrusted_put(db: &str, memory_id: &str, scope: &str, content: &str, reason: &str) {
    let directory = Path::new(db)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("trust-quarantine");
    let digest = put_digest(content);
    let path = directory.join(format!("{digest}.json"));
    let record = serde_json::json!({
        "schema_version": 1,
        "status": "quarantine",
        "authority": "A0",
        "influence_class": reason,
        "memory_id_sha256": put_digest(memory_id),
        "scope_sha256": put_digest(scope),
        "content_sha256": digest,
    });
    if std::fs::create_dir_all(&directory).is_ok() {
        let _ = std::fs::write(path, format!("{}\n", record));
    }
}

#[allow(clippy::too_many_arguments)]
fn record_cli_external(
    store: &MemoryStore,
    context: &crate::store::MemoryEventContext,
    operation: &str,
    stage: crate::store::ExternalLifecycleStage,
    status: &str,
    reason_code: &str,
    memory_id: Option<&str>,
    scope_id: Option<&str>,
    artifact_family: &str,
    producer: &str,
    quantity: usize,
) -> Result<(), String> {
    store.record_external_lifecycle(
        context,
        operation,
        stage,
        status,
        reason_code,
        memory_id,
        scope_id,
        artifact_family,
        producer,
        quantity,
    )
}

fn store_failure_stage(
    error: &str,
) -> (
    crate::store::ExternalLifecycleStage,
    &'static str,
    &'static str,
) {
    if error.contains("attribution") {
        (
            crate::store::ExternalLifecycleStage::Validation,
            "failed",
            "invalid_attribution",
        )
    } else if error.contains("embed") || error.contains("writes disabled") {
        (
            crate::store::ExternalLifecycleStage::Embedding,
            "unavailable",
            "embedding_unavailable",
        )
    } else {
        (
            crate::store::ExternalLifecycleStage::Commit,
            "failed",
            "commit_failed",
        )
    }
}

fn get_failure_shape(error: &str) -> (&'static str, &'static str, usize) {
    if error.starts_with("no memory with id ") {
        ("empty", "target_not_found", 0)
    } else {
        ("failed", "provider_failed", 1)
    }
}

fn build_info() -> serde_json::Value {
    serde_json::json!({
        "product_version": env!("CARGO_PKG_VERSION"),
        "membrane_source_commit": crate::release_identity::source_commit().unwrap_or("unknown"),
        "source_tree_sha256": crate::release_identity::source_tree_sha256().unwrap_or("unknown"),
        "release_generation": crate::release_identity::release_generation(),

        "release_identity_source": crate::release_identity::source_kind(),
        "store_replacement": rightkit_memory_store::store_replacement_status(&crate::cli::cortex_db_path()),
        "target": crate::release_identity::target_triple(),
    })
}

fn read_adapt_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("read Adapt input: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Adapt input must be a regular file".into());
    }
    let mut body = Vec::new();
    file.take(1_048_577)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > 1_048_576 {
        return Err("Adapt input exceeds 1 MiB".into());
    }
    serde_json::from_slice(&body).map_err(|error| format!("parse Adapt input: {error}"))
}

const TASTE_REVIEW_INPUT_CONTRACT: &str = "adapt.taste-review-input.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TasteReviewSourceV1 {
    /// Canonical absolute path for file sources; empty only when
    /// `stream_id` binds an absorbed-stream source instead.
    path: PathBuf,
    #[serde(default)]
    host: Option<String>,
    /// File sources: the parser prefix receipt digest. Stream sources: the
    /// committed stream tip `prefixDigest` at `tip_seq`.
    prefix_digest: String,
    /// Absorbed-stream identity (the CodeRight session id). Present only on
    /// `MineSession` output; when set the source is reverified against the
    /// committed stream tip at `tip_seq`, never the filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream_id: Option<String>,
    /// The stream position the mine was bound to; reverification replays
    /// exactly `1..=tip_seq` so later appends cannot move the reviewed set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tip_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TasteReviewInputV1 {
    contract: String,
    scope: String,
    sources: Vec<TasteReviewSourceV1>,
    candidate_set_sha256: String,
}

fn taste_candidate_set_sha256(candidates: &[rightkit_adapt::taste::TasteCandidateV1]) -> String {
    rightkit_adapt::canonical::sha256_canonical(
        &serde_json::to_value(candidates).expect("Taste candidates serialize"),
    )
}

fn require_current_taste_canonical_pool(
    reviewed_pool_sha256: &str,
    current: &rightkit_adapt::proposal::Gate1ReviewContextV1,
) -> Result<(), String> {
    if reviewed_pool_sha256 != current.canonical_pool_sha256() {
        return Err("Taste canonical pool changed since review".into());
    }
    Ok(())
}

fn adapt_scope_dimensions(
    values: &[String],
) -> Result<rightkit_adapt::scope::ScopeDimensions, String> {
    let mut raw = std::collections::BTreeMap::new();
    for value in values {
        let Some((key, item)) = value.split_once('=') else {
            return Err(format!(
                "invalid Adapt dimension `{value}`; expected KEY=VALUE"
            ));
        };
        if raw.insert(key.to_string(), item.to_string()).is_some() {
            return Err(format!("duplicate Adapt dimension `{key}`"));
        }
    }
    rightkit_adapt::scope::ScopeDimensions::normalize(&raw).map_err(|error| error.to_string())
}

/// Hub-safe input for native Adapt mining. Caller-selected transcript files
/// are the only Taste source; source binding is established by the parser
/// prefix receipt and rechecked during review.
#[derive(Debug, Clone)]
pub struct NativeAdaptMineRequest {
    pub transcripts: Vec<PathBuf>,
    pub host: Option<String>,
    pub scope: String,
    pub min_recurrence: u32,
}

/// Execute deterministic Adapt mining in-process. This is shared by CLI & Hub
/// so resident scheduling never spawns another Membrane authority process.
pub fn run_native_adapt_mine(request: NativeAdaptMineRequest) -> Result<serde_json::Value, String> {
    use rightkit_adapt::cli_api;

    if request.scope.trim().is_empty() || request.min_recurrence < 1 {
        return Err("invalid native Adapt mine request".into());
    }
    let mut events = Vec::new();
    let mut taste_candidates = Vec::new();
    let mut omissions = Vec::new();
    let mut taste_review_sources = Vec::new();
    let mut sources: Vec<(PathBuf, Option<String>)> = request
        .transcripts
        .into_iter()
        .map(|path| (path, request.host.clone()))
        .collect();
    sources.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    sources.dedup();
    if sources.is_empty() {
        return Err("Adapt mine requires at least one transcript source".into());
    }
    for (transcript, source_host) in sources {
        let prefix = match rightkit_agent_transcript::parse_prefix_receipt(
            &transcript,
            source_host.as_deref(),
        ) {
            Ok(prefix) => prefix,
            Err(error) => {
                omissions.push(serde_json::json!({
                    "host": source_host, "path": transcript, "stage": "receipt", "reason": error.to_string(),
                }));
                continue;
            }
        };
        let normalized = match rightkit_agent_transcript::parse_source_events(
            &transcript,
            source_host.as_deref(),
        ) {
            Ok(events) => events,
            Err(error) => {
                omissions.push(serde_json::json!({
                    "host": source_host, "path": transcript, "stage": "normalize", "reason": error.to_string(),
                }));
                continue;
            }
        };
        taste_review_sources.push(TasteReviewSourceV1 {
            path: std::fs::canonicalize(&transcript).map_err(|error| {
                format!(
                    "canonicalize Taste source {}: {error}",
                    transcript.display()
                )
            })?,
            host: source_host.clone(),
            prefix_digest: prefix.receipt.prefix_digest.clone(),
            stream_id: None,
            tip_seq: None,
        });
        taste_candidates.extend(rightkit_adapt::taste::extract_candidates_with_source(
            &normalized,
            &request.scope,
            &prefix.receipt.prefix_digest,
        ));
        for event in &normalized {
            match rightkit_adapt::insights::TranscriptEventV1::try_from(event) {
                Ok(event) => events.push(event),
                Err(reason) => omissions.push(serde_json::json!({
                    "event_id": event.event_id, "reason": reason,
                })),
            }
        }
    }
    taste_candidates.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
    taste_candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
    let taste_review = TasteReviewInputV1 {
        contract: TASTE_REVIEW_INPUT_CONTRACT.into(),
        scope: request.scope,
        sources: taste_review_sources,
        candidate_set_sha256: taste_candidate_set_sha256(&taste_candidates),
    };
    let response = cli_api::handle_mine(&cli_api::MineRequest {
        events,
        min_recurrence: request.min_recurrence,
    });
    Ok(serde_json::json!({
        "response": response,
        "taste_candidates": taste_candidates,
        "taste_review": taste_review,
        "omissions": omissions,
    }))
}

/// The absorbed-stream twin of [`run_native_adapt_mine`]: identical output
/// shape, identical normalization (`rightkit_agent_transcript::parse_absorbed_rows`
/// shares `parse_transcript`'s RawEvent loop), identical candidate
/// extraction. The only difference is the source: committed
/// `coderight_event_v1` rows from the session's absorbed stream, bound by the
/// stream tip's `prefixDigest` instead of a file prefix receipt.
fn run_native_adapt_mine_stream(
    store: &MemoryStore,
    session_ids: &[String],
    scope: &str,
    min_recurrence: u32,
) -> Result<serde_json::Value, String> {
    use crate::adapt_observations::session_transcript as stream;
    use rightkit_adapt::cli_api;

    if scope.trim().is_empty() || min_recurrence < 1 {
        return Err("invalid native Adapt mine-session request".into());
    }
    let db = rightkit_memory_store::AbsorbedStore::new(store.db().clone())
        .map_err(|error| format!("open absorbed store: {error}"))?;
    let mut events = Vec::new();
    let mut taste_candidates = Vec::new();
    let mut omissions = Vec::new();
    let mut taste_review_sources = Vec::new();
    let mut sessions: Vec<String> = session_ids.to_vec();
    sessions.sort();
    sessions.dedup();
    if sessions.is_empty() {
        return Err("Adapt mine-session requires at least one session id".into());
    }
    for session_id in sessions {
        // A forgotten session (or one that never exported) has no committed
        // tip: it is an omission, not a candidate source.
        match stream::session_forgotten(store, &session_id) {
            Ok(true) => {
                omissions.push(serde_json::json!({
                    "session_id": session_id, "stage": "stream",
                    "reason": "session transcript stream is forgotten",
                }));
                continue;
            }
            Ok(false) => {}
            Err(error) => {
                omissions.push(serde_json::json!({
                    "session_id": session_id, "stage": "stream",
                    "reason": error.detail(),
                }));
                continue;
            }
        }
        let tip = match stream::tip(&db, &session_id) {
            Ok(tip) if tip.last_seq > 0 => tip,
            Ok(_) => {
                omissions.push(serde_json::json!({
                    "session_id": session_id, "stage": "stream",
                    "reason": "session has no committed transcript events",
                }));
                continue;
            }
            Err(error) => {
                omissions.push(serde_json::json!({
                    "session_id": session_id, "stage": "stream",
                    "reason": error.detail(),
                }));
                continue;
            }
        };
        let rows = match db.events_range(&session_id, 1, tip.last_seq + 1) {
            Ok(rows) => rows,
            Err(error) => {
                omissions.push(serde_json::json!({
                    "session_id": session_id, "stage": "stream",
                    "reason": format!("read absorbed stream: {error}"),
                }));
                continue;
            }
        };
        let native_rows: Vec<(u64, serde_json::Value)> =
            rows.into_iter().map(|row| (row.seq, row.payload)).collect();
        let normalized = rightkit_agent_transcript::parse_absorbed_rows(
            &session_id,
            &tip.prefix_digest,
            &native_rows,
        );
        if normalized.is_empty() {
            omissions.push(serde_json::json!({
                "session_id": session_id, "stage": "normalize",
                "reason": "absorbed stream yielded no normalizable events",
            }));
            continue;
        }
        taste_review_sources.push(TasteReviewSourceV1 {
            path: PathBuf::new(),
            host: Some(
                rightkit_context_protocol::session_transcript::SESSION_TRANSCRIPT_HOST.to_string(),
            ),
            prefix_digest: tip.prefix_digest.clone(),
            stream_id: Some(session_id.clone()),
            tip_seq: Some(tip.last_seq),
        });
        taste_candidates.extend(rightkit_adapt::taste::extract_candidates_with_source(
            &normalized,
            scope,
            &tip.prefix_digest,
        ));
        for event in &normalized {
            match rightkit_adapt::insights::TranscriptEventV1::try_from(event) {
                Ok(event) => events.push(event),
                Err(reason) => omissions.push(serde_json::json!({
                    "event_id": event.event_id, "reason": reason,
                })),
            }
        }
    }
    taste_candidates.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
    taste_candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
    let taste_review = TasteReviewInputV1 {
        contract: TASTE_REVIEW_INPUT_CONTRACT.into(),
        scope: scope.to_string(),
        sources: taste_review_sources,
        candidate_set_sha256: taste_candidate_set_sha256(&taste_candidates),
    };
    let response = cli_api::handle_mine(&cli_api::MineRequest {
        events,
        min_recurrence,
    });
    Ok(serde_json::json!({
        "response": response,
        "taste_candidates": taste_candidates,
        "taste_review": taste_review,
        "omissions": omissions,
    }))
}

fn adapt_value<T: Serialize>(value: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

impl AdaptCmd {
    pub(crate) fn requires_canonical_store(&self) -> bool {
        !matches!(
            self,
            Self::Mine { .. }
                | Self::Review { .. }
                | Self::Report { .. }
                | Self::Benchmark { .. }
                | Self::ContextCost { .. }
                | Self::Doctor { input: Some(_) }
        )
    }
}

pub(crate) fn execute_adapt_command(
    command: AdaptCmd,
    resident: Option<&MemoryStore>,
    deployed: Option<&DeployedRuntime>,
) -> Result<serde_json::Value, String> {
    use rightkit_adapt::cli_api;

    match command {
        AdaptCmd::ProposalPlan { store, input } => {
            let request: crate::adapt::AdaptProposalPlanRequestV1 = read_adapt_json(&input)?;
            let plan = crate::adapt::execute_adapt_proposal_plan(&store, request)
                .map_err(|error| format!("execute Adapt proposal plan: {error}"))?;
            adapt_value(&serde_json::json!({
                "contract": crate::adapt::ADAPT_PROPOSAL_SERVICE_CONTRACT,
                "plan": plan,
            }))
        }
        AdaptCmd::Mine {
            transcripts,
            host,
            scope,
            min_recurrence,
        } => adapt_value(&run_native_adapt_mine(NativeAdaptMineRequest {
            transcripts,
            host,
            scope,
            min_recurrence,
        })?),
        AdaptCmd::MineSession {
            session_ids,
            scope,
            min_recurrence,
        } => {
            let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
            adapt_value(&run_native_adapt_mine_stream(
                store,
                &session_ids,
                &scope,
                min_recurrence,
            )?)
        }
        AdaptCmd::Review { input, issue_ids } => {
            let value: serde_json::Value = read_adapt_json(&input)?;
            let mined: cli_api::MineResponse =
                serde_json::from_value(value.get("response").cloned().unwrap_or(value))
                    .map_err(|error| format!("parse Adapt mine response: {error}"))?;
            adapt_value(&cli_api::handle_review(&cli_api::ReviewRequest {
                issue_ids,
                issues: mined.issues,
            }))
        }
        AdaptCmd::ReviewTaste {
            input,
            installation_id,
            canonical_pool_sha256,
            created_at,
        } => {
            let value: serde_json::Value = read_adapt_json(&input)?;
            let review: TasteReviewInputV1 = serde_json::from_value(
                value.get("taste_review").cloned().ok_or_else(||
                    "Taste review input lacks native source bindings; bare candidate JSON is not authoritative".to_string())?
            ).map_err(|error| format!("parse Adapt Taste source bindings: {error}"))?;
            if review.contract != TASTE_REVIEW_INPUT_CONTRACT || review.scope.trim().is_empty() {
                return Err("invalid Adapt Taste review source contract".into());
            }
            for source in &review.sources {
                if let Some(stream_id) = source.stream_id.as_deref() {
                    // Stream sources carry no filesystem path; the binding is
                    // the committed stream tip they were mined at.
                    if !source.path.as_os_str().is_empty() || source.tip_seq.is_none() {
                        return Err(format!("invalid Taste stream source binding: {stream_id}"));
                    }
                    continue;
                }
                let canonical = std::fs::canonicalize(&source.path).map_err(|error| {
                    format!(
                        "canonicalize Taste review source {}: {error}",
                        source.path.display()
                    )
                })?;
                if !source.path.is_absolute() || canonical != source.path {
                    return Err("Taste review source paths must be canonical absolute paths".into());
                }
            }
            let mut candidates = Vec::new();
            for source in &review.sources {
                if let Some(stream_id) = source.stream_id.as_deref() {
                    use crate::adapt_observations::session_transcript as stream;
                    let tip_seq = source.tip_seq.ok_or_else(|| {
                        format!("invalid Taste stream source binding: {stream_id}")
                    })?;
                    // Stream reverification needs the resident store; the
                    // file path's own check later keeps its behavior for
                    // pure-file inputs.
                    let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
                    if stream::session_forgotten(store, stream_id)
                        .map_err(|error| error.detail())?
                    {
                        return Err(format!("Taste stream source is forgotten: {stream_id}"));
                    }
                    let db = rightkit_memory_store::AbsorbedStore::new(store.db().clone())
                        .map_err(|error| format!("open absorbed store: {error}"))?;
                    // Recompute the tip digest at the RECORDED position: the
                    // append-only stream cannot mutate a committed prefix, so
                    // a match proves the mined rows are still the committed
                    // ones even if the stream has since grown.
                    let tip =
                        stream::tip_at(&db, stream_id, tip_seq).map_err(|error| error.detail())?;
                    if tip.prefix_digest != source.prefix_digest {
                        return Err(format!("Taste stream source digest changed: {stream_id}"));
                    }
                    let rows = db
                        .events_range(stream_id, 1, tip_seq + 1)
                        .map_err(|error| format!("re-read Taste stream {stream_id}: {error}"))?;
                    let native_rows: Vec<(u64, serde_json::Value)> =
                        rows.into_iter().map(|row| (row.seq, row.payload)).collect();
                    let normalized = rightkit_agent_transcript::parse_absorbed_rows(
                        stream_id,
                        &tip.prefix_digest,
                        &native_rows,
                    );
                    candidates.extend(rightkit_adapt::taste::extract_candidates_with_source(
                        &normalized,
                        &review.scope,
                        &tip.prefix_digest,
                    ));
                    continue;
                }
                let prefix = rightkit_agent_transcript::parse_prefix_receipt(
                    &source.path,
                    source.host.as_deref(),
                )
                .map_err(|error| {
                    format!("verify Taste source {}: {error}", source.path.display())
                })?;
                if prefix.receipt.prefix_digest != source.prefix_digest {
                    return Err(format!(
                        "Taste source digest changed: {}",
                        source.path.display()
                    ));
                }
                let normalized = rightkit_agent_transcript::parse_source_events(
                    &source.path,
                    source.host.as_deref(),
                )
                .map_err(|error| {
                    format!("reparse Taste source {}: {error}", source.path.display())
                })?;
                candidates.extend(rightkit_adapt::taste::extract_candidates_with_source(
                    &normalized,
                    &review.scope,
                    &prefix.receipt.prefix_digest,
                ));
            }
            candidates.sort_by(|left, right| left.candidate_id.cmp(&right.candidate_id));
            candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
            if taste_candidate_set_sha256(&candidates) != review.candidate_set_sha256 {
                return Err("Taste candidate set does not match reverified native sources".into());
            }
            let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
            if installation_id != store.installation_id() {
                return Err("Adapt installation identity mismatch".into());
            }
            let gate1 = store
                .taste_gate1_review_context()
                .map_err(|error| format!("load complete Taste Gate 1 context: {error}"))?;
            let canonical_pool_sha256 =
                canonical_pool_sha256.unwrap_or_else(|| gate1.canonical_pool_sha256().to_string());
            let manifest = rightkit_adapt::proposal::build_pending_manifest(
                &candidates,
                &installation_id,
                &canonical_pool_sha256,
                &created_at,
                &gate1,
            )
            .map_err(|error| format!("build Taste review manifest: {error}"))?;
            adapt_value(&manifest)
        }
        AdaptCmd::AdjudicateTaste {
            manifest,
            decisions,
            validated_at,
        } => {
            let pending: rightkit_adapt::manifest::PreferenceManifestV1 =
                read_adapt_json(&manifest)?;
            let adjudication: rightkit_adapt::proposal::SemanticAdjudicationV1 =
                read_adapt_json(&decisions)?;
            if adjudication.validated_at != validated_at {
                return Err("validated-at must match the semantic adjudication".into());
            }
            let verified = if adjudication.contract_version
                == rightkit_adapt::proposal::USER_TASTE_REVIEW_CONTRACT
            {
                rightkit_adapt::proposal::verify_user_taste_review(&pending, adjudication)
            } else {
                let trust_path = deployed
                    .map(|runtime| runtime.semantic_adjudicator_trust.as_path())
                    .ok_or_else(|| {
                        "deployed semantic-adjudicator trust is unavailable".to_string()
                    })?;
                let trust =
                    rightkit_adapt::proposal::SemanticAdjudicatorTrustStoreV1::load(trust_path)
                        .map_err(|error| format!("load semantic-adjudicator trust: {error}"))?;
                rightkit_adapt::proposal::verify_semantic_adjudication(
                    &pending,
                    adjudication,
                    &trust,
                )
            }
            .map_err(|error| format!("verify Taste semantic adjudication: {error}"))?;
            let finalised = rightkit_adapt::proposal::adjudicate_manifest(&pending, &verified)
                .map_err(|error| format!("adjudicate Taste manifest: {error}"))?;
            adapt_value(&finalised)
        }
        AdaptCmd::Apply { manifest, dry_run } => {
            let manifest: rightkit_adapt::manifest::PreferenceManifestV1 =
                read_adapt_json(&manifest)?;
            let local_review = manifest
                .semantic_adjudication
                .as_ref()
                .is_some_and(|value| {
                    value.contract_version == rightkit_adapt::proposal::USER_TASTE_REVIEW_CONTRACT
                });
            let trust = if local_review {
                None
            } else {
                let trust_path = deployed
                    .map(|runtime| runtime.semantic_adjudicator_trust.as_path())
                    .ok_or_else(|| {
                        "deployed semantic-adjudicator trust is unavailable".to_string()
                    })?;
                Some(
                    rightkit_adapt::proposal::SemanticAdjudicatorTrustStoreV1::load(trust_path)
                        .map_err(|error| format!("load semantic-adjudicator trust: {error}"))?,
                )
            };
            rightkit_adapt::proposal::verify_final_manifest_adjudication_or_local(
                &manifest,
                trust.as_ref(),
            )
            .map_err(|error| format!("verify Taste semantic adjudication before apply: {error}"))?;
            let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
            if manifest.installation_id != store.installation_id() {
                return Err("Adapt installation identity mismatch".into());
            }
            if dry_run {
                let manifest_record_payloads: std::collections::BTreeMap<String, String> = manifest
                    .records
                    .iter()
                    .map(|record| (record.id.clone(), record.payload_sha256.clone()))
                    .collect();
                let gate1 = store
                    .taste_gate1_review_context_excluding(&manifest_record_payloads)
                    .map_err(|error| format!("reload complete Taste Gate 1 context: {error}"))?;
                require_current_taste_canonical_pool(&manifest.canonical_pool_sha256, &gate1)?;
            }
            let response = cli_api::handle_apply(&cli_api::ApplyRequest {
                manifest: manifest.clone(),
                dry_run,
            });
            if !response.valid || dry_run || response.accepted_record_ids.is_empty() {
                return adapt_value(&serde_json::json!({"response": response}));
            }
            let receipt = store
                .try_put_verified_adapt_taste_manifest(&manifest, trust.as_ref())
                .map_err(|error| error.to_string())?;
            adapt_value(&serde_json::json!({
                "response": response,
                "cortex_receipt": receipt,
            }))
        }
        AdaptCmd::ApplyInsights {
            input,
            installation_id,
            dry_run,
        } => {
            let issues: Vec<rightkit_adapt::insights::sealed_issue::SealedInsightIssueV1> =
                read_adapt_json(&input)?;
            for issue in &issues {
                issue.verify().map_err(|error| {
                    format!("sealed Insight {} invalid: {error:?}", issue.issue_id)
                })?;
            }
            let mut ids: Vec<String> = issues.iter().map(|issue| issue.issue_id.clone()).collect();
            ids.sort();
            ids.dedup();
            if ids.len() != issues.len() {
                return Err("duplicate sealed Insight issue id".into());
            }
            let batch_material = serde_json::to_value(&ids).expect("issue ids serialize");
            let batch_id = format!(
                "adapt-insights-{}",
                rightkit_adapt::canonical::sha256_canonical(&batch_material)
            );
            let envelopes = insight_admission_requests(&issues, &installation_id);
            if dry_run || issues.is_empty() {
                return adapt_value(&serde_json::json!({
                    "valid": true,
                    "dry_run": dry_run,
                    "batch_id": batch_id,
                    "admission_requests": envelopes,
                }));
            }
            let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
            let receipt = store
                .try_put_verified_adapt_insights(&issues)
                .map_err(|error| error.to_string())?;
            adapt_value(&serde_json::json!({
                "valid": true,
                "batch_id": batch_id,
                "admission_requests": envelopes,
                "cortex_receipt": receipt,
            }))
        }
        AdaptCmd::Recall {
            query: _,
            k,
            scope,
            dimensions,
        } => {
            let store = resident.ok_or("membrane_unavailable { hub_inactive }")?;
            let dimensions = adapt_scope_dimensions(&dimensions)?;
            crate::adapt_service::inspect_preferences(store, &scope, dimensions, None, None, k)
        }
        AdaptCmd::Status { scope } => crate::adapt_service::status(
            resident.ok_or("membrane_unavailable { hub_inactive }")?,
            &scope,
            None,
        ),
        AdaptCmd::InspectIssues { scope, limit } => crate::adapt_service::inspect_issues(
            resident.ok_or("membrane_unavailable { hub_inactive }")?,
            &scope,
            limit,
        ),
        AdaptCmd::Compare { input } => {
            let request: rightkit_adapt::comparison::CandidateComparisonV1 =
                read_adapt_json(&input)?;
            let decision = rightkit_adapt::comparison::compare(&request)?;
            let receipt = crate::adapt_service::journal(
                resident.ok_or("membrane_unavailable { hub_inactive }")?,
                &request.scope,
                "adapt.comparison",
                &request.comparison_id,
                adapt_value(&decision)?,
            )?;
            Ok(serde_json::json!({"decision":decision,"receipt":receipt}))
        }
        AdaptCmd::GuardEligibility { input } => {
            let request: rightkit_adapt::guard_rollout::GuardTransitionRequestV1 =
                read_adapt_json(&input)?;
            let decision = rightkit_adapt::guard_rollout::evaluate(&request)?;
            let receipt = crate::adapt_service::journal(
                resident.ok_or("membrane_unavailable { hub_inactive }")?,
                &request.proposed_scope,
                "adapt.guard_eligibility",
                &decision.request_sha256,
                adapt_value(&decision)?,
            )?;
            Ok(serde_json::json!({"decision":decision,"receipt":receipt}))
        }
        AdaptCmd::Report { input } => {
            let request: cli_api::ReportRequest = read_adapt_json(&input)?;
            adapt_value(&cli_api::handle_report(&request))
        }
        AdaptCmd::Benchmark { input } => {
            let body = std::fs::read_to_string(&input)
                .map_err(|error| format!("read Adapt benchmark {}: {error}", input.display()))?;
            let request = match serde_json::from_str::<cli_api::BenchmarkRequest>(&body) {
                Ok(request) => request,
                Err(_) => {
                    let mut corpus = Vec::new();
                    for (index, line) in body.lines().enumerate() {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let value: serde_json::Value =
                            serde_json::from_str(line).map_err(|error| {
                                format!("parse portable benchmark line {}: {error}", index + 1)
                            })?;
                        corpus.push(
                            rightkit_adapt::benchmark::portable_case_from_value(&value).map_err(
                                |error| format!("portable benchmark line {}: {error}", index + 1),
                            )?,
                        );
                    }
                    cli_api::BenchmarkRequest { corpus }
                }
            };
            adapt_value(&cli_api::handle_benchmark(&request))
        }
        AdaptCmd::Doctor { input } => {
            let request = match input {
                Some(input) => read_adapt_json(&input)?,
                None => {
                    return crate::adapt_service::status(
                        resident.ok_or("membrane_unavailable { hub_inactive }")?,
                        "global",
                        None,
                    );
                }
            };
            adapt_value(&cli_api::handle_doctor(&request))
        }
        AdaptCmd::ContextCost { input } => {
            let request: cli_api::ContextCostRequest = read_adapt_json(&input)?;
            let response = cli_api::handle_context_cost(&request)
                .map_err(|error| format!("analyze Adapt context cost: {error}"))?;
            adapt_value(&response)
        }
    }
}

fn run_installation(command: &InstallationCmd) -> Result<(), String> {
    let workspace_root = skill_workspace_root(None);
    let paths = crate::installation_identity::InstallationPaths::for_workspace(&workspace_root);
    match command {
        InstallationCmd::Rotate { reason } => {
            let identity =
                crate::installation_identity::rotate_installation(&paths.identity, reason)
                    .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": identity.schema_version,
                    "installation_id": identity.installation_id,
                    "startup_generation": identity.startup_generation,
                    "lineage": identity.lineage,
                })
            );
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ReplayQuery {
    row_id: String,
    query: String,
    scope: String,
}

#[derive(Debug, Serialize)]
struct ReplayResult {
    row_id: String,
    ranked_ids: Vec<String>,
}

fn replay_queries<R: Read>(
    store: &MemoryStore,
    reader: R,
    k: usize,
) -> Result<Vec<ReplayResult>, String> {
    let scopes = store.scopes();
    std::io::BufReader::new(reader)
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Ok(line) if line.trim().is_empty() => None,
            other => Some((index, other)),
        })
        .map(|(index, line)| {
            let line = line.map_err(|error| format!("read replay line {}: {error}", index + 1))?;
            let row: ReplayQuery = serde_json::from_str(&line)
                .map_err(|error| format!("parse replay line {}: {error}", index + 1))?;
            if row.row_id.trim().is_empty() || row.query.trim().is_empty() {
                return Err(format!("replay line {} has empty row_id/query", index + 1));
            }
            let scope = normalize_scope(&row.scope);
            let chain = crate::scope_chain(&scope, &scopes);
            let ranked_ids = store
                .recall_scored(&row.query, k, &chain)
                .into_iter()
                .map(|(entry, _score)| entry.id)
                .collect();
            Ok(ReplayResult {
                row_id: row.row_id,
                ranked_ids,
            })
        })
        .collect()
}

fn run_main() -> Result<(), String> {
    run_main_with_argv(std::env::args().collect())
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum RetrievalArmArg {
    LexicalOnly,
    VectorOnly,
    Hybrid,
}

impl From<RetrievalArmArg> for rightkit_memory_core::retriever::RetrievalArm {
    fn from(value: RetrievalArmArg) -> Self {
        match value {
            RetrievalArmArg::LexicalOnly => Self::LexicalOnly,
            RetrievalArmArg::VectorOnly => Self::VectorOnly,
            RetrievalArmArg::Hybrid => Self::Hybrid,
        }
    }
}

/// Accept an explicit argv so `membrane memory ...` can forward without re-execing.
fn run_main_with_argv(argv: Vec<String>) -> Result<(), String> {
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            let rendered = error.to_string();
            if code == 0 {
                println!("{rendered}");
            } else {
                eprintln!("{rendered}");
            }
            return if code == 0 {
                Ok(())
            } else {
                Err(format!("CLI exited with status {code}"))
            };
        }
    };
    if let Cmd::Installation { command } = &cli.cmd {
        return run_installation(command);
    }
    if let Cmd::SkillRead {
        name,
        root,
        max_chars,
        privacy_values,
    } = &cli.cmd
    {
        return run_skill_read(
            name.clone(),
            root.clone(),
            *max_chars,
            privacy_values.clone(),
        );
    }
    let db = resolve_db(cli.db);
    // A resident that is serving holds the owner lock for its whole life; a maintenance write
    // takes it briefly, or says the resident is serving.
    let _owner = if command_takes_owner_lock(&cli.cmd) {
        Some(acquire_owner_lock()?)
    } else {
        None
    };
    match cli.cmd {
        Cmd::Installation { .. } => unreachable!("handled before database resolution"),

        Cmd::Checkpoint { command } => {
            let store = MemoryStore::open(MemDb::open(&db).map_err(|error| error.to_string())?);
            let now_ms = || crate::time::now_millis() as i64;
            match command {
                CheckpointCmd::Save { input } => {
                    let body = match input {
                        Some(input) => std::fs::read_to_string(&input).map_err(|error| {
                            format!("read checkpoint {}: {error}", input.display())
                        })?,
                        None => {
                            let body = String::from_utf8(read_request_stdin()?)
                                .map_err(|error| format!("read checkpoint stdin: {error}"))?;
                            body
                        }
                    };
                    let checkpoint: CheckpointV1 = serde_json::from_str(&body)
                        .map_err(|error| format!("parse checkpoint: {error}"))?;
                    store
                        .save_checkpoint(&checkpoint)
                        .map_err(|error| error.to_string())?;
                    println!(
                        "{}",
                        serde_json::json!({"checkpoint_id": checkpoint.checkpoint_id, "saved": true})
                    );
                }
                CheckpointCmd::Load { id, as_of_ms } => {
                    let checkpoint = store
                        .load_checkpoint(&id, as_of_ms.unwrap_or_else(now_ms))
                        .map_err(|error| error.to_string())?;
                    let root = crate::qa_env::workspace_root()
                        .map(PathBuf::from)
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_else(|| PathBuf::from("."));
                    println!(
                        "{}",
                        serde_json::json!({
                            "checkpoint": checkpoint,
                            "source_resolutions": crate::checkpoint::resolve_source_refs(&checkpoint, &root),
                        })
                    );
                }
                CheckpointCmd::List { scope, as_of_ms } => {
                    let checkpoints = store
                        .list_checkpoints(&scope, as_of_ms.unwrap_or_else(now_ms))
                        .map_err(|error| error.to_string())?;
                    println!(
                        "{}",
                        serde_json::to_string(&checkpoints).map_err(|error| error.to_string())?
                    );
                }
                CheckpointCmd::Done { id } => {
                    store
                        .close_checkpoint(&id)
                        .map_err(|error| error.to_string())?;
                    println!(
                        "{}",
                        serde_json::json!({"checkpoint_id": id, "closed": true})
                    );
                }
                CheckpointCmd::Promote { id, as_of_ms } => {
                    // CTX-019: promotion is proposal-first and governed. The
                    // checkpoint's own recorded repository/scope binding is the
                    // caller; the summary enters the durable proposal queue and
                    // never becomes canonical truth directly.
                    let checkpoint = store
                        .load_checkpoint(&id, as_of_ms.unwrap_or_else(now_ms))
                        .map_err(|error| error.to_string())?;
                    let receipt = crate::cortex_lifecycle::promote_checkpoint(
                        &store,
                        &checkpoint.repository_id,
                        &checkpoint.scope_id,
                        &id,
                    )
                    .map_err(|error| error.to_string())?;
                    println!("{receipt}");
                }
            }
        }
        Cmd::Hygiene { command } => match command {
            HygieneCmd::Storage => println!("{}", storage_hygiene_report(&db)?),
            HygieneCmd::Audit => println!("{}", hygiene_report(&db)?),
            HygieneCmd::Clean { plan } => {
                let report = hygiene_report(&db)?;
                if !plan {
                    return Err(
                        "hygiene clean requires --plan; no implicit mutation is allowed".into(),
                    );
                }
                let actions = report["findings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|finding| finding["count"].as_u64().unwrap_or(0) > 0)
                        .map(|finding| serde_json::json!({
                            "code": finding["code"],
                            "targets": finding.get("targets").cloned().unwrap_or_else(|| serde_json::json!([])),
                            "action": "review_then_apply_separately"
                        }))
                        .collect::<Vec<_>>();
                println!(
                    "{}",
                    serde_json::json!({
                        "schema":"membrane.hygiene-clean-plan.v1",
                        "applied":false,
                        "reversible":true,
                        "audit":report,
                        "actions":actions
                    })
                );
            }
        },
        Cmd::Explain { id } => println!("{}", explain_memory(&db, &id)?),
        Cmd::Recall {
            query,
            k,
            scope,
            as_of_ms,
            include_expired,
            arm,
        } => {
            let store = open(&db)?;
            // normalize_scope: CLI parity with serve (a lowercase-drive --scope must not
            // silently produce an empty chain — the 5th slug call-site, fixed 2026-07-05).
            let norm = scope.as_deref().map(normalize_scope);
            let chain = norm
                .as_deref()
                .map(|s| crate::scope_chain(s, &store.scopes()))
                .unwrap_or_default();
            let arm_name = match arm {
                RetrievalArmArg::LexicalOnly => "lexical-only",
                RetrievalArmArg::VectorOnly => "vector-only",
                RetrievalArmArg::Hybrid => "hybrid",
            };
            let hits = if matches!(arm, RetrievalArmArg::Hybrid) {
                store.recall_scored_at(
                    &query,
                    k,
                    &chain,
                    as_of_ms.unwrap_or_else(|| crate::time::now_millis() as i64),
                    include_expired,
                )
            } else {
                store.recall_scored_with_arm_at(
                    &query,
                    k,
                    &chain,
                    arm.into(),
                    as_of_ms.unwrap_or_else(|| crate::time::now_millis() as i64),
                    include_expired,
                )
            };
            println!("{{\"retrieval_arm\":\"{}\"}}", arm_name);
            if hits.is_empty()
                && store.last_recall_status().as_deref() == Some("insufficient_confidence")
            {
                // Say why confidence is low. A bare status made a build with
                // no real embedder indistinguishable from a corpus that
                // genuinely had nothing to say.
                let health = store.health();
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "insufficient_confidence",
                        "hits": [],
                        "embedderModel": health.embedder_model,
                        "embedderIssue": health.embedder_issue
                    })
                );
            }
            let (mut full, mut injected) = (0usize, 0usize);
            for (e, cos) in &hits {
                full += e.content.chars().count();
                injected += e
                    .content
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .count();
                println!("{:.3}  {}  {}", cos, e.scope_id, e.id);
            }
            // source='cli' keeps human debugging out of the agent-effectiveness numbers.
            // NO record_injections: inject_count means "shown to an agent session".
            store.log_recall(
                &crate::time::now_iso(),
                norm.as_deref(),
                query.chars().count(),
                hits.len(),
                full,
                injected,
                "cli",
                Some(&query.chars().take(120).collect::<String>()),
                Some("cli"),
                None,
                norm.as_deref(),
                Some("cli-recall"),
                None,
                Some("all"),
            );
        }
        Cmd::Replay { input, k } => {
            let store = open(&db)?;
            let rows = if let Some(path) = input {
                let file = std::fs::File::open(&path)
                    .map_err(|error| format!("open replay input {}: {error}", path.display()))?;
                replay_queries(&store, file, k)?
            } else {
                replay_queries(&store, std::io::Cursor::new(read_request_stdin()?), k)?
            };
            for row in rows {
                println!(
                    "{}",
                    serde_json::to_string(&row)
                        .map_err(|error| format!("serialize replay result: {error}"))?
                );
            }
        }
        Cmd::Curate { today } => {
            let today = today.unwrap_or_else(|| crate::time::now_iso().chars().take(10).collect());
            let store = open(&db)?;
            let status = store.dream_now(&today)?;
            println!(
                "{}",
                serde_json::json!({
                    "agent_id": status.agent_id,
                    "status": status.status,
                    "model": status.model,
                    "shell_allowed": status.shell_allowed,
                    "read_count": status.read_count,
                    "consolidated_count": status.consolidated_count,
                    "pruned_count": status.pruned_count,
                    "quarantined_count": status.quarantined_count,
                    "duplicate_quarantined_count": status.duplicate_quarantined_count,
                })
            );
        }
        Cmd::QuarantineList => {
            let store = open(&db)?;
            println!("{}", serde_json::json!({ "ids": store.quarantined_ids() }));
        }
        Cmd::QuarantineRestore { id } => {
            let store = open(&db)?;
            let restored = store.restore_quarantined(&id)?;
            println!("{}", serde_json::json!({ "id": id, "restored": restored }));
        }
        Cmd::Get { id } => {
            let store = open(&db)?;
            let context = cli_event_context();
            match store.get_full_observed(&id, &context) {
                Ok((content, access_count)) => {
                    eprintln!("[use recorded] access_count={access_count}");
                    print!("{content}");
                }
                Err(error) => {
                    let (status, reason, quantity) = get_failure_shape(&error);
                    record_cli_external(
                        &store,
                        &context,
                        "read",
                        crate::store::ExternalLifecycleStage::Provider,
                        status,
                        reason,
                        Some(&id),
                        None,
                        "memory",
                        "cli",
                        quantity,
                    )?;
                    return Err(error);
                }
            }
        }
        Cmd::Feedback {
            trace,
            candidate,
            sha,
            outcome,
            source,
            verdict_ref,
            scope,
        } => {
            let store = open(&db)?;
            let rec = crate::feedback::FeedbackRecord {
                trace_id: trace,
                candidate_id: candidate,
                content_sha256: sha,
                outcome: crate::feedback::parse_outcome(&outcome)?,
                source: crate::feedback::parse_source(&source),
                verdict_ref,
                scope_id: scope,
            };
            store.record_feedback(&rec)?;
            println!("{{\"ok\":true,\"verified\":{}}}", rec.verified());
        }
        Cmd::CloseUnknown {
            observed_since,
            observed_through,
            max_deliveries,
        } => {
            let store = open(&db)?;
            let closed = store.close_unresolved_deliveries(
                &observed_since,
                &observed_through,
                max_deliveries,
            )?;
            println!("{{\"ok\":true,\"closed\":{closed}}}");
        }
        Cmd::LearningRegister { input } => {
            let request: crate::store::LearningExperimentV1 =
                serde_json::from_slice(&std::fs::read(input).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let store = open(&db)?;
            let request_sha256 = store.register_learning_experiment(&request)?;
            println!(
                "{}",
                serde_json::json!({"schemaVersion":1,"experimentId":request.experiment_id,"requestSha256":request_sha256,"status":"registered"})
            );
        }
        Cmd::LearningQualify { experiment } => {
            let store = open(&db)?;
            println!(
                "{}",
                serde_json::to_string(&store.qualify_learning_experiment(&experiment)?)
                    .map_err(|e| e.to_string())?
            );
        }
        Cmd::LearningReceipt { experiment } => {
            let store = open(&db)?;
            println!(
                "{}",
                serde_json::to_string(&store.learning_promotion_receipt(&experiment)?)
                    .map_err(|e| e.to_string())?
            );
        }
        Cmd::SkillRead { .. } => unreachable!("handled before database resolution"),
        Cmd::IngestSkills { root } => {
            let store = open(&db)?;
            let ws = skill_workspace_root(root);
            let (ingested, skipped, pruned) = store.ingest_skills(&ws);
            println!(
                "{{\"skills_ingested\":{ingested},\"skills_skipped\":{skipped},\"skills_pruned\":{pruned}}}"
            );
        }
        Cmd::Put(args) => {
            let PutArgs {
                name,
                scope,
                tier,
                file,
                artifact_family,
                producer,
                record_type,
                authority,
                influence_class,
                session,
                trace,
                effective_from_ms,
                effective_until_ms,
                expires_at_ms,
                review_after_ms,
                priority_class,
                confidence,
                confidence_basis,
                supersedes,
            } = *args;
            let context = put_event_context(session.as_deref(), trace.as_deref());
            let memory_id = format!("{}/{}", crate::scope::normalize_scope(&scope), name);
            let content = match file {
                Some(p) => match std::fs::read_to_string(&p) {
                    Ok(content) => content,
                    Err(error) => {
                        let store = open(&db)?;
                        record_cli_external(
                            &store,
                            &context,
                            "write",
                            crate::store::ExternalLifecycleStage::Validation,
                            "unavailable",
                            "input_unavailable",
                            Some(&memory_id),
                            Some(&scope),
                            "memory",
                            "cli",
                            1,
                        )?;
                        return Err(error.to_string());
                    }
                },
                None => {
                    let mut s = String::new();
                    if let Err(error) =
                        String::from_utf8(read_request_stdin()?).map(|value| s = value)
                    {
                        let store = open(&db)?;
                        record_cli_external(
                            &store,
                            &context,
                            "write",
                            crate::store::ExternalLifecycleStage::Validation,
                            "unavailable",
                            "input_unavailable",
                            Some(&memory_id),
                            Some(&scope),
                            "memory",
                            "cli",
                            1,
                        )?;
                        return Err(error.to_string());
                    }
                    s
                }
            };
            if content.trim().is_empty() {
                let store = open(&db)?;
                record_cli_external(
                    &store,
                    &context,
                    "write",
                    crate::store::ExternalLifecycleStage::Validation,
                    "failed",
                    "empty_content",
                    Some(&memory_id),
                    Some(&scope),
                    "memory",
                    "cli",
                    1,
                )?;
                return Err("refusing to put empty content".into());
            }
            if let Some(influence_class) = trust_scan_content(&content) {
                quarantine_untrusted_put(&db, &memory_id, &scope, &content, influence_class);
                let store = open(&db)?;
                record_cli_external(
                    &store,
                    &context,
                    "write",
                    crate::store::ExternalLifecycleStage::Validation,
                    // "quarantined" is not a lifecycle status; the quarantine is the reason code.
                    "failed",
                    influence_class,
                    Some(&memory_id),
                    Some(&scope),
                    &artifact_family,
                    &producer,
                    1,
                )?;
                return Err(format!("memory content quarantined: {influence_class}"));
            }
            // Hand-typed scopes fork the corpus (the 2026-07-05 'heardright' mirror fork).
            // Refuse clearly invalid single-token scopes; allow proposed/global/cwd-slug/path.
            crate::scope::validate_write_scope(&scope)?;
            if !matches!(authority.as_str(), "A0" | "A1" | "A2" | "A3" | "A4" | "A5") {
                return Err("authority must be A0..=A5".into());
            }
            if influence_class.trim().is_empty() {
                return Err("influence_class must be non-empty".into());
            }
            let tier = match tier.as_str() {
                "working" => rightkit_memory_core::MemoryTier::Working,
                "episodic" => rightkit_memory_core::MemoryTier::Episodic,
                _ => rightkit_memory_core::MemoryTier::Semantic,
            };
            let store = open(&db)?;
            let lifecycle = crate::store::MemoryLifecycleInputV1 {
                effective_from_ms,
                effective_until_ms,
                expires_at_ms,
                review_after_ms,
                priority_class,
                confidence,
                confidence_basis,
                supersedes,
                authority: Some(authority),
                influence_class: Some(influence_class),
            };
            let id = match store.try_put_attributed_lifecycle_observed(
                &name,
                content.trim(),
                &scope,
                tier,
                &artifact_family,
                &producer,
                &record_type,
                &context,
                &lifecycle,
            ) {
                Ok(id) => id,
                Err(error) => {
                    let (stage, status, reason) = store_failure_stage(&error);
                    record_cli_external(
                        &store,
                        &context,
                        "write",
                        stage,
                        status,
                        reason,
                        Some(&memory_id),
                        Some(&scope),
                        if reason == "invalid_attribution" {
                            "memory"
                        } else {
                            &artifact_family
                        },
                        if reason == "invalid_attribution" || producer.trim().is_empty() {
                            "cli"
                        } else {
                            &producer
                        },
                        1,
                    )?;
                    return Err(error);
                }
            };
            println!("{{\"put\":{id:?}}}");
        }
        Cmd::Delete { id } => {
            let context = cli_event_context();
            let store = open(&db)?;
            let removed = match store.try_delete_observed(&id, &context) {
                Ok(true) => true,
                Ok(false) => {
                    record_cli_external(
                        &store,
                        &context,
                        "delete",
                        crate::store::ExternalLifecycleStage::Commit,
                        "empty",
                        "target_not_found",
                        Some(&id),
                        None,
                        "memory",
                        "cli",
                        0,
                    )?;
                    false
                }
                Err(error) => {
                    record_cli_external(
                        &store,
                        &context,
                        "delete",
                        crate::store::ExternalLifecycleStage::Commit,
                        "failed",
                        "commit_failed",
                        Some(&id),
                        None,
                        "memory",
                        "cli",
                        1,
                    )?;
                    return Err(error);
                }
            };
            println!("{{\"deleted\":{removed}}}");
        }
        Cmd::List { scope, limit } => {
            let store = open(&db)?;
            let context = cli_event_context();
            let page = match store.try_list_bounded(scope.as_deref(), limit) {
                Ok(page) => page,
                Err(error) => {
                    record_cli_external(
                        &store,
                        &context,
                        "read",
                        crate::store::ExternalLifecycleStage::Provider,
                        "failed",
                        "provider_failed",
                        None,
                        scope.as_deref(),
                        "memory",
                        "cli",
                        1,
                    )?;
                    return Err(error);
                }
            };
            let rows = page.items;
            record_cli_external(
                &store,
                &context,
                "read",
                crate::store::ExternalLifecycleStage::Provider,
                if rows.is_empty() { "empty" } else { "success" },
                if rows.is_empty() {
                    "no_results"
                } else {
                    "results_returned"
                },
                None,
                scope.as_deref(),
                "memory",
                "cli",
                rows.len(),
            )?;
            for (id, tier, chars, access, inject) in rows {
                println!("{access:>4} {inject:>4} {chars:>7} {tier:<9} {id}");
            }
            eprintln!(
                "{}",
                serde_json::to_string(&page.completeness).map_err(|error| error.to_string())?
            );
        }
        Cmd::ReviewDue { scope, limit } => {
            let store = open(&db)?;
            let page = store.lifecycle_reviews_due(
                scope.as_deref(),
                crate::time::now_millis() as i64,
                limit,
            )?;
            println!(
                "{}",
                serde_json::to_string(&page.items).map_err(|error| error.to_string())?
            );
            eprintln!(
                "{}",
                serde_json::to_string(&page.completeness).map_err(|error| error.to_string())?
            );
        }
        Cmd::DrainBackgroundProposals {
            repository,
            path,
            limit,
        } => {
            let store = open(&db)?;
            let receipt = crate::cortex_lifecycle::drain_background_proposals(
                &store,
                &repository,
                &path,
                limit,
            )
            .map_err(|error| error.to_string())?;
            println!("{receipt}");
        }
        Cmd::Graph {
            anonymous,
            neighbors,
            min_cosine,
        } => {
            let store = open(&db)?;
            println!(
                "{}",
                store.relationship_graph_json(neighbors, min_cosine, anonymous)
            );
        }
        Cmd::Reindex => {
            let store = open(&db)?;
            // With no model the store embeds with the hash stand-in; re-embedding the corpus
            // with it would replace real vectors from an earlier model with hash vectors.
            if store.health().embedder_model.starts_with("hash-") {
                return Err(format!(
                    "reindex needs a semantic model; {}",
                    crate::store::MODEL_READINESS.line()
                ));
            }
            let (n, skipped) = store.reindex()?;
            // Rebuild the [[wikilink]] edge table from content while we're re-scanning the corpus.
            let (edges, resolvable) = store.backfill_links();
            // Reingest git-tracked skill bodies into the engine skills table (portability store).
            let ws = skill_workspace_root(None);
            let (skills_in, skills_skip, skills_pruned) = store.ingest_skills(&ws);
            println!(
                "{{\"reindexed\":{n},\"skipped\":{skipped},\"link_edges\":{edges},\"resolvable_links\":{resolvable},\"skills_ingested\":{skills_in},\"skills_skipped\":{skills_skip},\"skills_pruned\":{skills_pruned}}}"
            );
        }
        Cmd::Erase { id } => {
            let store = open(&db)?;
            let erased = store.hard_erase(&id)?;
            println!("{}", serde_json::json!({"erased": erased, "id": id}));
        }
        Cmd::Backup { output } => {
            let store = open(&db)?;
            let backup = store.backup_cortex()?;
            let bytes = serde_json::to_vec_pretty(&backup).map_err(|error| error.to_string())?;
            std::fs::write(&output, bytes).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::json!({"backed_up": backup.memories.len() + backup.quarantined.len(), "output": output})
            );
        }
        Cmd::Restore { input } => {
            let store = open(&db)?;
            let raw = std::fs::read_to_string(&input).map_err(|error| error.to_string())?;
            let backup: crate::store::CortexBackupV1 =
                serde_json::from_str(&raw).map_err(|error| error.to_string())?;
            let restored = store.restore_cortex(&backup)?;
            println!(
                "{}",
                serde_json::json!({"restored": restored, "input": input})
            );
        }
        Cmd::ExportMd { dir } => {
            let store = open(&db)?;
            let n = store.export_md(std::path::Path::new(&dir));
            println!("{{\"exported\":{n},\"dir\":{dir:?}}}");
        }
        Cmd::Export { dir } => {
            let store = open(&db)?;
            let n = store.export_md(&dir);
            println!("{}", serde_json::json!({"exported":n,"dir":dir}));
        }
        Cmd::VaultExport {
            output,
            format,
            include_content,
        } => {
            let store = open(&db)?;
            let n = store.export_vault_review(&output, &format, include_content)?;
            println!(
                "{}",
                serde_json::json!({"schemaVersion":1,"exported":n,"output":output,"format":format,"contentIncluded":include_content})
            );
        }
        Cmd::Import { dir } => {
            let store = open(&db)?;
            let n = import_markdown_tree(&store, &dir)?;
            println!("{}", serde_json::json!({"imported":n,"dir":dir}));
        }
    }
    Ok(())
}

/// Commands that only read the store run beside a serving resident; every other command takes
/// the owner lock, so it cannot write underneath one.
fn command_takes_owner_lock(command: &Cmd) -> bool {
    !matches!(
        command,
        Cmd::List { .. }
            | Cmd::Explain { .. }
            | Cmd::Recall { .. }
            | Cmd::Replay { .. }
            | Cmd::Graph { .. }
            | Cmd::ReviewDue { .. }
            | Cmd::QuarantineList
            | Cmd::Hygiene { .. }
            | Cmd::LearningReceipt { .. }
            | Cmd::Export { .. }
            | Cmd::ExportMd { .. }
            | Cmd::VaultExport { .. }
            | Cmd::Backup { .. }
    )
}

pub fn run_cli() {
    let result = std::thread::Builder::new()
        .name("membrane-cli".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(run_main)
        .map_err(|error| format!("start Membrane CLI: {error}"))
        .and_then(|thread| {
            thread
                .join()
                .map_err(|_| "Membrane CLI panicked".to_string())
        })
        .and_then(|result| result);
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

/// MBR-102 entrypoint: run the CLI with an explicit argv. The Membrane binary forwards the
/// canonical `cli` subcommand tail here.
pub fn run_cli_from(argv: &[&str]) -> Result<(), String> {
    let argv: Vec<String> = argv.iter().map(|arg| arg.to_string()).collect();
    std::thread::Builder::new()
        .name("membrane-cli".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run_main_with_argv(argv))
        .map_err(|error| format!("start membrane CLI: {error}"))?
        .join()
        .map_err(|_| "membrane CLI panicked".to_string())
        .and_then(|result| result)
}

/// Cortex projection of CLI: durable-memory verbs only. Pull, Push, Ledger,
/// Blueprint, Adapt, & orchestration stay addressable through Membrane.
pub const CORTEX_DURABLE_COMMANDS: &[&str] = &[
    "backup",
    "checkpoint",
    "close-unknown",
    "curate",
    "delete",
    "drain-background-proposals",
    "erase",
    "explain",
    "export",
    "export-md",
    "feedback",
    "get",
    "graph",
    "hygiene",
    "import",
    "installation",
    "learning-qualify",
    "learning-receipt",
    "learning-register",
    "list",
    "put",
    "quarantine-list",
    "quarantine-restore",
    "recall",
    "replay",
    "reindex",
    "restore",
    "review-due",
    "ingest-skills",
    "skill-read",
    "vault-export",
];

/// Run Cortex's durable-memory CLI projection after rejecting every other
/// Membrane axis at its entry boundary.
pub fn run_cortex_durable_cli_from(argv: &[&str]) -> Result<(), String> {
    let mut index = 1;
    let command = loop {
        let Some(argument) = argv.get(index).copied() else {
            break None;
        };
        if argument == "--db" {
            index += 2;
            continue;
        }
        if argument.starts_with("--db=") {
            index += 1;
            continue;
        }
        break Some(argument);
    };
    let Some(command) = command else {
        return Err("durable-memory command required".into());
    };
    if !CORTEX_DURABLE_COMMANDS.contains(&command) {
        return Err(format!(
            "unsupported command `{command}`; use membrane for Pull, Push, Ledger, Blueprint, Adapt, & service orchestration"
        ));
    }
    run_cli_from(argv)
}

fn secret_value_looks_credible(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|character: char| matches!(character, '`' | '\'' | '"' | ',' | ';'));
    if value.is_empty()
        || value.starts_with(['<', '[', '{'])
        || [
            "option<",
            "vec<",
            "result<",
            "hashmap<",
            "btreemap<",
            "hashset<",
            "arc<",
            "box<",
            "cow<",
        ]
        .iter()
        .any(|type_prefix| value.starts_with(type_prefix))
        || [
            "placeholder",
            "redacted",
            "example",
            "your_api_key",
            "string",
            "secret",
            "token",
            "value",
        ]
        .contains(&value)
    {
        return false;
    }
    let compact = value
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect::<String>();
    compact.len() >= 6
        && compact
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && (compact.chars().any(|character| character.is_ascii_digit())
            || compact.starts_with("sk-")
            || compact.starts_with("ghp_"))
}
