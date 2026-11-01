//! Live lever testing (QA build only; the module exists only with `test-mode`).
//!
//! A test-mode shell forwards the owner's `RIGHTKIT_*` switches to the engine it starts
//! (rightkit-dictation-app-runtime `engine_supervisor::env::pass_lever_env`). At startup the engine records
//! the set it actually received, once, so every log line after it is attributable to a lever set:
//!
//! - stderr (reaches the shell's `events.jsonl` as `sidecar_stderr`): `engine_levers count=N` and
//!   one `engine_lever NAME=VALUE` line per lever. The shell's log scrubber redacts any word that
//!   contains `token`, so a name such as `RIGHTKIT_KWS_THRESHOLD_TOKENS` reads `[redacted]` there;
//! - `<RIGHTKIT_APP_DATA_DIR>/engine-levers.jsonl`: one unredacted JSON line per engine start
//!   (`ts_ms`, `pid`, `role`, `levers`), the authoritative record. Written only when the data
//!   root is the explicit QA override.

use std::collections::BTreeMap;
use std::io::Write;

/// Every `RIGHTKIT_*` variable in `vars`, sorted by name (non-UTF-8 entries skipped).
pub fn collect(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> BTreeMap<String, String> {
    vars.into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| name.starts_with("RIGHTKIT_"))
        .collect()
}

/// The `engine-levers.jsonl` record for one engine start.
pub fn record(ts_ms: u128, pid: u32, role: &str, levers: &BTreeMap<String, String>) -> String {
    serde_json::json!({
        "event": "engine_levers",
        "ts_ms": ts_ms,
        "pid": pid,
        "role": role,
        "levers": levers,
    })
    .to_string()
}

/// The stderr lines for one engine start.
pub fn stderr_lines(levers: &BTreeMap<String, String>) -> Vec<String> {
    std::iter::once(format!("engine_levers count={}", levers.len()))
        .chain(
            levers
                .iter()
                .map(|(name, value)| format!("engine_lever {name}={value}")),
        )
        .collect()
}

/// Record this process's lever set (stderr + `engine-levers.jsonl`). Never fails the engine.
pub fn log_startup() {
    let levers = collect(std::env::vars_os());
    for line in stderr_lines(&levers) {
        eprintln!("{line}");
    }
    // Only into an isolated QA data root: never a file in the real per-user folder.
    if !rightkit_dictation_core::app_paths::app_data_root_is_explicit_override() {
        eprintln!("engine_levers_file_skipped reason=no_HR_APP_DATA_DIR");
        return;
    }
    let role = std::env::args().nth(1).unwrap_or_else(|| "engine".into());
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = rightkit_dictation_core::app_paths::app_data_root().join("engine-levers.jsonl");
    let line = record(ts_ms, std::process::id(), &role, &levers);
    let written = std::fs::create_dir_all(path.parent().unwrap_or(&path))
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        })
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = written {
        eprintln!("engine_levers_file_failed error={error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn collects_only_hr_switches_sorted() {
        let levers = collect(vec![
            (OsString::from("RIGHTKIT_KWS_ONSET"), OsString::from("1")),
            (OsString::from("PATH"), OsString::from("/bin")),
            (
                OsString::from("RIGHTKIT_KWS_BACKEND"),
                OsString::from("rust"),
            ),
            (OsString::from("RIGHTKIT_PASS3"), OsString::from("0")),
        ]);
        assert_eq!(
            levers.into_iter().collect::<Vec<_>>(),
            [
                ("RIGHTKIT_KWS_BACKEND".to_string(), "rust".to_string()),
                ("RIGHTKIT_KWS_ONSET".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn record_and_stderr_name_every_lever() {
        let levers = collect(vec![
            (
                OsString::from("RIGHTKIT_KWS_THRESHOLD_TOKENS"),
                OsString::from("3"),
            ),
            (OsString::from("RIGHTKIT_KWS_ONSET"), OsString::from("1")),
        ]);
        let parsed: serde_json::Value =
            serde_json::from_str(&record(7, 42, "engine", &levers)).unwrap();
        assert_eq!(parsed["event"], "engine_levers");
        assert_eq!(parsed["pid"], 42);
        assert_eq!(parsed["levers"]["RIGHTKIT_KWS_THRESHOLD_TOKENS"], "3");
        assert_eq!(
            stderr_lines(&levers),
            [
                "engine_levers count=2",
                "engine_lever RIGHTKIT_KWS_ONSET=1",
                "engine_lever RIGHTKIT_KWS_THRESHOLD_TOKENS=3",
            ]
        );
    }
}
