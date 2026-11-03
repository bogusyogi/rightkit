//! The `secret_absent` step: prove a secret reached no result, no file the scenario produced and no
//! `cli` step output.
//!
//! What is scanned, byte for byte and in every encoding in `forms`:
//! * the scenario's whole data directory (stores, databases, write-ahead logs, engine logs,
//!   settings, crash reports: every regular file, symlinks not followed);
//! * the scenario's log directory, which holds the **full** stdout and stderr of every `cli` step
//!   (`NN-cli.log`, `NN-seed-NN.log`), whether the call was served by the persistent `--stdio`
//!   sidecar or ran cold, plus the daemon, plugin and spawned-child logs. The scan reads these step
//!   logs and never an in-memory buffer: the sidecar keeps only the tail of its stderr, so a scan of
//!   that buffer would pass a leak that scrolled out of it. A log that says it was cut
//!   ([`CUT_MARKER`]) fails the step, because a cut log proves nothing; so does a step log the `cli`
//!   step registered ([`super::StepState::note_cli_log`]) that is not on disk.
//! * the saved step results named in `vars` (their JSON text).
//!
//! Fields: `needle` (the secret, at least 12 characters so a match means something), `vars`,
//! `min_files` (default 1: how many files the scan must have seen, so an empty or misplaced
//! directory cannot pass), `min_cli_logs` (default 0: how many `cli` step logs it must have seen),
//! `forms` (default all of `raw`, `base64`, `url`, `hex`, `sha256`; see [`crate::util::secret_forms`]),
//! `skip` (file-name prefixes to leave out of the log directory scan, for the output of a process
//! that is not the engine, such as a mock the plugin spawned: `skip = ["spawn-mock-platform"]`; the
//! data directory is always scanned whole).
//!
//! A failure names the variable or file, never the secret. Result `{ absent, files_scanned,
//! bytes_scanned, cli_logs_scanned, vars_checked, needle_len, forms_checked }`. Not covered: a copy
//! split across fields or lines.
use super::{only, required, strings, uint};
use crate::scenario::Ctx;
use crate::util::{err, secret_forms, sha256_hex, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Shortest `needle` accepted: a shorter one matches ordinary text and proves nothing.
pub const SECRET_MIN_LEN: usize = 12;
/// Largest single file the scan reads, and most files it walks.
const FILE_MAX: u64 = 64 * 1024 * 1024;
const FILES_MAX: usize = 20_000;
const ALL_FORMS: [&str; 5] = ["raw", "base64", "url", "hex", "sha256"];
/// What the engine's capped stderr buffer writes where it dropped earlier output
/// (`[N bytes of earlier stderr dropped]`). A step log carrying it is incomplete.
pub const CUT_MARKER: &str = "bytes of earlier stderr dropped]";

/// What one scan covers.
pub(super) struct Scope<'a> {
    pub data_dir: &'a Path,
    pub log_dir: &'a Path,
    pub vars: &'a BTreeMap<String, Value>,
    /// Logs the `cli` steps registered.
    pub cli_logs: &'a [PathBuf],
}

pub(super) fn run(ctx: &mut Ctx, p: &Map<String, Value>) -> Result<Value> {
    let scope = Scope {
        data_dir: &ctx.data_dir,
        log_dir: &ctx.log_dir,
        vars: &ctx.vars,
        cli_logs: ctx.steps.cli_logs(),
    };
    run_with(&scope, p)
}

/// Every regular file under `dir` (symlinks are not followed), appended to `out`, leaving out the
/// top-level entries whose name starts with a `skip` prefix. A missing directory holds nothing.
fn walk(dir: &Path, skip: &[String], top: bool, out: &mut BTreeSet<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return err(format!("read {}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| Error(format!("read {}: {e}", dir.display())))?;
        let kind = entry
            .file_type()
            .map_err(|e| Error(format!("stat {}: {e}", entry.path().display())))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if top && skip.iter().any(|prefix| name.starts_with(prefix.as_str())) {
            continue;
        }
        if kind.is_dir() {
            walk(&path, skip, false, out)?;
        } else if kind.is_file() {
            out.insert(path);
        }
        if out.len() > FILES_MAX {
            return err(format!(
                "more than {FILES_MAX} files under {}: refusing to scan",
                dir.display()
            ));
        }
    }
    Ok(())
}

/// A `cli` step log by name: `NN-cli.log` or `NN-seed-NN.log`.
fn is_cli_log(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy())
        .is_some_and(|n| n.ends_with("-cli.log") || (n.contains("-seed-") && n.ends_with(".log")))
}

/// The first form of `secret` found in `haystack`, limited to the `wanted` groups. The encodings,
/// including the base64 forms at every stream alignment, come from [`secret_forms`]; the SHA-256
/// digest is checked here.
pub(crate) fn leaked_form(haystack: &[u8], secret: &str, wanted: &[String]) -> Option<String> {
    let has = |group: &str| wanted.iter().any(|f| f == group);
    let in_wanted_group = |name: &str| {
        if name == "raw" {
            has("raw")
        } else if name.starts_with("base64") {
            has("base64")
        } else if name.starts_with("percent") || name.starts_with("form") {
            has("url")
        } else if name.starts_with("hex") {
            has("hex")
        } else {
            false
        }
    };
    let found = secret_forms(secret).into_iter().find(|form| {
        in_wanted_group(form.name)
            && !form.text.is_empty()
            && haystack
                .windows(form.text.len())
                .any(|w| w == form.text.as_bytes())
    });
    if let Some(form) = found {
        return Some(form.name.to_string());
    }
    let digest = sha256_hex(secret.as_bytes());
    (has("sha256")
        && !digest.is_empty()
        && haystack
            .windows(digest.len())
            .any(|w| w == digest.as_bytes()))
    .then(|| "sha256".to_string())
}

pub(super) fn run_with(scope: &Scope, p: &Map<String, Value>) -> Result<Value> {
    only(
        p,
        "secret_absent",
        &[
            "needle",
            "vars",
            "min_files",
            "min_cli_logs",
            "forms",
            "skip",
        ],
    )?;
    let needle = required(p, "secret_absent", "needle")?;
    let wanted: Vec<String> = if p.get("forms").is_some_and(|v| !v.is_null()) {
        strings(p, "forms")?
    } else {
        ALL_FORMS.iter().map(|f| f.to_string()).collect()
    };
    if wanted.is_empty() || wanted.iter().any(|f| !ALL_FORMS.contains(&f.as_str())) {
        return err("`forms` must list some of raw, base64, url, hex, sha256");
    }
    if needle.len() < SECRET_MIN_LEN {
        return err(format!(
            "`needle` must be at least {SECRET_MIN_LEN} characters (got {})",
            needle.len()
        ));
    }
    let to_count = |key: &str| -> Result<usize> {
        usize::try_from(uint(p, key)?.unwrap_or(0)).map_err(|e| Error(format!("`{key}`: {e}")))
    };
    let min_files = if p.get("min_files").is_some_and(|v| !v.is_null()) {
        to_count("min_files")?
    } else {
        1
    };
    let min_cli_logs = to_count("min_cli_logs")?;
    let skip = strings(p, "skip")?;
    let var_names = strings(p, "vars")?;
    for name in &var_names {
        let value = scope.vars.get(name).ok_or_else(|| {
            Error(format!(
                "secret_absent: no saved step result named `{name}`"
            ))
        })?;
        let text = value.to_string();
        if let Some(form) = leaked_form(text.as_bytes(), &needle, &wanted) {
            return err(format!(
                "the secret ({form} form) appears in the saved result `{name}`"
            ));
        }
    }

    let mut data_files: BTreeSet<PathBuf> = BTreeSet::new();
    walk(scope.data_dir, &[], false, &mut data_files)?;
    let mut log_files: BTreeSet<PathBuf> = BTreeSet::new();
    walk(scope.log_dir, &skip, true, &mut log_files)?;
    // A step log the `cli` step registered must be there, skip or not: a call with no log would
    // hide whatever it printed.
    for registered in scope.cli_logs {
        if !registered.is_file() {
            return err(format!(
                "the cli step log {} is missing, so the scan could not see that call's output",
                registered.display()
            ));
        }
        log_files.insert(registered.clone());
    }
    let cli_logs: BTreeSet<&PathBuf> = log_files.iter().filter(|f| is_cli_log(f)).collect();
    let cli_logs_found = cli_logs.len();
    if cli_logs_found < min_cli_logs {
        return err(format!(
            "secret_absent saw {cli_logs_found} cli step log(s), fewer than the {min_cli_logs} required: the scan would prove nothing"
        ));
    }
    let all: BTreeSet<&PathBuf> = data_files.iter().chain(log_files.iter()).collect();
    if all.len() < min_files {
        return err(format!(
            "secret_absent scanned {} file(s) under the scenario data and log directories, fewer than the {min_files} required: the scan would prove nothing",
            all.len()
        ));
    }
    let mut bytes_scanned: u64 = 0;
    let mut files_scanned: u64 = 0;
    for path in all {
        let length = match std::fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return err(format!("stat {}: {e}", path.display())),
        };
        if length > FILE_MAX {
            return err(format!(
                "{} is {length} bytes, over the {FILE_MAX} byte scan bound",
                path.display()
            ));
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return err(format!("read {}: {e}", path.display())),
        };
        if log_files.contains(path)
            && bytes
                .windows(CUT_MARKER.len())
                .any(|w| w == CUT_MARKER.as_bytes())
        {
            return err(format!(
                "the log {} says its earlier output was dropped: a cut log proves nothing, so the scan refuses it",
                path.display()
            ));
        }
        if let Some(form) = leaked_form(&bytes, &needle, &wanted) {
            return err(format!(
                "the secret ({form} form) appears in the file {}",
                path.display()
            ));
        }
        files_scanned += 1;
        bytes_scanned += length;
    }
    Ok(json!({
        "absent": true,
        "files_scanned": files_scanned,
        "bytes_scanned": bytes_scanned,
        "cli_logs_scanned": cli_logs_found,
        "vars_checked": var_names.len(),
        "needle_len": needle.len(),
        "forms_checked": wanted.len(),
    }))
}
