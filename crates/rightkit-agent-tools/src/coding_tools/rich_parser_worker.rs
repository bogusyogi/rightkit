//! Killable process boundary for untrusted rich-file parsing.
//!
//! The daemon reuses its own executable under a private, environment-gated
//! entry point. The worker only accepts a bounded JSON request on stdin and
//! emits one bounded JSON response on stdout; it never invokes external
//! helpers or performs network I/O.

use crate::coding_tools::tool_trait::ToolResult;
use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const WORKER_ENV: &str = "RIGHTKIT_INTERNAL_RICH_PARSER_WORKER";
const WORKER_TOKEN: &str = "v1:8f6c1d98f6a8465eb77b6efc9bb62a51";
const RESPONSE_PREFIX: &str = "RIGHTKIT_RICH_PARSER_RESPONSE_V1:";
const MAX_REQUEST_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
#[cfg(not(test))]
const WORKER_TIMEOUT: Duration = Duration::from_secs(15);
// Test budget must absorb a cold worker spawn on a loaded host; 250ms made the PDF/image
// read tests fail with "exceeded its 0 second timeout" under concurrent cargo load.
#[cfg(test)]
const WORKER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
struct WorkerRequest {
    workspace_root: PathBuf,
    path: PathBuf,
    page: Option<u64>,
    page_limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerResponse {
    content: String,
    is_error: bool,
    media_type: Option<String>,
}

impl From<ToolResult> for WorkerResponse {
    fn from(result: ToolResult) -> Self {
        Self {
            content: result.content,
            is_error: result.is_error,
            media_type: result.media_type,
        }
    }
}

impl From<WorkerResponse> for ToolResult {
    fn from(response: WorkerResponse) -> Self {
        let mut result = if response.is_error {
            ToolResult::error(response.content)
        } else {
            ToolResult::ok(response.content)
        };
        result.media_type = response.media_type;
        result
    }
}

/// Parse supported rich files in a separate process. Unsupported extensions
/// return `None`, preserving the text reader fallback.
pub async fn read_rich_file(
    path: &Path,
    args: &Value,
    workspace_root: &Path,
) -> Option<ToolResult> {
    if !is_supported_rich_path(path) {
        return None;
    }

    let request = WorkerRequest {
        workspace_root: workspace_root.to_path_buf(),
        path: path.to_path_buf(),
        page: args.get("page").and_then(Value::as_u64),
        page_limit: args.get("page_limit").and_then(Value::as_u64),
    };
    Some(run_worker(request).await)
}

fn is_supported_rich_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ipynb" | "pdf"
    )
}

async fn run_worker(request: WorkerRequest) -> ToolResult {
    let encoded = match serde_json::to_vec(&request) {
        Ok(encoded) if encoded.len() <= MAX_REQUEST_BYTES => encoded,
        Ok(_) => return ToolResult::error("Rich parser request exceeds the safe input limit"),
        Err(error) => {
            return ToolResult::error(format!("Failed to encode rich parser request: {error}"))
        }
    };

    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            return ToolResult::error(format!("Failed to locate isolated rich parser: {error}"))
        }
    };
    let mut argv = Vec::new();
    let mut env = vec![(WORKER_ENV.to_string(), WORKER_TOKEN.to_string())];
    configure_worker_args(&mut argv, &mut env);
    let context = ProcessLaunchContext {
        workspace_root: request.workspace_root.display().to_string(),
        sandbox_profile: Some("local-model-probe".to_string()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = CommandLaunchSpec {
        program: executable.display().to_string(),
        argv,
        cwd: context.workspace_root.clone(),
        context,
        env,
    };
    let adapter = worker_launch_adapter();
    let mut child = match adapter.spawn(spec).await {
        Ok(child) => child,
        Err(error) => {
            return ToolResult::error(format!("Failed to start isolated rich parser: {error:?}"))
        }
    };
    let Some(mut stdin) = child.take_stdin() else {
        return ToolResult::error("Rich parser worker missing stdin");
    };
    let Some(stdout) = child.take_stdout() else {
        return ToolResult::error("Rich parser worker missing stdout");
    };
    let Some(stderr) = child.take_stderr() else {
        return ToolResult::error("Rich parser worker missing stderr");
    };

    let exchange = async {
        stdin.write_all(&encoded).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_limited(stdout, MAX_RESPONSE_BYTES),
            read_limited(stderr, MAX_DIAGNOSTIC_BYTES)
        );
        let status = status
            .map_err(|error| std::io::Error::other(format!("worker wait failed: {error:?}")))?;
        Ok::<_, std::io::Error>((status, stdout?, stderr?))
    };

    let (status, stdout, stderr) = match tokio::time::timeout(WORKER_TIMEOUT, exchange).await {
        Ok(Ok(exchange)) => exchange,
        Ok(Err(error)) => {
            let _ = child.cancel().await;
            return ToolResult::error(format!("Isolated rich parser I/O failed: {error}"));
        }
        Err(_) => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            return ToolResult::error(format!(
                "Isolated rich parser exceeded its {} second timeout",
                WORKER_TIMEOUT.as_secs()
            ));
        }
    };

    if status != 0 {
        let diagnostic = String::from_utf8_lossy(&stderr);
        return ToolResult::error(format!(
            "Isolated rich parser exited unsuccessfully{}",
            if diagnostic.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", diagnostic.trim())
            }
        ));
    }
    decode_response(&stdout).unwrap_or_else(ToolResult::error)
}

async fn read_limited(
    reader: impl AsyncRead + Unpin,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut output)
        .await?;
    if output.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "worker output exceeded the safe limit",
        ));
    }
    Ok(output)
}

fn decode_response(stdout: &[u8]) -> Result<ToolResult, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("Isolated rich parser returned invalid UTF-8: {error}"))?;
    let start = text
        .rfind(RESPONSE_PREFIX)
        .ok_or_else(|| "Isolated rich parser returned no protocol response".to_string())?;
    let payload = text[start + RESPONSE_PREFIX.len()..]
        .lines()
        .next()
        .unwrap_or_default();
    serde_json::from_str::<WorkerResponse>(payload)
        .map(ToolResult::from)
        .map_err(|error| format!("Isolated rich parser returned invalid JSON: {error}"))
}

/// Production launch adapter for the worker process: always the real,
/// enforced sandbox seam (CG-B02). Tests that exercise worker *logic*
/// (parsing, path validation) rather than confinement itself may swap this
/// for `process_launch::UnconfinedProcessLaunchAdapter` via
/// [`TEST_LAUNCH_ADAPTER`] since a confined child cannot reach a plain
/// `tempdir()` fixture on every platform.
#[cfg(not(test))]
fn worker_launch_adapter() -> Arc<dyn ProcessLaunchAdapter> {
    Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter)
}

#[cfg(test)]
fn worker_launch_adapter() -> Arc<dyn ProcessLaunchAdapter> {
    TEST_LAUNCH_ADAPTER
        .with(|cell| cell.borrow().clone())
        .unwrap_or_else(|| Arc::new(crate::coding_tools::shell::NativeCommandSandboxAdapter))
}

/// Test-only: override the rich parser worker's process launch adapter for
/// the duration of the calling thread. Used by tests elsewhere in the crate
/// (e.g. `read_file` tests) whose subject is worker/parsing logic rather
/// than sandbox confinement.
#[cfg(test)]
pub(crate) fn set_test_launch_adapter(adapter: Option<Arc<dyn ProcessLaunchAdapter>>) {
    TEST_LAUNCH_ADAPTER.with(|cell| *cell.borrow_mut() = adapter);
}

#[cfg(not(test))]
fn configure_worker_args(_args: &mut Vec<String>, _env: &mut Vec<(String, String)>) {}

#[cfg(test)]
fn configure_worker_args(args: &mut Vec<String>, env: &mut Vec<(String, String)>) {
    args.extend(
        [
            "--exact",
            "rich_parser_worker::tests::worker_process_entry",
            "--ignored",
            "--nocapture",
        ]
        .map(String::from),
    );
    if let Some(behavior) = TEST_BEHAVIOR.with(|value| value.borrow().clone()) {
        env.push(("RIGHTKIT_RICH_PARSER_TEST_BEHAVIOR".to_string(), behavior));
    }
}

/// Run the private worker protocol when invoked by the daemon's pre-CLI gate.
/// Returns false for every ordinary invocation.
pub fn run_if_requested() -> bool {
    if std::env::var(WORKER_ENV).as_deref() != Ok(WORKER_TOKEN) {
        return false;
    }
    run_worker_stdio();
    true
}

fn run_worker_stdio() {
    #[cfg(test)]
    match std::env::var("RIGHTKIT_RICH_PARSER_TEST_BEHAVIOR").as_deref() {
        Ok("timeout") => std::thread::sleep(WORKER_TIMEOUT + Duration::from_secs(2)),
        Ok("crash") => std::process::exit(73),
        _ => {}
    }

    let mut input = Vec::new();
    let read_result = std::io::stdin()
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut input);
    let result = match read_result {
        Err(error) => ToolResult::error(format!("Failed to read rich parser request: {error}")),
        Ok(_) => process_request(&input),
    };
    let response = WorkerResponse::from(result);
    let encoded = serde_json::to_string(&response).unwrap_or_else(|error| {
        format!(
            "{{\"content\":\"Failed to encode worker response: {error}\",\"is_error\":true,\"media_type\":null}}"
        )
    });
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{RESPONSE_PREFIX}{encoded}");
    let _ = stdout.flush();
}

fn process_request(input: &[u8]) -> ToolResult {
    if input.len() > MAX_REQUEST_BYTES {
        return ToolResult::error("Rich parser request exceeds the safe input limit");
    }
    let request: WorkerRequest = match serde_json::from_slice(input) {
        Ok(request) => request,
        Err(error) => return ToolResult::error(format!("Invalid rich parser request: {error}")),
    };
    let workspace_root = match request.workspace_root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        Ok(_) => return ToolResult::error("Rich parser workspace root is not a directory"),
        Err(error) => {
            return ToolResult::error(format!("Invalid rich parser workspace root: {error}"))
        }
    };
    let path = match request.path.canonicalize() {
        Ok(path) => path,
        Err(error) => return ToolResult::error(format!("Invalid rich parser path: {error}")),
    };
    if !path.starts_with(&workspace_root) || !path.is_file() {
        return ToolResult::error("Rich parser path must be a file inside the workspace root");
    }
    if !is_supported_rich_path(&path) {
        return ToolResult::error("Rich parser received an unsupported file type");
    }
    let args = serde_json::json!({
        "page": request.page,
        "page_limit": request.page_limit,
    });
    crate::coding_tools::read_file::read_rich_file_in_process(&path, &args)
        .unwrap_or_else(|| ToolResult::error("Rich parser received an unsupported file type"))
}

#[cfg(test)]
thread_local! {
    static TEST_BEHAVIOR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// Test-only override for the worker's process launch adapter (see
    /// `worker_launch_adapter`). Tests whose subject is worker logic (not
    /// sandbox confinement) set this to
    /// `process_launch::UnconfinedProcessLaunchAdapter` so the child can
    /// reach a plain `tempdir()` fixture; tests that don't set it still run
    /// the real enforced adapter.
    static TEST_LAUNCH_ADAPTER: std::cell::RefCell<Option<Arc<dyn ProcessLaunchAdapter>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::process_launch::UnconfinedProcessLaunchAdapter;
    use tempfile::tempdir;

    fn use_unconfined_launch_adapter() {
        TEST_LAUNCH_ADAPTER
            .with(|cell| *cell.borrow_mut() = Some(Arc::new(UnconfinedProcessLaunchAdapter)));
    }

    fn reset_launch_adapter() {
        TEST_LAUNCH_ADAPTER.with(|cell| *cell.borrow_mut() = None);
    }

    #[test]
    #[ignore]
    fn worker_process_entry() {
        assert!(run_if_requested());
    }

    #[tokio::test]
    async fn isolated_worker_extracts_normal_notebook() {
        use_unconfined_launch_adapter();
        let dir = tempdir().unwrap();
        let path = dir.path().join("normal.ipynb");
        std::fs::write(
            &path,
            r#"{"nbformat":4,"nbformat_minor":5,"cells":[{"cell_type":"code","source":["x = 1"]}]}"#,
        )
        .unwrap();
        let result = read_rich_file(&path, &serde_json::json!({}), dir.path())
            .await
            .unwrap();
        reset_launch_adapter();
        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("cell 0 [code]"));
    }

    #[tokio::test]
    async fn isolated_worker_rejects_path_outside_workspace() {
        use_unconfined_launch_adapter();
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let path = outside.path().join("outside.ipynb");
        std::fs::write(&path, r#"{"nbformat":4,"cells":[]}"#).unwrap();
        let result = read_rich_file(&path, &serde_json::json!({}), workspace.path())
            .await
            .unwrap();
        reset_launch_adapter();
        assert!(result.is_error);
        assert!(result.content.contains("inside the workspace root"));
    }

    #[tokio::test]
    async fn isolated_worker_reports_crash() {
        use_unconfined_launch_adapter();
        TEST_BEHAVIOR.with(|value| *value.borrow_mut() = Some("crash".into()));
        let dir = tempdir().unwrap();
        let path = dir.path().join("crash.ipynb");
        std::fs::write(&path, r#"{"nbformat":4,"cells":[]}"#).unwrap();
        let result = read_rich_file(&path, &serde_json::json!({}), dir.path())
            .await
            .unwrap();
        TEST_BEHAVIOR.with(|value| *value.borrow_mut() = None);
        reset_launch_adapter();
        assert!(result.is_error);
        assert!(
            result.content.contains("exited unsuccessfully"),
            "{}",
            result.content
        );
    }

    #[tokio::test]
    async fn isolated_worker_enforces_timeout() {
        use_unconfined_launch_adapter();
        TEST_BEHAVIOR.with(|value| *value.borrow_mut() = Some("timeout".into()));
        let dir = tempdir().unwrap();
        let path = dir.path().join("timeout.ipynb");
        std::fs::write(&path, r#"{"nbformat":4,"cells":[]}"#).unwrap();
        let result = read_rich_file(&path, &serde_json::json!({}), dir.path())
            .await
            .unwrap();
        TEST_BEHAVIOR.with(|value| *value.borrow_mut() = None);
        reset_launch_adapter();
        assert!(result.is_error);
        assert!(result.content.contains("timeout"), "{}", result.content);
    }

    #[test]
    fn worker_rejects_oversized_or_invalid_input() {
        let oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
        let oversized_result = process_request(&oversized);
        assert!(oversized_result.is_error);
        assert!(oversized_result.content.contains("safe input limit"));

        let invalid = process_request(b"not-json");
        assert!(invalid.is_error);
        assert!(invalid.content.contains("Invalid rich parser request"));
    }
}
