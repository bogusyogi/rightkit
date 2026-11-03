//! `<engine> local-model …` is refused in every build. The debug-only probe of the machine-wide
//! local-image model-server lock was removed with the debug-assertion switches; the release
//! answer (`unknown_command`, exit 2) is kept exactly.

use std::io::Write;
use std::path::Path;

use serde_json::{json, Value};

/// The engine word that the removed probe answered to.
pub const PROBE_WORD: &str = "local-model";

fn print(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// Refuses every invocation with `unknown_command`, as a release build always did.
pub fn run(_args: &[String], _data_dir: &Path) -> i32 {
    print(&json!({"error": {
        "code": "unknown_command",
        "message": "unknown command: local-model",
        "details": Value::Null,
    }}));
    2
}
