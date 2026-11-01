//! Holder-independent native Blueprint execution.
//!
//! Explicit CLI and MCP work dispatch directly to Blueprint's bounded
//! in-process operation. This module never discovers an endpoint or starts
//! an interpreter-backed child.

use rightkit_repo_graph::{
    native_blueprint_operation, BlueprintRequest, BlueprintResponse, CancellationToken,
    OneShotExecutor, Operation,
};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Execute one explicit request without acquiring resident authority.
pub(crate) fn dispatch_native(
    request: BlueprintRequest,
    cancellation: CancellationToken,
) -> BlueprintResponse {
    OneShotExecutor::new(native_blueprint_operation()).dispatch(request, cancellation)
}

/// Run a native Blueprint CLI verb from current repository root.
pub(crate) fn run_cli(args: &[String]) -> Result<(), String> {
    if let Some(result) = run_legacy_alias(args)? {
        crate::cli::emit_stdout(format_args!(
            "{}",
            serde_json::to_string(&result)
                .map_err(|error| format!("encode Blueprint response: {error}"))?
        ));
        return Ok(());
    }
    let (method, root, input, cancel_before_dispatch, deadline_ms) =
        cli_request_with_deadline(args)?;
    let request_id = format!(
        "blueprint-cli-{}-{}",
        std::process::id(),
        crate::time::now_millis()
    );
    let mut request = BlueprintRequest::new(request_id, method, root.to_string_lossy());
    request.deadline_ms = deadline_ms;
    request.input = input;
    let cancellation = CancellationToken::new();
    if cancel_before_dispatch {
        cancellation.cancel();
    }
    let response = dispatch_native(request, cancellation);
    if response.ok {
        crate::cli::emit_stdout(format_args!(
            "{}",
            serde_json::to_string(&response.result)
                .map_err(|error| format!("encode Blueprint response: {error}"))?
        ));
        return Ok(());
    }
    let error = response
        .error
        .map(|error| format!("{}: {}", error.code, error.message))
        .unwrap_or_else(|| "native Blueprint request failed".into());
    Err(error)
}

/// Keep compatibility verbs that were CLI-only in legacy Blueprint while
/// routing their implementation through native Rust helpers.
fn run_legacy_alias(args: &[String]) -> Result<Option<Value>, String> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Ok(None);
    };
    let root = args
        .windows(2)
        .find(|pair| pair[0] == "--repo-root")
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".into())
        });
    match verb {
        "doctor" => {
            let full = args.iter().any(|v| v == "--full");
            Ok(Some(rightkit_repo_graph::cli::doctor(root, full)))
        }
        "languages" => Ok(Some(rightkit_repo_graph::cli::languages())),
        "docs" => {
            let limit = args
                .windows(2)
                .find(|pair| pair[0] == "--limit")
                .and_then(|pair| pair[1].parse().ok());
            rightkit_repo_graph::cli::docs(root, limit, None)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        "rules" => {
            let subcommand = args
                .get(1)
                .filter(|v| !v.starts_with('-'))
                .map(String::as_str);
            rightkit_repo_graph::cli::rules(root, subcommand)
                .map(Some)
                .map_err(|e| e.to_string())
        }
        "snapshot" if args.get(1).map(String::as_str) == Some("create") => {
            let name = args
                .get(2)
                .filter(|v| !v.starts_with('-'))
                .ok_or("snapshot create requires a name")?;
            rightkit_repo_graph::cli::snapshot_create(root, name.clone())
                .map(Some)
                .map_err(|e| e.to_string())
        }
        _ => Ok(None),
    }
}

/// Parse a native Blueprint CLI verb and its arguments into a dispatchable
/// request, additionally returning the deadline (in
/// milliseconds) this invocation should run with. An explicit `--deadline-ms`
/// always wins. Absent an override, `Operation::Refresh` uses the same
/// extended cap the resident watcher path already applies to refresh work
/// (`membrane-blueprint/src/service.rs::execute_refresh_event`) rather than
/// the short interactive-query default: an explicit CLI refresh performs the
/// same real graph rebuild/publish as a watcher-triggered refresh, so the
/// short default (tuned for point queries like `status`/`search`) starves it
/// before it can finish (LC-02: installed refresh against a real repository
/// observed at ~2.1s wall time against a 2000ms/`DEFAULT_DEADLINE_MS`
/// deadline, returning `deadline_exceeded` every time).
fn cli_request_with_deadline(
    args: &[String],
) -> Result<(Operation, PathBuf, Value, bool, u64), String> {
    let Some(verb) = args.first() else {
        return Err("native Blueprint operation is required".into());
    };
    let method = Operation::parse(verb)
        .ok_or_else(|| format!("unsupported native Blueprint operation: {verb}"))?;
    let mut root =
        std::env::current_dir().map_err(|error| format!("resolve repository root: {error}"))?;
    let mut input = json!({"repoRoot": root.to_string_lossy()});
    let mut positional = Vec::new();
    let mut cancel_before_dispatch = false;
    let mut deadline_ms: Option<u64> = None;
    let mut index = 1;
    while index < args.len() {
        let key = &args[index];
        if key == "--repo-root" {
            index += 1;
            root = PathBuf::from(args.get(index).ok_or("--repo-root requires a path")?);
        } else if key == "--deadline-ms" {
            index += 1;
            let value = args.get(index).ok_or("--deadline-ms requires a value")?;
            deadline_ms = Some(parse_cli_u64("deadline-ms", value)?);
        } else if key == "--cancel-before-dispatch" {
            cancel_before_dispatch = true;
        } else if key == "--allow-stale" {
            input["allowStale"] = Value::Bool(true);
        } else if key == "--dry-run" {
            input["dryRun"] = Value::Bool(true);
        } else if key == "--offline" {
            input["offline"] = Value::Bool(true);
        } else if key == "--update-checks-disabled" {
            input["updateChecksDisabled"] = Value::Bool(true);
        } else if let Some(field) = key.strip_prefix("--") {
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| format!("{key} requires a value"))?;
            match field {
                "query" => {
                    input["query"] = Value::String(value.clone());
                    input["task"] = Value::String(value.clone());
                }
                "task" => input["task"] = Value::String(value.clone()),
                "generation" | "baseline-generation" => {
                    input[if field == "generation" {
                        "generation"
                    } else {
                        "baselineGeneration"
                    }] = Value::String(value.clone());
                }
                "snapshot" => input["snapshot"] = Value::String(value.clone()),
                "since-generation" => input["sinceGeneration"] = Value::String(value.clone()),
                "treeish" => input["treeish"] = serde_json::json!({"base": value}),
                "head" => {
                    if !input.get("treeish").is_some_and(Value::is_object) {
                        input["treeish"] = serde_json::json!({});
                    }
                    input["treeish"]["head"] = Value::String(value.clone());
                }
                "name" | "fingerprint" => input[field] = Value::String(value.clone()),
                "fingerprints" => {
                    input["fingerprints"] = Value::Array(
                        value
                            .split(',')
                            .filter(|v| !v.is_empty())
                            .map(|v| Value::String(v.to_owned()))
                            .collect(),
                    )
                }
                "paths" => {
                    input["paths"] = Value::Array(
                        value
                            .split(',')
                            .filter(|v| !v.is_empty())
                            .map(|v| Value::String(v.to_owned()))
                            .collect(),
                    )
                }
                "event-kind" => input["eventKind"] = Value::String(value.clone()),
                "source-clock" => input["sourceClock"] = Value::from(parse_cli_u64(field, value)?),
                "node" => input["nodeId"] = Value::String(value.clone()),
                "seed" | "target" | "from" | "to" | "direction" => {
                    input[field] = Value::String(value.clone());
                }
                "limit" => {
                    let value = parse_cli_u64(field, value)?;
                    input["maxCandidates"] = Value::from(value);
                    input["maxSeeds"] = Value::from(value);
                }
                "depth" => input["maxDepth"] = Value::from(parse_cli_u64(field, value)?),
                "budget" => input["maxBytes"] = Value::from(parse_cli_u64(field, value)?),
                "max-paths" => input["maxPaths"] = Value::from(parse_cli_u64(field, value)?),
                _ => return Err(format!("unsupported native Blueprint option: {key}")),
            }
        } else {
            positional.push(key.clone());
        }
        index += 1;
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("canonicalize repository root: {error}"))?;
    input["repoRoot"] = Value::String(root.to_string_lossy().into_owned());
    if let Some(value) = positional.first().cloned() {
        match method {
            Operation::Search => {
                input["query"] = Value::String(value.clone());
                input["task"] = Value::String(value);
            }
            Operation::Recall | Operation::Architecture => input["task"] = Value::String(value),
            Operation::Resolve => input["nodeId"] = Value::String(value),
            Operation::Expand | Operation::Impact => {
                input["seed"] = Value::String(value.clone());
                input["target"] = Value::String(value);
            }
            _ => input["args"] = Value::Array(positional.into_iter().map(Value::String).collect()),
        }
    }
    let deadline_ms = deadline_ms.unwrap_or_else(|| default_deadline_ms(method));
    Ok((method, root, input, cancel_before_dispatch, deadline_ms))
}

/// Default deadline (ms) for a native CLI operation absent an explicit
/// `--deadline-ms`. Point queries (`status`, `search`, `recall`, ...) keep
/// the protocol's short interactive default. `Build` performs a complete
/// graph scan, while `Refresh` performs the same
/// real rebuild-and-publish graph work as the resident watcher's refresh
/// path, which already runs under `model::MAX_DEADLINE_MS` rather than the
/// interactive default (see `membrane-blueprint/src/service.rs`,
/// `execute_refresh_event`) — an explicit CLI refresh gets the same cap for
/// the same reason.
fn default_deadline_ms(method: Operation) -> u64 {
    match method {
        // Build performs a complete source scan and durable graph publish;
        // match resident startup's bounded build cap instead of starving the
        // first explicit Hub-off build with the point-query default.
        Operation::Build => rightkit_repo_graph::model::MAX_BUILD_DEADLINE_MS,
        Operation::Refresh => rightkit_repo_graph::model::MAX_DEADLINE_MS,
        _ => rightkit_repo_graph::model::DEFAULT_DEADLINE_MS,
    }
}

fn parse_cli_u64(field: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("--{field} requires an unsigned integer"))
}
