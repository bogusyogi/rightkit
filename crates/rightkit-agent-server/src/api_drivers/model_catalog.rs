//! Codex `model/list` app-server protocol: spawns a short-lived, sandboxed
//! `app-server --stdio` process purely to answer a read-only model catalogue
//! query, then unconditionally kills it. Decomposed out of `api_drivers.rs`
//! (ra-016) with the same wire protocol, timeout, and process-lifetime
//! guarantees; `api_drivers.rs` re-exports these functions unqualified
//! (`pub(crate) use model_catalog::*;`) so its route handlers call them
//! exactly as before the split.

use rightkit_agents::driver_host::cli_runtime::CliAccountProfile;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Bound on the dedicated app-server process spawned purely to answer `model/list`. The daemon
/// does not keep a long-lived app-server connection open for this read-only lookup: a short-lived
/// process is spawned, queried, and killed unconditionally (success, error, or timeout) rather
/// than reused across requests.
const CODEX_MODEL_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Query Codex's `model/list` app-server method by spawning a dedicated, short-lived
/// `app-server` process purely for this read-only lookup. Bounded by
/// `CODEX_MODEL_QUERY_TIMEOUT` and unconditionally killed on the way out (success, error, or
/// timeout), since the daemon does not keep a long-lived app-server connection open just to
/// answer this.
pub(crate) async fn query_codex_model_catalogue(executable: &str) -> Result<Vec<Value>, String> {
    match tokio::time::timeout(
        CODEX_MODEL_QUERY_TIMEOUT,
        query_codex_model_catalogue_inner(executable),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!("{executable} model/list query timed out")),
    }
}

pub(crate) async fn query_codex_model_catalogue_with_profile(
    executable: &str,
    profile: &CliAccountProfile,
) -> Result<Vec<Value>, String> {
    let home = profile.managed_home.clone();
    query_codex_model_catalogue_managed(
        executable,
        vec![("CODEX_HOME".into(), home.to_string_lossy().into_owned())],
    )
    .await
}

async fn query_codex_model_catalogue_inner(executable: &str) -> Result<Vec<Value>, String> {
    query_codex_model_catalogue_managed(executable, Vec::new()).await
}

async fn query_codex_model_catalogue_managed(
    executable: &str,
    env: Vec<(String, String)>,
) -> Result<Vec<Value>, String> {
    let adapter = rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter;
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.display().to_string(),
        sandbox_profile: Some("codex-app-server".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.into(),
        argv: vec!["app-server".into(), "--stdio".into()],
        cwd: context.workspace_root.clone(),
        context,
        env,
    };
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
        .await
        .map_err(|e| format!("failed to spawn {executable}: {e:?}"))?;
    let mut stdin = match child.take_stdin() {
        Some(stream) => stream,
        None => {
            let _ = child.cancel().await;
            return Err("Codex stdin pipe unavailable".to_string());
        }
    };
    let stdout = match child.take_stdout() {
        Some(stream) => stream,
        None => {
            let _ = child.cancel().await;
            return Err("Codex stdout pipe unavailable".to_string());
        }
    };
    let mut lines = BufReader::new(stdout).lines();
    let result = query_codex_jsonrpc_stream(&mut stdin, &mut lines).await;
    let _ = child.cancel().await;
    let _ = child.wait().await;
    result
}

#[allow(dead_code)]
async fn query_codex_model_catalogue_over_stdio(
    child: &mut tokio::process::Child,
) -> Result<Vec<Value>, String> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Codex stdin pipe unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Codex stdout pipe unavailable".to_string())?;
    let mut lines = BufReader::new(stdout).lines();

    write_codex_jsonrpc_line(
        &mut stdin,
        &json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": crate::host_identity::app(),
                    "title": "RightKit",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": { "experimentalApi": false }
            }
        }),
    )
    .await?;
    read_codex_jsonrpc_response(&mut lines, 1).await?;

    write_codex_jsonrpc_line(&mut stdin, &json!({ "method": "initialized" })).await?;

    write_codex_jsonrpc_line(
        &mut stdin,
        // Wire method names are lowercase; `Model/list` in the generated
        // schema is the Rust TYPE name, not the JSON-RPC method. Sending the
        // type name returns an error the caller surfaced as a bodyless 502.
        &json!({ "id": 2, "method": "model/list", "params": { "includeHidden": false } }),
    )
    .await?;
    let result = read_codex_jsonrpc_response(&mut lines, 2).await?;
    Ok(extract_visible_codex_models(&result))
}

async fn query_codex_jsonrpc_stream<W, R>(
    stdin: &mut W,
    lines: &mut tokio::io::Lines<BufReader<R>>,
) -> Result<Vec<Value>, String>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    write_codex_jsonrpc_line(stdin, &json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":crate::host_identity::app(),"title":"RightKit","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}}})).await?;
    read_codex_jsonrpc_response(lines, 1).await?;
    write_codex_jsonrpc_line(stdin, &json!({"method":"initialized"})).await?;
    write_codex_jsonrpc_line(
        stdin,
        &json!({"id":2,"method":"model/list","params":{"includeHidden":false}}),
    )
    .await?;
    let result = read_codex_jsonrpc_response(lines, 2).await?;
    Ok(extract_visible_codex_models(&result))
}

async fn write_codex_jsonrpc_line(
    stdin: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &Value,
) -> Result<(), String> {
    let bytes = rightkit_sidecar::encode_json_line(value, rightkit_sidecar::UNCAPPED)
        .map_err(|error| error.to_string())?;
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    stdin.flush().await.map_err(|error| error.to_string())
}

/// Read stdout lines until the JSON-RPC response correlating to `expected_id` arrives, skipping
/// any notification or unrelated response the app-server emits in the meantime.
async fn read_codex_jsonrpc_response(
    lines: &mut tokio::io::Lines<BufReader<impl tokio::io::AsyncRead + Unpin>>,
    expected_id: u64,
) -> Result<Value, String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Codex app-server closed stdout before responding".to_string())?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if id != expected_id {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(format!("Codex app-server returned an error: {error}"));
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// Map a `model/list` result onto the documented `Model` fields
/// (`id`, `model`, `displayName`, `description`, `isDefault`, `hidden`, ...), dropping hidden
/// entries and any item missing the required `id`.
pub(crate) fn extract_visible_codex_models(result: &Value) -> Vec<Value> {
    let items = result
        .get("items")
        .or_else(|| result.get("models"))
        .or_else(|| result.get("data"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    items
        .into_iter()
        .filter(|item| item.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?.to_string();
            Some(json!({
                "id": id,
                "model": item.get("model").and_then(Value::as_str),
                "displayName": item.get("displayName").and_then(Value::as_str),
                "description": item.get("description").and_then(Value::as_str),
                "isDefault": item.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
        .collect()
}
