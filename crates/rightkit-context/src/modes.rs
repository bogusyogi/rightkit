//! What each command does: ask the resident first, then run in process (A3, C4).
//!
//! The resident is `membrane serve`, started and owned by the host app. A command tries its endpoint
//! with a 100 ms connect budget; when nothing answers (`ClientError::NotRunning`) it runs the
//! same request in this process against the stores, which never loads the embedding model.
//! A query also falls back when the resident was reached but could not answer it. A write does
//! not: once a resident was reached the write may have run, so replaying it here could do it
//! twice.

use crate::cli::{
    AdaptAdjudicateTasteArgs, AdaptApplyArgs, AdaptMineSessionArgs, AdaptObserveArgs,
    AdaptRecallArgs, AdaptStatusArgs, AdaptTasteReviewArgs, BlueprintArgs, ConfirmArgs, DeleteArgs,
    HookArgs, InitArgs, MarkArgs, PullArgs, PushArgs, ReceiptsListArgs, SearchArgs, StatusArgs,
    TierArg, TranscriptAppendArgs, TranscriptTipArgs, WhereArgs, WireArg,
};
use crate::{
    SCHEMA_ADAPT_ADJUDICATE_TASTE, SCHEMA_ADAPT_APPLY, SCHEMA_ADAPT_MINE_SESSION,
    SCHEMA_ADAPT_OBSERVE, SCHEMA_ADAPT_RECALL, SCHEMA_ADAPT_STATUS, SCHEMA_ADAPT_TASTE_REVIEW,
    SCHEMA_BLUEPRINT, SCHEMA_BLUEPRINT_SEARCH, SCHEMA_CONTEXT_PULL, SCHEMA_MEMORY_DELETE,
    SCHEMA_MEMORY_GET, SCHEMA_MEMORY_INFO, SCHEMA_MEMORY_LIST, SCHEMA_MEMORY_PUSH,
    SCHEMA_MEMORY_SCOPES, SCHEMA_MEMORY_SEARCH, SCHEMA_RECEIPTS_MARK, SCHEMA_STATUS,
    SCHEMA_TRANSCRIPT_APPEND, SCHEMA_TRANSCRIPT_TIP, SCHEMA_WHERE,
};
use rightkit_context_client::{ClientError, ResidentClient};
use rightkit_context_protocol::session_transcript::{
    SessionTranscriptAppendV1, SessionTranscriptEventV1, SESSION_TRANSCRIPT_SCHEMA_VERSION,
};
use rightkit_context_protocol::ResidentOp;
use rightkit_context_runtime::cli::MEMORY_SEARCH_MAX_LIMIT;
use rightkit_context_runtime::hook::{self, Wire};
use rightkit_context_runtime::receipt::{self, HookReceiptState};
use serde_json::{json, Map, Value};
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Wall budget for a resident answer, per command. A hook shares its host's timeout, so it gets
/// only what a warm pull needs; the in-process run then has the rest.
const PULL_DEADLINE: Duration = Duration::from_secs(10);
const PUSH_DEADLINE: Duration = Duration::from_secs(5);
const BLUEPRINT_DEADLINE: Duration = Duration::from_secs(5);
/// How long `blueprint build` and `refresh` wait for the resident: the build's own limit
/// (`MAX_BUILD_DEADLINE_MS`, 300 s) plus a margin, so a normal build is not reported unavailable
/// while the resident is still writing it.
const BLUEPRINT_WRITE_DEADLINE: Duration = Duration::from_secs(330);
const STATUS_DEADLINE: Duration = Duration::from_secs(1);
const SEARCH_DEADLINE: Duration = Duration::from_secs(5);
const DELETE_DEADLINE: Duration = Duration::from_secs(5);
const HOOK_DEADLINE: Duration = Duration::from_millis(1_500);

/// The text a push may carry; the same bound the resident enforces.
const MAX_PUSH_BYTES: usize = 1024 * 1024;

/// A hook payload is a small JSON object. A host may keep stdin open for the life of its
/// session, so the read is bounded in time as well as size: the first bytes must arrive within
/// the grace, and the payload ends at EOF or after a short quiet interval once bytes flowed.
const MAX_HOOK_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const STDIN_FIRST_BYTES_GRACE: Duration = Duration::from_secs(2);
const STDIN_QUIET: Duration = Duration::from_millis(400);

#[derive(Debug)]
pub enum DispatchOutcome {
    Ok,
    UserError(String),
    /// The answer asked for cannot be produced (no store, no context, no graph): exit 3.
    NotAvailable(String),
    InternalError(String),
    /// The command already reported itself and decided the exit code (`serve`).
    Exit(i32),
}

/// How the resident answered.
enum Resident {
    Answered(Value),
    /// Nothing answered and nothing was sent.
    Absent,
    /// The resident was reached but did not produce an answer.
    Failed(ClientError),
}

fn ask(op: ResidentOp, args: Value, deadline: Duration) -> Resident {
    let state_root = rightkit_context_runtime::cli::state_root();
    let client = match ResidentClient::connect(&state_root) {
        Ok(client) => client,
        Err(ClientError::NotRunning { .. }) => return Resident::Absent,
        Err(error) => return Resident::Failed(error),
    };
    client.set_deadline(Some(deadline));
    match client.call(op, args) {
        Ok(data) => Resident::Answered(data),
        Err(ClientError::NotRunning { .. }) => Resident::Absent,
        Err(error) => Resident::Failed(error),
    }
}

fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|error| format!("resolve current directory: {error}"))
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(|error| format!("encode answer: {error}"))?
    );
    Ok(())
}

/// `membrane context pull`: print the context for a task, or nothing when there is none. With
/// `--wire` and `--request-id` the context is also recorded as a receipt the caller can later mark
/// delivered.
///
/// What is printed is the one projection `hook::project_pull_answer` makes of the answer in its
/// delivery mode (the bare context and a newline, or with `--json` the whole
/// `membrane.context.pull.v1` document, `receipt` included). The resident or the in-process pull
/// selected a representation whose complete projection fits the declared budget; here the answer
/// is re-projected and proved to be that response (`hook::verify_pull_projection`) before
/// anything is recorded or printed, and the approved bytes are printed as they are.
pub fn context_pull(args: &PullArgs) -> DispatchOutcome {
    if SCHEMA_CONTEXT_PULL != hook::PULL_DOCUMENT_SCHEMA {
        return DispatchOutcome::InternalError(format!(
            "context pull schema {SCHEMA_CONTEXT_PULL} is not the projected {}",
            hook::PULL_DOCUMENT_SCHEMA
        ));
    }
    let receipt_id = match (args.wire, args.request_id.as_deref()) {
        (None, Some(_)) => {
            return DispatchOutcome::UserError(
                "context pull: --request-id needs --wire coderight".into(),
            );
        }
        (Some(_), Some(id)) if !receipt::valid_receipt_id(id) => {
            return DispatchOutcome::UserError(
                "context pull: --request-id takes 1 to 128 letters, digits, '-' or '_'".into(),
            );
        }
        (Some(_), Some(id)) => Some(id.to_owned()),
        _ => None,
    };
    // `--cwd` and `--repo` name the same thing; both are accepted only when they are the same path.
    if let (Some(repo), Some(cwd)) = (&args.repo, &args.cwd) {
        let same = match (repo.canonicalize(), cwd.canonicalize()) {
            (Ok(repo), Ok(cwd)) => repo == cwd,
            _ => repo == cwd,
        };
        if !same {
            return DispatchOutcome::UserError(
                "context pull: --cwd and --repo name different paths; give one".into(),
            );
        }
    }
    let cwd = match args
        .repo
        .clone()
        .or_else(|| args.cwd.clone())
        .map(Ok)
        .unwrap_or_else(current_dir)
    {
        Ok(cwd) => cwd,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    let spec = hook::PullProjectionSpec {
        delivery: if args.json {
            hook::PullDelivery::Json
        } else {
            hook::PullDelivery::Text
        },
        receipt_id,
    };
    let budget = hook::effective_pull_budget(args.budget.map(|budget| budget as u64));
    let request = json!({
        "cwd": cwd,
        "prompt": args.prompt,
        "budget": args.budget,
        "client": "cli",
        "session": format!("cli-{}", std::process::id()),
        "delivery": spec.delivery.as_str(),
        "receiptId": spec.receipt_id,
    });
    let from_resident = match ask(ResidentOp::Pull, request.clone(), PULL_DEADLINE) {
        Resident::Answered(data) => hook::context_text(&data).map(|_| data),
        // An answered refusal is final: the in-process path never runs behind it, whether the
        // caller was denied or no response fits the declared budget.
        Resident::Failed(ClientError::Refused { code, message, .. })
            if code == rightkit_context_runtime::authorization::PULL_REFUSAL_CODE
                || message.starts_with(SELECTION_REFUSAL_PREFIX) =>
        {
            return DispatchOutcome::NotAvailable(message);
        }
        _ => None,
    };
    let data = match from_resident {
        Some(data) => data,
        None => match rightkit_context_runtime::cli::op_pull(&request) {
            Ok(data) => data,
            Err(error) => return DispatchOutcome::NotAvailable(error),
        },
    };
    let approved = match hook::verify_pull_projection(&data, &spec, budget) {
        Ok(approved) => approved,
        Err(error) => return DispatchOutcome::NotAvailable(error),
    };
    if let Some(id) = &spec.receipt_id {
        let emitted = hook::context_text(&data).flatten().is_some();
        if let Err(error) = record_pull_receipt(id, emitted) {
            return DispatchOutcome::InternalError(error);
        }
    }
    // The age of the graph travels beside the approved bytes, never inside them: the projection
    // above is proved token-exact against its budget, so a line added to it would break that
    // proof. stderr is what a calling agent's shell shows next to the context (#555).
    if let Some(line) = rightkit_context_runtime::graph_health::staleness_line(
        graph_status(&cwd).as_ref(),
        rightkit_context_runtime::graph_health::is_enrolled(&cwd),
    ) {
        use std::io::Write as _;
        let _ = writeln!(io::stderr(), "{line}");
    }
    print!("{approved}");
    DispatchOutcome::Ok
}

/// How long a pull waits for the Blueprint status that decides its staleness line: the resident's
/// answer, or the in-process read bounded by `--deadline-ms` below.
const GRAPH_STATUS_DEADLINE: Duration = Duration::from_millis(2_500);
const GRAPH_STATUS_READ_MS: &str = "1500";

/// The Blueprint `status` answer for `cwd`: from the resident; a bounded in-process read only when
/// no resident is running. `None` when the resident timed out or the read failed, which callers report as "not checked", never as fresh.
fn graph_status(cwd: &Path) -> Option<Value> {
    let query = vec![
        "status".to_owned(),
        "--deadline-ms".to_owned(),
        GRAPH_STATUS_READ_MS.to_owned(),
    ];
    match ask(
        ResidentOp::Blueprint,
        json!({"query": query, "cwd": cwd}),
        GRAPH_STATUS_DEADLINE,
    ) {
        Resident::Answered(data) => Some(data),
        // A resident that is running but did not answer in time is reported as "not checked":
        // a second, in-process read behind it would add its own 1.5 s plus SQLite's busy wait.
        Resident::Failed(_) => None,
        // No resident is running: the bounded in-process read is the only reader.
        Resident::Absent => {
            rightkit_context_runtime::cli::blueprint_query_value(&query, Some(cwd)).ok()
        }
    }
}

/// `membrane blueprint status --all`: one entry per enrolled root with its last build, freshness
/// state and its own watcher state, so one degraded root no longer hides the healthy ones behind
/// the aggregate `watcherReady` (#555).
fn blueprint_status_all() -> DispatchOutcome {
    let roots = match rightkit_context_runtime::graph_health::enrolled_roots() {
        Ok(roots) => roots,
        Err(error) => return DispatchOutcome::NotAvailable(error),
    };
    let resident = match ask(ResidentOp::Status, json!({}), STATUS_DEADLINE) {
        Resident::Answered(status) => Some(status),
        Resident::Absent | Resident::Failed(_) => None,
    };
    let entries: Vec<Value> = roots
        .iter()
        .map(|root| {
            let graph = graph_status(Path::new(root));
            rightkit_context_runtime::graph_health::root_status(
                root,
                graph.as_ref(),
                resident.as_ref(),
            )
        })
        .collect();
    let mut fields = Map::new();
    fields.insert("roots".into(), Value::Array(entries));
    fields.insert(
        "residentWatcher".into(),
        resident
            .as_ref()
            .and_then(|status| status.get("blueprint").cloned())
            .unwrap_or(Value::Null),
    );
    print_document(SCHEMA_BLUEPRINT, fields)
}

/// The reason prefix of a typed request-time refusal (no response fits the declared budget, a
/// stale or mismatched projection): an answered refusal, final like an authorization denial.
const SELECTION_REFUSAL_PREFIX: &str = "request_time_selection_refused:";

/// Record the context a caller will inject: `emitted` when there was context to deliver, which
/// writes the receipt, and `empty` when there was none, which writes nothing. The receipt the
/// document reports is part of the approved projection (`hook::pull_receipt_projection`).
fn record_pull_receipt(id: &str, emitted: bool) -> Result<(), String> {
    if emitted {
        let record =
            receipt::ContextReceipt::emitted(id, hook::PULL_RECEIPT_SESSION, "coderight", "pull");
        receipt::write_context_receipt(&rightkit_context_runtime::cli::state_root(), &record)
            .map_err(|error| format!("record receipt {id}: {error}"))?;
    }
    Ok(())
}

/// Print `fields` as one JSON object whose top-level `schema` is `schema`. A `schema` field the
/// answer already carried moves to `payload_schema` rather than being lost.
fn print_document(schema: &str, mut fields: Map<String, Value>) -> DispatchOutcome {
    if let Some(inner) = fields.remove("schema") {
        if inner != Value::String(schema.to_owned()) {
            fields.insert("payload_schema".into(), inner);
        }
    }
    let mut document = Map::new();
    document.insert("schema".into(), Value::String(schema.to_owned()));
    document.extend(fields);
    match print_json(&Value::Object(document)) {
        Ok(()) => DispatchOutcome::Ok,
        Err(error) => DispatchOutcome::InternalError(error),
    }
}

/// `membrane memory push`: store text as a durable memory.
pub fn memory_push(args: &PushArgs) -> DispatchOutcome {
    let text = match (&args.text, args.stdin) {
        (Some(text), false) => text.clone(),
        _ => {
            let mut bytes = Vec::new();
            if let Err(error) = io::stdin()
                .take((MAX_PUSH_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
            {
                return DispatchOutcome::UserError(format!("read stdin: {error}"));
            }
            if bytes.len() > MAX_PUSH_BYTES {
                return DispatchOutcome::UserError("text exceeds 1 MiB".into());
            }
            match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => return DispatchOutcome::UserError("stdin is not UTF-8 text".into()),
            }
        }
    };
    if text.trim().is_empty() {
        return DispatchOutcome::UserError("refusing to push empty text".into());
    }
    // The placement is checked here, before anything is sent: the scope is made canonical and the
    // name checked, so the resident and the in-process path store exactly what is answered.
    let scope = match args
        .scope
        .as_deref()
        .map(rightkit_context_runtime::cli::canonical_write_scope)
    {
        Some(Ok(scope)) => Some(scope),
        Some(Err(error)) => return DispatchOutcome::UserError(format!("memory push: {error}")),
        None => None,
    };
    let name = args.name.as_deref().map(str::trim);
    if let Some(name) = name {
        if let Err(error) = rightkit_context_runtime::cli::validate_memory_name(name) {
            return DispatchOutcome::UserError(format!("memory push: {error}"));
        }
    }
    let tier = args.tier.map(tier_name);
    let mut request = json!({"content": text});
    if let Some(name) = name {
        request["name"] = json!(name);
    }
    if let Some(scope) = &scope {
        request["scope"] = json!(scope);
    }
    if let Some(tier) = tier {
        request["tier"] = json!(tier);
    }
    if !args.keywords.is_empty() {
        request["keywords"] = json!(args.keywords);
    }
    let result = match ask(ResidentOp::Push, request.clone(), PUSH_DEADLINE) {
        Resident::Answered(data) => Ok(data),
        Resident::Absent => rightkit_context_runtime::cli::push_memory_request(&request),
        Resident::Failed(error) => Err(error.to_string()),
    };
    let data = match result {
        Ok(data) => data,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    // A resident started before this build stores `content` and ignores the placement. The write
    // has happened and is not repeated, so say what was stored instead of printing a false id.
    let stored_scope = data
        .get("scope_id")
        .or_else(|| data.get("scope"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let stored_tier = data
        .get("tier")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let id = data.get("id").and_then(Value::as_str).unwrap_or("");
    let expected_id = name.map(|name| format!("{}/{name}", scope.as_deref().unwrap_or("global")));
    let placed = scope.as_deref().is_none_or(|scope| scope == stored_scope)
        && tier.is_none_or(|tier| tier == stored_tier)
        && expected_id.as_deref().is_none_or(|expected| expected == id);
    if !placed {
        return DispatchOutcome::InternalError(format!(
            "memory push: the resident stored memory {id} in scope {stored_scope} without the requested name, scope or tier (it predates this option; restart the host app); remove it with `membrane memory delete {id}` and push again"
        ));
    }
    if !args.json {
        return match print_json(&data) {
            Ok(()) => DispatchOutcome::Ok,
            Err(error) => DispatchOutcome::UserError(error),
        };
    }
    let mut fields = Map::new();
    fields.insert("id".into(), json!(id));
    fields.insert("scope".into(), json!(stored_scope));
    fields.insert("tier".into(), json!(stored_tier));
    fields.insert("name".into(), name.map_or(Value::Null, |name| json!(name)));
    fields.insert(
        "keywords".into(),
        data.get("keywords").cloned().unwrap_or(json!([])),
    );
    print_document(SCHEMA_MEMORY_PUSH, fields)
}

/// `membrane memory <command>`: the durable-memory maintenance commands, in process.
/// `memory list --json` is answered here as one `membrane.memory.list.v1` document.
pub fn memory_durable(rest: &[String]) -> DispatchOutcome {
    if rest.first().map(String::as_str) == Some("list") {
        let names_repository = rest.iter().any(|arg| {
            matches!(
                arg.split('=').next(),
                Some("--repo" | "--repo-root" | "--cwd")
            )
        });
        let json = rest.iter().any(|arg| arg == "--json");
        if names_repository && !json {
            return DispatchOutcome::UserError(
                "memory list: --repo and --cwd print JSON; add --json".into(),
            );
        }
        let scope_flags = rest
            .iter()
            .filter(|arg| arg.split('=').next() == Some("--scope"))
            .count();
        if scope_flags > 1 && !json {
            return DispatchOutcome::UserError(
                "memory list: a scope chain (--scope more than once) prints JSON; add --json"
                    .into(),
            );
        }
        if json {
            return memory_list_json(&rest[1..]);
        }
    }
    let json = rest.iter().any(|arg| arg == "--json");
    match rest.first().map(String::as_str) {
        Some("get") if json => return memory_get_json(&rest[1..]),
        Some("scopes") => return memory_scopes(&rest[1..]),
        Some("info") => return memory_info(&rest[1..]),
        // `host memory history ...` reaches here as `memory history ...`.
        Some("history") => return history(&rest[1..]),
        _ => {}
    }
    let mut argv = vec!["membrane".to_owned()];
    argv.extend(rest.iter().cloned());
    let refs = argv.iter().map(String::as_str).collect::<Vec<_>>();
    match rightkit_context_runtime::cli::run_cortex_durable_cli_from(&refs) {
        Ok(()) => DispatchOutcome::Ok,
        Err(error) => DispatchOutcome::UserError(error),
    }
}

/// `membrane history search|status|erase`: the ChatGPT and Codex history lane (Ledger transcripts).
/// Runs in process against the stores; nothing is searched while the manifest is absent or off.
pub fn history(words: &[String]) -> DispatchOutcome {
    use rightkit_context_runtime::ledger::limits::WorkBudget;
    use rightkit_context_runtime::ledger::transcripts as tx;
    let value_of = |flag: &str| -> Result<Option<String>, String> {
        let inline = format!("{flag}=");
        let mut found = None;
        let mut iter = words.iter();
        while let Some(word) = iter.next() {
            if let Some(value) = word.strip_prefix(&inline) {
                found = Some(value.to_owned());
            } else if word == flag {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("history: {flag} needs a value"))?;
                found = Some(value.clone());
            }
        }
        Ok(found)
    };
    let to_user = |error: String| DispatchOutcome::NotAvailable(format!("history: {error}"));
    match words.first().map(String::as_str) {
        Some("search") => {
            // The text is every word that is not a flag or a flag's value.
            let mut text = Vec::new();
            let mut skip = false;
            for word in &words[1..] {
                if skip {
                    skip = false;
                } else if matches!(word.as_str(), "--scope" | "--limit") {
                    skip = true;
                } else if !word.starts_with("--") {
                    text.push(word.as_str());
                }
            }
            if text.is_empty() {
                return DispatchOutcome::UserError("history search requires text".into());
            }
            let limit = match value_of("--limit") {
                Ok(Some(raw)) => match raw.parse::<usize>() {
                    Ok(limit) if limit >= 1 => limit,
                    _ => {
                        return DispatchOutcome::UserError(
                            "history: --limit must be a positive number".into(),
                        )
                    }
                },
                Ok(None) => 10,
                Err(error) => return DispatchOutcome::UserError(error),
            };
            let scopes = match value_of("--scope") {
                // An explicit scope is only a narrowing of what the manifest enrols: the search
                // owner checks it against the current manifest and refuses one outside it.
                Ok(Some(scope)) => vec![scope],
                Ok(None) => match tx::load_manifest(&tx::manifest_path()) {
                    Ok(Some(manifest)) if manifest.enabled => {
                        let mut scopes: Vec<String> =
                            manifest.sources.iter().map(|s| s.scope.clone()).collect();
                        scopes.sort();
                        scopes.dedup();
                        scopes
                    }
                    Ok(_) => {
                        return DispatchOutcome::NotAvailable(
                            "history: off; turn on \"Search ChatGPT and Codex history\" in the host app settings".into(),
                        )
                    }
                    Err(error) => return to_user(error),
                },
                Err(error) => return DispatchOutcome::UserError(error),
            };
            let query = tx::TxQuery {
                scopes,
                text: text.join(" "),
                limit,
                provider: None,
                role: None,
                since_ms: None,
                until_ms: None,
                current_path_only: true,
            };
            match tx::search_default(&query, &WorkBudget::bounded(Duration::from_secs(10))) {
                Ok(result) => match serde_json::to_value(&result) {
                    Ok(Value::Object(fields)) => print_document("membrane.history.search.v1", fields),
                    Ok(_) => DispatchOutcome::InternalError("history: unexpected result shape".into()),
                    Err(error) => DispatchOutcome::InternalError(error.to_string()),
                },
                Err(error) => to_user(error),
            }
        }
        Some("status") => match tx::status_default() {
            Ok(Value::Object(fields)) => print_document("membrane.history.status.v1", fields),
            Ok(other) => {
                let mut fields = Map::new();
                fields.insert("status".into(), other);
                print_document("membrane.history.status.v1", fields)
            }
            Err(error) => to_user(error),
        },
        Some("erase") => {
            let conversation = value_of("--conversation");
            let source = value_of("--source");
            match (conversation, source) {
                (Ok(Some(id)), Ok(None)) => match tx::erase_conversation_default(&id) {
                    Ok(removed) => {
                        let mut fields = Map::new();
                        fields.insert("conversation".into(), Value::String(id));
                        fields.insert("removed".into(), json!(removed));
                        print_document("membrane.history.erase.v1", fields)
                    }
                    Err(error) => to_user(error),
                },
                (Ok(None), Ok(Some(id))) => match tx::erase_source_default(&id) {
                    Ok(()) => {
                        let mut fields = Map::new();
                        fields.insert("source".into(), Value::String(id));
                        fields.insert("erased".into(), Value::Bool(true));
                        print_document("membrane.history.erase.v1", fields)
                    }
                    Err(error) => to_user(error),
                },
                (Err(error), _) | (_, Err(error)) => DispatchOutcome::UserError(error),
                _ => DispatchOutcome::UserError(
                    "history erase needs exactly one of --conversation ID or --source ID".into(),
                ),
            }
        }
        _ => DispatchOutcome::UserError(
            "history: use search <text> [--scope S] [--limit N], status, or erase --conversation ID | --source ID".into(),
        ),
    }
}

/// Take `--repo <path>` (or `--repo-root <path>`, or either joined with `=`) out of a blueprint
/// query. What is left is the query. The flag may stand anywhere in the words.
fn split_repo_flag(words: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut repo = None;
    let mut rest = Vec::new();
    let mut iter = words.iter();
    while let Some(word) = iter.next() {
        let inline = word
            .strip_prefix("--repo=")
            .or_else(|| word.strip_prefix("--repo-root="));
        if let Some(value) = inline {
            repo = Some(PathBuf::from(value));
        } else if word == "--repo" || word == "--repo-root" {
            let value = iter
                .next()
                .ok_or_else(|| format!("blueprint: {word} needs a path"))?;
            repo = Some(PathBuf::from(value));
        } else {
            rest.push(word.clone());
        }
    }
    if rest.is_empty() {
        return Err("blueprint requires a query".into());
    }
    Ok((repo, rest))
}

/// `membrane blueprint <query…> [--repo <path>]`: read verbs answer from the resident, else in
/// process. `build` and `refresh` write: resident answers, or with no resident the CLI builds under
/// the owner lock, or a typed unavailable error. `--repo` names the repository without a chdir; without it, the current directory.
pub fn blueprint(args: &BlueprintArgs) -> DispatchOutcome {
    let (repo, query) = match split_repo_flag(&args.query) {
        Ok(split) => split,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    if query == ["status", "--all"] {
        return blueprint_status_all();
    }
    let cwd = match repo.map(Ok).unwrap_or_else(current_dir) {
        Ok(cwd) => cwd,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    // `blueprint search <text>` has its own schema id; the other verbs keep the Blueprint id.
    let schema = if query.first().map(String::as_str) == Some("search") {
        SCHEMA_BLUEPRINT_SEARCH
    } else {
        SCHEMA_BLUEPRINT
    };
    let request = json!({"query": query, "cwd": cwd});
    let deadline = if rightkit_context_runtime::cli::blueprint_verb_writes(&query) {
        BLUEPRINT_WRITE_DEADLINE
    } else {
        BLUEPRINT_DEADLINE
    };
    let data = match ask(ResidentOp::Blueprint, request, deadline) {
        Resident::Answered(data) => Ok(data),
        // `build` and `refresh` write the graph. Only an absent resident lets the CLI write, and
        // then under the owner lock; a resident that timed out or failed may still be writing, so
        // a second writer is never started (INV-008).
        Resident::Absent if rightkit_context_runtime::cli::blueprint_verb_writes(&query) => {
            rightkit_context_runtime::cli::blueprint_write_one_shot(&query, Some(&cwd))
        }
        Resident::Failed(error) if rightkit_context_runtime::cli::blueprint_verb_writes(&query) => Err(
            format!("membrane_resident_unavailable: the resident did not answer ({error}); not writing a second graph"),
        ),
        Resident::Absent | Resident::Failed(_) => {
            rightkit_context_runtime::cli::blueprint_query_value(&query, Some(&cwd))
        }
    };
    match data {
        Ok(Value::Object(fields)) => print_document(schema, fields),
        Ok(other) => {
            let mut fields = Map::new();
            fields.insert("result".into(), other);
            print_document(schema, fields)
        }
        Err(error) => {
            let usage = error.starts_with("unsupported")
                || error.starts_with("--")
                || error.contains(" requires ");
            if usage {
                DispatchOutcome::UserError(error)
            } else {
                DispatchOutcome::NotAvailable(error)
            }
        }
    }
}

/// `membrane memory list [--scope <id>]... [--limit <n>] [--db <path>] --json`. Repeating
/// `--scope` lists a scope chain, in the order given.
fn memory_list_json(flags: &[String]) -> DispatchOutcome {
    let mut scopes: Vec<String> = Vec::new();
    let mut limit: usize = 1_000;
    let mut db: Option<String> = None;
    let mut repo: Option<PathBuf> = None;
    let mut index = 0;
    while index < flags.len() {
        let flag = flags[index].as_str();
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if name == "--json" && inline.is_none() {
            index += 1;
            continue;
        }
        if !matches!(
            name,
            "--scope" | "--limit" | "--db" | "--repo" | "--repo-root" | "--cwd"
        ) {
            return DispatchOutcome::UserError(format!("memory list: unknown argument `{flag}`"));
        }
        let value = match inline {
            Some(value) => value,
            None => {
                index += 1;
                match flags.get(index) {
                    Some(value) => value.clone(),
                    None => {
                        return DispatchOutcome::UserError(format!(
                            "memory list: {name} needs a value"
                        ));
                    }
                }
            }
        };
        match name {
            "--scope" => scopes.push(value),
            "--db" => db = Some(value),
            "--repo" | "--repo-root" | "--cwd" => {
                if repo.is_some() {
                    return DispatchOutcome::UserError(
                        "memory list: give the repository once".into(),
                    );
                }
                repo = Some(PathBuf::from(value));
            }
            _ => match value.parse::<usize>() {
                Ok(parsed) if parsed > 0 => limit = parsed,
                _ => {
                    return DispatchOutcome::UserError(
                        "memory list: --limit must be a positive integer".into(),
                    );
                }
            },
        }
        index += 1;
    }
    if repo.is_some() && !scopes.is_empty() {
        return DispatchOutcome::UserError(
            "memory list: --scope and --repo both name the scope; give one".into(),
        );
    }
    if repo.is_some() {
        scopes = repository_scope(repo.as_deref(), None)
            .into_iter()
            .collect();
    }
    let path = memory_db_path(db.as_deref().map(Path::new));
    if !path.is_file() {
        return DispatchOutcome::NotAvailable(no_store_message(&path));
    }
    let answer =
        match rightkit_context_runtime::cli::list_memories_in_process(&path, &scopes, limit) {
            Ok(answer) => answer,
            Err(error) => return DispatchOutcome::InternalError(format!("list memories: {error}")),
        };
    let Value::Object(mut document) = answer else {
        return DispatchOutcome::InternalError("list memories: the answer is not an object".into());
    };
    // `scope` is kept for callers of the single-scope form: the scope listed, else null.
    let single = match document
        .get("scopes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        Some([only]) => only.clone(),
        _ => Value::Null,
    };
    document.insert("scope".into(), single);
    print_document(SCHEMA_MEMORY_LIST, document)
}

/// The flags of a `memory get|scopes|info` command line: its positional words, whether `--json`
/// was given, and the value of each flag in `valued` (`--flag value` or `--flag=value`).
type MemoryFlags = (Vec<String>, bool, Vec<(String, String)>);

fn memory_flags(command: &str, flags: &[String], valued: &[&str]) -> Result<MemoryFlags, String> {
    let mut positional = Vec::new();
    let mut json = false;
    let mut values: Vec<(String, String)> = Vec::new();
    let mut index = 0;
    while index < flags.len() {
        let flag = flags[index].as_str();
        index += 1;
        if flag == "--json" {
            json = true;
            continue;
        }
        if !flag.starts_with("--") {
            positional.push(flag.to_owned());
            continue;
        }
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if !valued.contains(&name) {
            return Err(format!("memory {command}: unknown argument `{flag}`"));
        }
        let value = match inline {
            Some(value) => value,
            None => match flags.get(index) {
                Some(value) => {
                    index += 1;
                    value.clone()
                }
                None => return Err(format!("memory {command}: {name} needs a value")),
            },
        };
        values.push((name.to_owned(), value));
    }
    Ok((positional, json, values))
}

fn flag_value<'a>(values: &'a [(String, String)], name: &str) -> Option<&'a str> {
    values
        .iter()
        .rev()
        .find(|(flag, _)| flag == name)
        .map(|(_, value)| value.as_str())
}

/// The memory store a read command opens, or the outcome that says why it cannot.
fn memory_read_store(values: &[(String, String)]) -> Result<PathBuf, DispatchOutcome> {
    let path = memory_db_path(flag_value(values, "--db").map(Path::new));
    if path.is_file() {
        Ok(path)
    } else {
        Err(DispatchOutcome::NotAvailable(no_store_message(&path)))
    }
}

/// `membrane memory get <id> --json [--db <path>]`: one memory with its full content as a
/// `membrane.memory.get.v1` document. A read that records no use. A missing id is exit 3.
fn memory_get_json(flags: &[String]) -> DispatchOutcome {
    let (positional, _, values) = match memory_flags("get", flags, &["--db"]) {
        Ok(parsed) => parsed,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    let [id] = positional.as_slice() else {
        return DispatchOutcome::UserError("memory get: give exactly one memory id".into());
    };
    let path = match memory_read_store(&values) {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    match rightkit_context_runtime::cli::get_memory_in_process(&path, id) {
        Ok(Some(Value::Object(mut fields))) => {
            fields.insert("found".into(), Value::Bool(true));
            print_document(SCHEMA_MEMORY_GET, fields)
        }
        Ok(_) => {
            let mut fields = Map::new();
            fields.insert("id".into(), json!(id));
            fields.insert("found".into(), Value::Bool(false));
            if let DispatchOutcome::InternalError(error) = print_document(SCHEMA_MEMORY_GET, fields)
            {
                return DispatchOutcome::InternalError(error);
            }
            eprintln!("membrane: no memory has id {id}");
            DispatchOutcome::Exit(crate::EXIT_NOT_AVAILABLE)
        }
        Err(error) => DispatchOutcome::InternalError(format!("memory get: {error}")),
    }
}

/// `membrane memory scopes [--json] [--db <path>]`: every scope that holds a memory, with its
/// count.
fn memory_scopes(flags: &[String]) -> DispatchOutcome {
    let (positional, json, values) = match memory_flags("scopes", flags, &["--db"]) {
        Ok(parsed) => parsed,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    if !positional.is_empty() {
        return DispatchOutcome::UserError("memory scopes: takes no arguments".into());
    }
    let path = match memory_read_store(&values) {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let answer = match rightkit_context_runtime::cli::memory_scopes_in_process(&path) {
        Ok(answer) => answer,
        Err(error) => return DispatchOutcome::InternalError(format!("memory scopes: {error}")),
    };
    if json {
        let Value::Object(fields) = answer else {
            return DispatchOutcome::InternalError(
                "memory scopes: the answer is not an object".into(),
            );
        };
        return print_document(SCHEMA_MEMORY_SCOPES, fields);
    }
    for entry in answer
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "{} {}",
            entry.get("count").and_then(Value::as_i64).unwrap_or(0),
            str_field(entry, "scope")
        );
    }
    DispatchOutcome::Ok
}

/// `membrane memory info [--limit <n>] [--json] [--db <path>]`: what the store holds, the recent
/// recall activity, and the embedder the resident runs (`embedder.dim`, null while no resident
/// answers).
fn memory_info(flags: &[String]) -> DispatchOutcome {
    let (positional, json, values) = match memory_flags("info", flags, &["--db", "--limit"]) {
        Ok(parsed) => parsed,
        Err(error) => return DispatchOutcome::UserError(error),
    };
    if !positional.is_empty() {
        return DispatchOutcome::UserError("memory info: takes no arguments".into());
    }
    let limit = match flag_value(&values, "--limit").map(str::parse::<usize>) {
        None => 20,
        Some(Ok(parsed)) if (1..=1_000).contains(&parsed) => parsed,
        Some(_) => {
            return DispatchOutcome::UserError("memory info: --limit must be 1 to 1000".into());
        }
    };
    let path = match memory_read_store(&values) {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let answer = match rightkit_context_runtime::cli::memory_info_in_process(&path, limit) {
        Ok(answer) => answer,
        Err(error) => return DispatchOutcome::InternalError(format!("memory info: {error}")),
    };
    let Value::Object(mut fields) = answer else {
        return DispatchOutcome::InternalError("memory info: the answer is not an object".into());
    };
    // The live embedder belongs to the resident; a named store is not the resident's.
    let resident = if flag_value(&values, "--db").is_none() {
        ask(ResidentOp::Status, json!({}), STATUS_DEADLINE)
    } else {
        Resident::Absent
    };
    let embedder = match resident {
        Resident::Answered(status) => json!({
            "source": "resident",
            "dim": status.get("embedderDim").cloned().unwrap_or(Value::Null),
            "model": status.get("model").cloned().unwrap_or(Value::Null),
        }),
        _ => json!({"source": "unavailable", "dim": null, "model": null}),
    };
    fields.insert("embedder".into(), embedder);
    if json {
        return print_document(SCHEMA_MEMORY_INFO, fields);
    }
    let info = Value::Object(fields);
    println!(
        "memories: {} in {} scope(s)",
        info.get("total").and_then(Value::as_i64).unwrap_or(0),
        info.get("scopeCount").and_then(Value::as_u64).unwrap_or(0)
    );
    println!(
        "embedder dim: {}",
        info.pointer("/embedder/dim")
            .filter(|dim| !dim.is_null())
            .map_or_else(|| "unknown (no resident)".to_owned(), Value::to_string)
    );
    DispatchOutcome::Ok
}

/// The Cortex store a memory command reads: `--db`, else the one Cortex store path (the state root's store; debug builds also honor `CORTEX_DB`).
fn memory_db_path(flag: Option<&Path>) -> PathBuf {
    flag.map(Path::to_path_buf)
        .unwrap_or_else(rightkit_context_runtime::cli::cortex_db_path)
}

fn no_store_message(path: &Path) -> String {
    format!(
        "no memory store at {} (it is created by the first `memory push`)",
        path.display()
    )
}

/// The memory scope of the repository named by `--repo` or `--cwd`: the path slug of its root,
/// which is the scope a context pull for that root reads. `None` when neither is given.
fn repository_scope(repo: Option<&Path>, cwd: Option<&Path>) -> Option<String> {
    let path = repo.or(cwd)?;
    let root = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Some(rightkit_context_runtime::path_to_scope(
        &root.to_string_lossy(),
    ))
}

fn str_field<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

fn tier_name(tier: TierArg) -> &'static str {
    match tier {
        TierArg::Working => "working",
        TierArg::Episodic => "episodic",
        TierArg::Semantic => "semantic",
    }
}

/// `membrane memory search <query> [--tier T…] [--limit N] [--cwd|--repo] [--json]`. The resident
/// answers when one runs, else the store is read in process with the lexical arm, so the model is
/// never loaded.
pub fn memory_search(args: &SearchArgs) -> DispatchOutcome {
    if args.query.trim().is_empty() {
        return DispatchOutcome::UserError("memory search: the query is empty".into());
    }
    if args.limit == 0 || args.limit > MEMORY_SEARCH_MAX_LIMIT {
        return DispatchOutcome::UserError(format!(
            "memory search: --limit must be 1 to {MEMORY_SEARCH_MAX_LIMIT}"
        ));
    }
    let request = json!({
        "query": args.query,
        "tiers": args.tiers.iter().map(|tier| tier_name(*tier)).collect::<Vec<_>>(),
        "scope": repository_scope(args.repo.as_deref(), args.cwd.as_deref()),
        "scopes": args.scopes,
        "limit": args.limit,
    });
    // A named store is not the resident's, so only the default store asks the resident.
    let resident = if args.db.is_none() {
        ask(ResidentOp::MemorySearch, request.clone(), SEARCH_DEADLINE)
    } else {
        Resident::Absent
    };
    let answer = match resident {
        // A resident started before scope chains ignores `scopes` and would search every scope;
        // its answer lacks the `scopes` echo, so it is not used.
        Resident::Answered(data) if args.scopes.is_empty() || data.get("scopes").is_some() => data,
        // A read has no side effect, so a resident that failed is simply asked again in process.
        Resident::Answered(_) | Resident::Absent | Resident::Failed(_) => {
            let path = memory_db_path(args.db.as_deref());
            if !path.is_file() {
                return DispatchOutcome::NotAvailable(no_store_message(&path));
            }
            match rightkit_context_runtime::cli::search_memory_in_process(&path, &request) {
                Ok(answer) => answer,
                Err(error) => {
                    return DispatchOutcome::InternalError(format!("memory search: {error}"));
                }
            }
        }
    };
    let Value::Object(fields) = answer else {
        return DispatchOutcome::InternalError("memory search: the answer is not an object".into());
    };
    if args.json {
        return print_document(SCHEMA_MEMORY_SEARCH, fields);
    }
    let more = fields.get("more").and_then(Value::as_bool).unwrap_or(false);
    for item in fields
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "{} {} {}",
            str_field(item, "id"),
            str_field(item, "tier"),
            str_field(item, "scope")
        );
        println!("{}", str_field(item, "content"));
        println!();
    }
    if more {
        eprintln!(
            "membrane: more memories match; raise --limit (at most {MEMORY_SEARCH_MAX_LIMIT})"
        );
    }
    DispatchOutcome::Ok
}

/// `membrane memory delete <id> [--json]`. The resident deletes when one runs, so the store is
/// never written under it. A missing id is exit 3, with the document saying `deleted: false`.
pub fn memory_delete(args: &DeleteArgs) -> DispatchOutcome {
    if args.id.trim().is_empty() {
        return DispatchOutcome::UserError("memory delete: the id is empty".into());
    }
    let request = json!({"id": args.id});
    let resident = if args.db.is_none() {
        ask(ResidentOp::MemoryDelete, request.clone(), DELETE_DEADLINE)
    } else {
        Resident::Absent
    };
    let answer = match resident {
        Resident::Answered(data) => data,
        // A write the resident may have run is never replayed here, as with `memory push`.
        Resident::Failed(error) => {
            return DispatchOutcome::InternalError(format!(
                "memory delete {}: the resident failed ({error}), so the memory may still exist; check with `membrane memory search`",
                args.id
            ));
        }
        Resident::Absent => {
            let path = memory_db_path(args.db.as_deref());
            if !path.is_file() {
                return DispatchOutcome::NotAvailable(no_store_message(&path));
            }
            match rightkit_context_runtime::cli::delete_memory_in_process(&path, &request) {
                Ok(answer) => answer,
                Err(error) if error.starts_with("membrane is serving") => {
                    return DispatchOutcome::NotAvailable(error);
                }
                Err(error) => {
                    return DispatchOutcome::InternalError(format!("memory delete: {error}"));
                }
            }
        }
    };
    let deleted = answer
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if args.json {
        let Value::Object(fields) = answer else {
            return DispatchOutcome::InternalError(
                "memory delete: the answer is not an object".into(),
            );
        };
        if let DispatchOutcome::InternalError(error) = print_document(SCHEMA_MEMORY_DELETE, fields)
        {
            return DispatchOutcome::InternalError(error);
        }
    } else if deleted {
        println!("deleted {}", args.id);
    }
    if deleted {
        DispatchOutcome::Ok
    } else {
        eprintln!("membrane: no memory has id {}", args.id);
        DispatchOutcome::Exit(crate::EXIT_NOT_AVAILABLE)
    }
}

/// Deadline for an Adapt or transcript answer: a mine or an admission can take a while.
const ADAPT_DEADLINE: Duration = Duration::from_secs(30);
/// The bound on a JSON document an Adapt or transcript command reads, the same as the route's.
const MAX_ADAPT_BYTES: usize = 1024 * 1024;

/// An absolute path. The resident runs in another working directory, so a path it must read is
/// made absolute here first.
fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(current_dir()?.join(path))
}

/// The text of a JSON document named by `--stdin` or `--input`, which is exactly one of the two.
fn read_document(stdin: bool, input: Option<&Path>) -> Result<String, String> {
    let mut bytes = Vec::new();
    match (stdin, input) {
        (true, None) => {
            io::stdin()
                .take((MAX_ADAPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read stdin: {error}"))?;
        }
        (false, Some(path)) => {
            let file = std::fs::File::open(path)
                .map_err(|error| format!("open {}: {error}", path.display()))?;
            file.take((MAX_ADAPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
        }
        _ => return Err("give exactly one of --stdin or --input".into()),
    }
    if bytes.len() > MAX_ADAPT_BYTES {
        return Err("document exceeds 1 MiB".into());
    }
    String::from_utf8(bytes).map_err(|_| "document is not UTF-8 text".to_owned())
}

/// A route body that was text as a JSON value; text that is not JSON stays a string.
fn body_value(text: String) -> Value {
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(_) => Value::String(text),
    }
}

/// The resident's `{status, body}` answer as a status and a body.
fn envelope(data: Value) -> Result<(u16, Value), String> {
    let status = data
        .get("status")
        .and_then(Value::as_u64)
        .ok_or_else(|| "the resident's answer has no status".to_owned())?;
    let status =
        u16::try_from(status).map_err(|_| "the resident's status is out of range".to_owned())?;
    Ok((status, data.get("body").cloned().unwrap_or(Value::Null)))
}

/// Answer one Adapt or transcript body. The resident answers when one runs and the db is the
/// default. Otherwise the store is opened here: a read lexically, a write under the owner lock. A
/// write the resident was reached for is never replayed here, as with `memory push`.
fn adapt_answer(
    op: ResidentOp,
    body: Value,
    write: bool,
    db: Option<&Path>,
) -> Result<(u16, Value), DispatchOutcome> {
    let resident = if db.is_none() {
        ask(op, body.clone(), ADAPT_DEADLINE)
    } else {
        Resident::Absent
    };
    match resident {
        Resident::Answered(data) => {
            return envelope(data).map_err(DispatchOutcome::InternalError);
        }
        Resident::Failed(error) if write => {
            return Err(DispatchOutcome::InternalError(format!(
                "the resident failed ({error}); the write may have run, so it is not repeated here"
            )));
        }
        Resident::Failed(_) | Resident::Absent => {}
    }
    let path = memory_db_path(db);
    if !path.is_file() {
        return Err(DispatchOutcome::NotAvailable(no_store_message(&path)));
    }
    let text = body.to_string();
    let answered = match op {
        ResidentOp::AdaptObservation => {
            rightkit_context_runtime::cli::adapt_observation_in_process(&path, write, &text)
        }
        ResidentOp::TranscriptTip => {
            rightkit_context_runtime::cli::transcript_tip_in_process(&path, &text)
        }
        _ => rightkit_context_runtime::cli::adapt_operator_in_process(&path, write, &text),
    };
    match answered {
        Ok((status, text)) => Ok((status, body_value(text))),
        Err(error) if error.starts_with("membrane is serving") => {
            Err(DispatchOutcome::NotAvailable(error))
        }
        Err(error) => Err(DispatchOutcome::InternalError(error)),
    }
}

/// Print an Adapt answer: the `result` under the verb's schema with `--json`, else the result
/// alone. A refusal (any status but 200) is exit 2 with its reason on stderr.
fn adapt_outcome(
    schema: &str,
    status: u16,
    result: Value,
    json: bool,
    verb: &str,
) -> DispatchOutcome {
    if status == 200 {
        if !json {
            return match print_json(&result) {
                Ok(()) => DispatchOutcome::Ok,
                Err(error) => DispatchOutcome::InternalError(error),
            };
        }
        let mut fields = Map::new();
        fields.insert("result".into(), result);
        return print_document(schema, fields);
    }
    let detail = result
        .get("detail")
        .or_else(|| result.get("error"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("refused with status {status}"));
    DispatchOutcome::UserError(format!("{verb}: {detail}"))
}

/// `membrane adapt observe --stdin|--input <file> [--json]`: one observation document. The
/// transcript and observation routes answer it; it is a write.
pub fn adapt_observe(args: &AdaptObserveArgs) -> DispatchOutcome {
    let text = match read_document(args.stdin, args.input.as_deref()) {
        Ok(text) => text,
        Err(error) => return DispatchOutcome::UserError(format!("adapt observe: {error}")),
    };
    let body = match serde_json::from_str::<Value>(&text) {
        Ok(body) => body,
        Err(error) => {
            return DispatchOutcome::UserError(format!("adapt observe: not JSON: {error}"));
        }
    };
    match adapt_answer(ResidentOp::AdaptObservation, body, true, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_OBSERVE,
            status,
            result,
            args.json,
            "adapt observe",
        ),
        Err(outcome) => outcome,
    }
}

/// `membrane adapt recall <query> [-k n] [--scope s] [--dimension K=V]… [--json]`: the Taste
/// records that apply. A read.
pub fn adapt_recall(args: &AdaptRecallArgs) -> DispatchOutcome {
    if args.query.trim().is_empty() {
        return DispatchOutcome::UserError("adapt recall: the query is empty".into());
    }
    let body = json!({
        "command": "recall",
        "query": args.query,
        "k": args.k,
        "scope": args.scope,
        "dimensions": args.dimensions,
    });
    match adapt_answer(ResidentOp::AdaptOperator, body, false, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_RECALL,
            status,
            result,
            args.json,
            "adapt recall",
        ),
        Err(outcome) => outcome,
    }
}

/// `membrane adapt apply (--manifest <file> | --insights <file> --installation-id <id>)
/// [--dry-run] [--json]`: admit a reviewed manifest, or sealed Insight issues. A write.
pub fn adapt_apply(args: &AdaptApplyArgs) -> DispatchOutcome {
    let body = match (&args.manifest, &args.insights) {
        (Some(manifest), None) => {
            let manifest = match absolute_path(manifest) {
                Ok(path) => path,
                Err(error) => return DispatchOutcome::UserError(format!("adapt apply: {error}")),
            };
            json!({"command": "apply", "manifest": manifest, "dry_run": args.dry_run})
        }
        (None, Some(insights)) => {
            let Some(installation_id) = args.installation_id.as_deref() else {
                return DispatchOutcome::UserError(
                    "adapt apply: --insights needs --installation-id".into(),
                );
            };
            let input = match absolute_path(insights) {
                Ok(path) => path,
                Err(error) => return DispatchOutcome::UserError(format!("adapt apply: {error}")),
            };
            json!({
                "command": "apply_insights",
                "input": input,
                "installation_id": installation_id,
                "dry_run": args.dry_run,
            })
        }
        _ => {
            return DispatchOutcome::UserError(
                "adapt apply: give exactly one of --manifest or --insights".into(),
            );
        }
    };
    match adapt_answer(ResidentOp::AdaptOperator, body, true, args.db.as_deref()) {
        Ok((status, result)) => {
            adapt_outcome(SCHEMA_ADAPT_APPLY, status, result, args.json, "adapt apply")
        }
        Err(outcome) => outcome,
    }
}

/// `membrane adapt taste-review --input <file> --installation-id <id> --created-at <t> [--json]`:
/// build a pending Taste manifest from native mine output. It opens the store for reading in
/// full, so it runs under the owner lock when no resident answers.
pub fn adapt_taste_review(args: &AdaptTasteReviewArgs) -> DispatchOutcome {
    let input = match absolute_path(&args.input) {
        Ok(path) => path,
        Err(error) => return DispatchOutcome::UserError(format!("adapt taste-review: {error}")),
    };
    let body = json!({
        "command": "review_taste",
        "input": input,
        "installation_id": args.installation_id,
        "canonical_pool_sha256": args.canonical_pool_sha256,
        "created_at": args.created_at,
    });
    match adapt_answer(ResidentOp::AdaptOperator, body, true, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_TASTE_REVIEW,
            status,
            result,
            args.json,
            "adapt taste-review",
        ),
        Err(outcome) => outcome,
    }
}

/// `membrane adapt status [--scope s] [--json]`: live Adapt progress for a scope. A read.
pub fn adapt_status(args: &AdaptStatusArgs) -> DispatchOutcome {
    if args.scope.trim().is_empty() {
        return DispatchOutcome::UserError("adapt status: --scope is empty".into());
    }
    let body = json!({"command": "status", "scope": args.scope});
    match adapt_answer(ResidentOp::AdaptOperator, body, false, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_STATUS,
            status,
            result,
            args.json,
            "adapt status",
        ),
        Err(outcome) => outcome,
    }
}

/// `membrane adapt adjudicate-taste --manifest <file> --decisions <file> --validated-at <t>
/// [--json]`: bind semantic decisions to a pending Taste manifest and print the adjudicated
/// manifest. It reads the two files and writes nothing; `adapt apply` admits the result.
pub fn adapt_adjudicate_taste(args: &AdaptAdjudicateTasteArgs) -> DispatchOutcome {
    let manifest = match absolute_path(&args.manifest) {
        Ok(path) => path,
        Err(error) => {
            return DispatchOutcome::UserError(format!("adapt adjudicate-taste: {error}"));
        }
    };
    let decisions = match absolute_path(&args.decisions) {
        Ok(path) => path,
        Err(error) => {
            return DispatchOutcome::UserError(format!("adapt adjudicate-taste: {error}"));
        }
    };
    let body = json!({
        "command": "adjudicate_taste",
        "manifest": manifest,
        "decisions": decisions,
        "validated_at": args.validated_at,
    });
    match adapt_answer(ResidentOp::AdaptOperator, body, false, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_ADJUDICATE_TASTE,
            status,
            result,
            args.json,
            "adapt adjudicate-taste",
        ),
        Err(outcome) => outcome,
    }
}

/// `membrane adapt mine-session --session <id>... [--scope s] [--min-recurrence n] [--json]`:
/// mine the committed events of the named sessions (`transcript append` rows) for Taste and
/// Insight evidence. Report only: nothing is admitted. A read.
pub fn adapt_mine_session(args: &AdaptMineSessionArgs) -> DispatchOutcome {
    if args.session_ids.iter().any(|id| id.trim().is_empty()) {
        return DispatchOutcome::UserError("adapt mine-session: a --session is empty".into());
    }
    let body = json!({
        "command": "mine_session",
        "session_ids": args.session_ids,
        "scope": args.scope,
        "min_recurrence": args.min_recurrence,
    });
    match adapt_answer(ResidentOp::AdaptOperator, body, false, args.db.as_deref()) {
        Ok((status, result)) => adapt_outcome(
            SCHEMA_ADAPT_MINE_SESSION,
            status,
            result,
            args.json,
            "adapt mine-session",
        ),
        Err(outcome) => outcome,
    }
}

/// The stream's committed end, from the tip op. `None` when the tip cannot be read.
fn transcript_end(session: &str, db: Option<&Path>) -> Option<u64> {
    let body = json!({"session_id": session});
    match adapt_answer(ResidentOp::TranscriptTip, body, false, db) {
        Ok((200, answer)) => answer
            .get("sessionTranscriptTip")
            .and_then(|tip| tip.get("lastSeq"))
            .and_then(Value::as_u64),
        _ => None,
    }
}

/// A refused transcript append: exit 2, with the conflict receipt on stdout under `--json` (else
/// one line on stderr). `current_cursor` is the stream's committed end from the tip op, or null
/// when it could not be read.
fn transcript_refused(
    args: &TranscriptAppendArgs,
    status: u16,
    code: &str,
    current: Option<u64>,
    detail: Value,
) -> DispatchOutcome {
    let current_text = current.map_or_else(|| "unknown".to_owned(), |end| end.to_string());
    let mut fields = Map::new();
    fields.insert("session".into(), Value::String(args.session.clone()));
    fields.insert("cursor".into(), json!(args.cursor));
    fields.insert("conflict".into(), Value::Bool(true));
    fields.insert("replayed".into(), Value::Bool(false));
    fields.insert("status".into(), json!(status));
    fields.insert("code".into(), Value::String(code.to_owned()));
    fields.insert("current_cursor".into(), json!(current));
    fields.insert("detail".into(), detail);
    if args.json {
        if let DispatchOutcome::InternalError(error) =
            print_document(SCHEMA_TRANSCRIPT_APPEND, fields)
        {
            return DispatchOutcome::InternalError(error);
        }
    }
    eprintln!(
        "membrane: transcript append refused ({code}); cursor {} is not the end; current cursor {current_text}",
        args.cursor
    );
    DispatchOutcome::Exit(crate::EXIT_USER_ERROR)
}

/// `membrane transcript append --session <id> --cursor <n> (--stdin | --text <json>) [--scope s]
/// [--json]`. The events are a JSON array of `SessionTranscriptEventV1` rows that extend the
/// stream at `--cursor`.
///
/// The tip op is read first. Outcomes:
/// - `--cursor` is the current end: the batch is appended. Success is `{conflict: false,
///   replayed: false, receipt}`.
/// - `--cursor` is behind the end and the batch lies wholly within the stream: the rows can
///   only be exact replays, so the store is asked, which writes nothing. Identical rows are
///   success with `{conflict: false, replayed: true}`. Different rows are a conflict.
/// - Otherwise (ahead of the end, or the batch would reach past it): refused with
///   `{conflict: true, current_cursor}`, without any write. Exit 2.
///
/// A 409 from the store is also a conflict, with `current_cursor` read from the tip op again.
pub fn transcript_append(args: &TranscriptAppendArgs) -> DispatchOutcome {
    if args.session.trim().is_empty() {
        return DispatchOutcome::UserError("transcript append: --session is empty".into());
    }
    let text = match &args.text {
        Some(text) => text.clone(),
        None => match read_document(args.stdin, None) {
            Ok(text) => text,
            Err(error) => {
                return DispatchOutcome::UserError(format!("transcript append: {error}"));
            }
        },
    };
    let events: Vec<SessionTranscriptEventV1> = match serde_json::from_str(&text) {
        Ok(events) => events,
        Err(error) => {
            return DispatchOutcome::UserError(format!(
                "transcript append: the events are not a JSON array of rows: {error}"
            ));
        }
    };
    let batch = events.len() as u64;
    let append = SessionTranscriptAppendV1 {
        schema_version: SESSION_TRANSCRIPT_SCHEMA_VERSION,
        session_id: args.session.clone(),
        scope_id: args.scope.clone(),
        expected_stream_seq: args.cursor,
        events,
    };
    // The runtime's own contract for the batch: non-empty scope and session ids, at most 512
    // bytes each, and sequence positions that follow the cursor.
    if let Err(error) = append.validate() {
        return DispatchOutcome::UserError(format!("transcript append: {error}"));
    }
    let db = args.db.as_deref();
    let end = match transcript_end(&args.session, db) {
        Some(end) => end,
        None => {
            return transcript_refused(args, 409, "transcript_tip_unavailable", None, Value::Null);
        }
    };
    let behind_within = args
        .cursor
        .checked_add(batch)
        .is_some_and(|last| args.cursor < end && last <= end);
    if args.cursor != end && !behind_within {
        return transcript_refused(
            args,
            409,
            "transcript_cursor_mismatch",
            Some(end),
            json!({"reason": "cursor is not the stream's current end"}),
        );
    }
    let body = json!({"operation": "session_transcript", "append": append});
    let (status, result) = match adapt_answer(ResidentOp::AdaptObservation, body, true, db) {
        Ok(answer) => answer,
        Err(outcome) => return outcome,
    };
    if status != 200 {
        let code = result
            .get("code")
            .or_else(|| result.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_owned();
        return transcript_refused(
            args,
            status,
            &code,
            transcript_end(&args.session, db),
            result,
        );
    }
    let receipt = result
        .get("sessionTranscript")
        .cloned()
        .unwrap_or(Value::Null);
    let inserted = receipt.get("inserted").and_then(Value::as_u64).unwrap_or(0);
    let already = receipt
        .get("alreadyPresent")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let replayed = inserted == 0 && already > 0;
    if !args.json {
        let last_seq = receipt.get("lastSeq").cloned().unwrap_or(Value::Null);
        let verb = if replayed { "replayed" } else { "appended" };
        println!("{} {verb} {last_seq}", args.session);
        return DispatchOutcome::Ok;
    }
    let mut fields = Map::new();
    fields.insert("session".into(), Value::String(args.session.clone()));
    fields.insert("cursor".into(), json!(args.cursor));
    fields.insert("conflict".into(), Value::Bool(false));
    fields.insert("replayed".into(), Value::Bool(replayed));
    fields.insert("receipt".into(), receipt);
    print_document(SCHEMA_TRANSCRIPT_APPEND, fields)
}

/// `membrane transcript tip --session <id> [--json]`: the committed end of the session's stream.
/// A read. A forgotten session is exit 2.
pub fn transcript_tip(args: &TranscriptTipArgs) -> DispatchOutcome {
    if args.session.trim().is_empty() {
        return DispatchOutcome::UserError("transcript tip: --session is empty".into());
    }
    let body = json!({"session_id": args.session});
    let (status, result) =
        match adapt_answer(ResidentOp::TranscriptTip, body, false, args.db.as_deref()) {
            Ok(answer) => answer,
            Err(outcome) => return outcome,
        };
    if status != 200 {
        return adapt_outcome(
            SCHEMA_TRANSCRIPT_TIP,
            status,
            result,
            args.json,
            "transcript tip",
        );
    }
    let tip = result
        .get("sessionTranscriptTip")
        .cloned()
        .unwrap_or(Value::Null);
    let current = tip.get("lastSeq").cloned().unwrap_or(Value::Null);
    if !args.json {
        println!("{current}");
        return DispatchOutcome::Ok;
    }
    let mut fields = Map::new();
    fields.insert("session".into(), Value::String(args.session.clone()));
    fields.insert("current_cursor".into(), current);
    fields.insert(
        "prefix_digest".into(),
        tip.get("prefixDigest").cloned().unwrap_or(Value::Null),
    );
    print_document(SCHEMA_TRANSCRIPT_TIP, fields)
}

/// `membrane receipts mark <id> --delivered [--json]`: an `emitted` receipt with this id becomes
/// `delivered`. Any other state stays. Exit 3 when no receipt has the id.
pub fn receipts_mark(args: &MarkArgs) -> DispatchOutcome {
    if !receipt::valid_receipt_id(&args.id) {
        return DispatchOutcome::UserError(
            "receipts mark: the id takes 1 to 128 letters, digits, '-' or '_'".into(),
        );
    }
    let found =
        match receipt::mark_delivered(&rightkit_context_runtime::cli::state_root(), &args.id) {
            Ok(found) => found,
            Err(error) => {
                return DispatchOutcome::InternalError(format!(
                    "receipts mark {}: {error}",
                    args.id
                ));
            }
        };
    if args.json {
        let receipts: Vec<Value> = found
            .iter()
            .map(|record| {
                json!({"session": record.session, "state": record.state.as_str(), "ts": record.ts})
            })
            .collect();
        let mut fields = Map::new();
        fields.insert("id".into(), Value::String(args.id.clone()));
        fields.insert("receipts".into(), Value::Array(receipts));
        if let DispatchOutcome::InternalError(error) = print_document(SCHEMA_RECEIPTS_MARK, fields)
        {
            return DispatchOutcome::InternalError(error);
        }
    } else {
        for record in &found {
            println!("{} {}", record.state.as_str(), record.session);
        }
    }
    if found.is_empty() {
        eprintln!("membrane: no receipt has id {}", args.id);
        return DispatchOutcome::Exit(crate::EXIT_NOT_AVAILABLE);
    }
    DispatchOutcome::Ok
}

/// `membrane where [--json]`: the absolute path of the running binary, with its version, so a
/// caller holding a candidate path can confirm it is Membrane and which build it is. Read-only;
/// it never contacts the resident.
pub fn where_binary(args: &WhereArgs) -> DispatchOutcome {
    let binary = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(error) => {
            return DispatchOutcome::NotAvailable(format!("resolve the membrane binary: {error}"));
        }
    };
    if !args.json {
        println!("{binary}");
        return DispatchOutcome::Ok;
    }
    let status = rightkit_context_runtime::cli::status_value();
    let mut fields = Map::new();
    fields.insert("binary".into(), binary.into());
    for key in [
        "product_version",
        "membrane_source_commit",
        "target",
        "stateRoot",
    ] {
        fields.insert(key.into(), status.get(key).cloned().unwrap_or(Value::Null));
    }
    print_document(SCHEMA_WHERE, fields)
}

/// `membrane status [--json]`.
pub fn status(args: &StatusArgs) -> DispatchOutcome {
    let mut status = rightkit_context_runtime::cli::status_value();
    // The resident reports its own model state below; without one, say what this build ships.
    status["model"] = rightkit_context_runtime::store::MODEL_READINESS.status_json();
    status["schema"] = SCHEMA_STATUS.into();
    let mut resident_answered = false;
    if let Resident::Answered(answer) = ask(ResidentOp::Status, json!({}), STATUS_DEADLINE) {
        status["mode"] = "resident".into();
        status["resident"] = answer;
        resident_answered = true;
    }
    if args.json {
        if let Err(error) = print_json(&status) {
            return DispatchOutcome::InternalError(error);
        }
    } else {
        println!("{}", status_text(&status));
    }
    if args.require_resident && !resident_answered {
        eprintln!("membrane: the resident is not answering");
        return DispatchOutcome::Exit(crate::EXIT_RESIDENT_UNREACHABLE);
    }
    DispatchOutcome::Ok
}

fn status_text(status: &Value) -> String {
    let text = |pointer: &str| {
        status
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned()
    };
    let resident = match status.pointer("/resident/running").and_then(Value::as_bool) {
        Some(true) => format!(
            "running (pid {})",
            status
                .pointer("/resident/pid")
                .and_then(Value::as_u64)
                .unwrap_or_default()
        ),
        _ => text("/resident/state"),
    };
    let store = if status.pointer("/db/present").and_then(Value::as_bool) == Some(true) {
        "present"
    } else {
        "not created yet"
    };
    [
        format!("membrane {}", text("/product_version")),
        format!("mode: {}", text("/mode")),
        format!("resident: {resident}"),
        format!("model: {}", text("/model")),
        format!("store: {store}"),
        format!("state root: {}", text("/stateRoot")),
    ]
    .join("\n")
}

/// `membrane hook`: always exits 0. Context events try the resident first; every event runs in
/// process otherwise. Only the reply the host's schema accepts goes to stdout.
pub fn hook_event(args: &HookArgs) -> DispatchOutcome {
    let wire = match args.wire {
        WireArg::Claude => Wire::Claude,
        WireArg::Codex => Wire::Codex,
    };
    let event = args.event.as_str();
    let payload = read_hook_payload();
    let mut resident_absent = false;
    if hook::emits_context(event) {
        let request = json!({"wire": wire.as_str(), "event": event, "payload": payload});
        match ask(ResidentOp::Hook, request, HOOK_DEADLINE) {
            Resident::Answered(data) => {
                if let Some(output) = data.get("output").filter(|output| !output.is_null()) {
                    println!("{output}");
                }
                let state = data
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let reason = data.get("reason").and_then(Value::as_str).unwrap_or("");
                report_hook(event, state, reason);
                return DispatchOutcome::Ok;
            }
            Resident::Absent => resident_absent = true,
            Resident::Failed(_) => {}
        }
    }
    let outcome = hook::run(wire, event, payload);
    if let Some(output) = &outcome.output {
        println!("{output}");
    }
    // Without the host app the lexical stores answer alone. When they found nothing, the warm path
    // (semantic retrieval) is what was missing, and the receipt says so.
    let (state, reason) = match outcome.receipt.state {
        HookReceiptState::Empty if resident_absent => {
            (HookReceiptState::Off, "host not running".to_owned())
        }
        state => (state, outcome.receipt.reason.clone()),
    };
    report_hook(event, state.as_str(), &reason);
    DispatchOutcome::Ok
}

/// One line on stderr when a run ended in anything but emitted or empty.
fn report_hook(event: &str, state: &str, reason: &str) {
    if !matches!(state, "emitted" | "empty") {
        eprintln!("membrane: hook {event}: {state} ({reason})");
    }
}

/// `membrane receipts confirm`: flip this session's `emitted` receipts to `delivered` where the
/// host's transcript shows their marker. Prints nothing and always exits 0, because the host app's Stop
/// hook runs it; a transcript it cannot read leaves the receipts `emitted`.
pub fn receipts_confirm(args: &ConfirmArgs) -> DispatchOutcome {
    let wire = match args.wire {
        WireArg::Claude => Wire::Claude,
        WireArg::Codex => Wire::Codex,
    };
    receipt::confirm_delivered(
        &rightkit_context_runtime::cli::state_root(),
        &args.session,
        wire.as_str(),
        &args.transcript,
    );
    DispatchOutcome::Ok
}

/// `membrane receipts list [--session <id>] [--json]`: the durable context receipts.
pub fn receipts_list(args: &ReceiptsListArgs) -> DispatchOutcome {
    let receipts = receipt::list_context_receipts(
        &rightkit_context_runtime::cli::state_root(),
        args.session.as_deref(),
    );
    if args.json {
        return match serde_json::to_value(&receipts)
            .map_err(|error| error.to_string())
            .and_then(|value| print_json(&value))
        {
            Ok(()) => DispatchOutcome::Ok,
            Err(error) => DispatchOutcome::InternalError(error),
        };
    }
    for receipt in &receipts {
        println!(
            "{} {} {} {} {} {}",
            receipt.state.as_str(),
            receipt.id,
            receipt.harness,
            receipt.event,
            receipt.session,
            receipt.ts
        );
    }
    DispatchOutcome::Ok
}

/// The hook payload as JSON. Anything unreadable is an empty object, so the run still ends in a
/// receipt (refused) instead of an error.
fn read_hook_payload() -> Value {
    if io::stdin().is_terminal() {
        return json!({});
    }
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = std::thread::Builder::new()
        .name("membrane-hook-stdin".into())
        .spawn(move || {
            let mut stdin = io::stdin();
            loop {
                let mut chunk = vec![0u8; 8192];
                match stdin.read(&mut chunk) {
                    Ok(0) => {
                        let _ = sender.send(Vec::new());
                        break;
                    }
                    Ok(read) => {
                        chunk.truncate(read);
                        if sender.send(chunk).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    if reader.is_err() {
        return json!({});
    }
    let mut body = Vec::new();
    match receiver.recv_timeout(STDIN_FIRST_BYTES_GRACE) {
        Ok(chunk) => body.extend_from_slice(&chunk),
        Err(_) => return json!({}),
    }
    while body.len() <= MAX_HOOK_PAYLOAD_BYTES {
        match receiver.recv_timeout(STDIN_QUIET) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => body.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    serde_json::from_slice(&body).unwrap_or_else(|_| json!({}))
}

/// `membrane serve`: the resident. The host app starts it with a stdin pipe it holds for the resident's life; `serve` reports itself and
/// returns the exit code ("already serving" is 0).
pub fn serve() -> DispatchOutcome {
    DispatchOutcome::Exit(crate::serve::run_serve())
}

/// `membrane init <root> --repository <id> --scope <id>`: operator-owned registry enrollment.
pub fn init(args: &InitArgs) -> DispatchOutcome {
    let options = crate::native_init::InitOptions {
        root: args.root.clone(),
        repository_id: args.repository.clone(),
        scope_id: args.scope.clone(),
        rebind_from: args.rebind_from.clone(),
        virtual_id: args.virtual_id.clone(),
        tenant_id: args.tenant_id.clone(),
        parents: args.parent.clone(),
        native: args.native.clone(),
        config: args.config.clone(),
        dry_run: args.dry_run,
    };
    match crate::native_init::init(&options) {
        Ok(receipt) => match serde_json::to_string_pretty(&receipt) {
            Ok(json) => {
                println!("{json}");
                DispatchOutcome::Ok
            }
            Err(error) => DispatchOutcome::InternalError(format!(
                "init receipt serialization failed: {error}"
            )),
        },
        Err(error) => DispatchOutcome::UserError(format!("init: {error}")),
    }
}
