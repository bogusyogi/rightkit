use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// Read one newline-terminated REPL protocol frame with a hard byte bound.
/// The frame is the helper's only output surface; an unbounded read would
/// let a runaway helper grow session memory without limit.
pub(super) async fn read_frame_bounded(
    reader: &mut BufReader<Pin<Box<dyn AsyncRead + Send>>>,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF: a partial frame still counts if bytes were read.
            return Ok(if frame.is_empty() { None } else { Some(frame) });
        }
        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            frame.extend_from_slice(&available[..pos]);
            reader.consume(pos + 1);
            return Ok(Some(frame));
        }
        let remaining = limit.saturating_sub(frame.len());
        if remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("REPL frame exceeds {limit} byte limit"),
            ));
        }
        let take = available.len().min(remaining);
        frame.extend_from_slice(&available[..take]);
        reader.consume(take);
    }
}

pub(super) fn node_command() -> String {
    if let Ok(path) = std::env::var("RIGHTKIT_NODE") {
        if !path.trim().is_empty() {
            return path;
        }
    }

    #[cfg(windows)]
    {
        if let Ok(user_profile) = std::env::var("USERPROFILE") {
            let bundled = std::path::PathBuf::from(user_profile)
                .join(".cache")
                .join("codex-runtimes")
                .join("codex-primary-runtime")
                .join("dependencies")
                .join("node")
                .join("bin")
                .join("node.exe");
            if bundled.exists() {
                return bundled.to_string_lossy().into_owned();
            }
        }
    }

    "node".to_string()
}

/// CQ-16: the `language` enum may only advertise runtimes whose backing
/// executable actually resolves on this host — an advertised-but-absent
/// runtime is a guaranteed `start` failure. Probed once per process; PATH
/// is stable for a session's lifetime.
pub(crate) fn available_languages() -> &'static [&'static str] {
    static LANGS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    LANGS
        .get_or_init(|| {
            let mut langs = Vec::new();
            // Mirror the spawn targets in `spawn_session` exactly.
            let python_exe = if cfg!(windows) { "python" } else { "python3" };
            if executable_on_path(python_exe) {
                langs.push("python");
            }
            if node_runtime_available() {
                langs.push("node");
            }
            langs
        })
        .as_slice()
}

pub(crate) fn node_runtime_available() -> bool {
    // `node_command` resolves RIGHTKIT_NODE, then the bundled runtime, then
    // the bare PATH fallback — a non-"node" result means a concrete path won.
    node_command() != "node" || executable_on_path("node")
}

pub(crate) fn executable_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        if std::fs::metadata(dir.join(name)).is_ok_and(|m| m.is_file()) {
            return true;
        }
        #[cfg(windows)]
        {
            // Executables carry an extension; check each PATHEXT suffix.
            std::env::var_os("PATHEXT").is_some_and(|exts| {
                exts.to_string_lossy().split(';').any(|ext| {
                    !ext.is_empty()
                        && std::fs::metadata(dir.join(format!("{name}{}", ext.to_lowercase())))
                            .is_ok_and(|m| m.is_file())
                })
            })
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

pub(super) const PYTHON_HELPER: &str = r#"
import ast
import contextlib
import io
import json
import sys
import traceback

ns = {}

def split_expr(src):
    tree = ast.parse(src, mode="exec")
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        body = tree.body[:-1]
        expr = ast.Expression(tree.body[-1].value)
        ast.fix_missing_locations(expr)
        mod = ast.Module(body=body, type_ignores=[])
        ast.fix_missing_locations(mod)
        return mod, expr
    return tree, None

for line in sys.stdin:
    req = {}
    try:
        req = json.loads(line)
        code = req.get("code", "")
        out = io.StringIO()
        err = io.StringIO()
        result = None
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            mod, expr = split_expr(code)
            exec(compile(mod, "<rightkit-agent-repl>", "exec"), ns, ns)
            if expr is not None:
                result = eval(compile(expr, "<rightkit-agent-repl>", "eval"), ns, ns)
        print(json.dumps({
            "ok": True,
            "id": req.get("id"),
            "stdout": out.getvalue(),
            "stderr": err.getvalue(),
            "result": "" if result is None else repr(result),
        }), flush=True)
    except Exception:
        print(json.dumps({
            "ok": False,
            "id": req.get("id"),
            "stdout": locals().get("out", io.StringIO()).getvalue(),
            "stderr": locals().get("err", io.StringIO()).getvalue(),
            "error": traceback.format_exc(),
        }), flush=True)
"#;

pub(super) const NODE_HELPER: &str = r#"
const readline = require('readline');
const vm = require('vm');
const context = vm.createContext({ console });
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

rl.on('line', (line) => {
  let stdout = '';
  let stderr = '';
  let req = null;
  const originalLog = console.log;
  const originalError = console.error;
  console.log = (...args) => { stdout += args.map(String).join(' ') + '\n'; };
  console.error = (...args) => { stderr += args.map(String).join(' ') + '\n'; };
  try {
    req = JSON.parse(line);
    const result = vm.runInContext(req.code || '', context);
    console.log = originalLog;
    console.error = originalError;
    process.stdout.write(JSON.stringify({
      ok: true,
      id: req.id === undefined ? null : req.id,
      stdout,
      stderr,
      result: result === undefined ? '' : String(result),
    }) + '\n');
  } catch (error) {
    console.log = originalLog;
    console.error = originalError;
    process.stdout.write(JSON.stringify({
      ok: false,
      id: req === null || req.id === undefined ? null : req.id,
      stdout,
      stderr,
      error: error && error.stack ? error.stack : String(error),
    }) + '\n');
  }
});
"#;
