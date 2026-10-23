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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{base64_encode, new_id};

    const SECRET: &str = "s3cr3t-token-0123456789";

    struct Dirs {
        data: PathBuf,
        log: PathBuf,
    }

    impl Dirs {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("rkqa-secret-{}", new_id()));
            let (data, log) = (root.join("data"), root.join("log"));
            std::fs::create_dir_all(&data).unwrap();
            std::fs::create_dir_all(&log).unwrap();
            Self { data, log }
        }

        fn scope<'a>(&'a self, vars: &'a BTreeMap<String, Value>, cli: &'a [PathBuf]) -> Scope<'a> {
            Scope {
                data_dir: &self.data,
                log_dir: &self.log,
                vars,
                cli_logs: cli,
            }
        }

        fn cli_log(&self, name: &str, stdout: &str, stderr: &str) -> PathBuf {
            let path = self.log.join(name);
            std::fs::write(
                &path,
                format!("$ cmd\nexit Some(0)\n--- stdout\n{stdout}\n--- stderr\n{stderr}\n"),
            )
            .unwrap();
            path
        }
    }

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn needle() -> Map<String, Value> {
        params(json!({ "needle": SECRET }))
    }

    #[test]
    fn a_clean_scenario_passes_and_reports_what_it_scanned() {
        let d = Dirs::new();
        std::fs::write(d.data.join("store.db"), "rows without it").unwrap();
        let log = d.cli_log("01-cli.log", "{}", "progress");
        let vars = BTreeMap::new();
        let cli = [log];
        let r = run_with(
            &d.scope(&vars, &cli),
            &params(json!({"needle": SECRET, "min_cli_logs": 1})),
        )
        .unwrap();
        assert_eq!(r["absent"], true);
        assert_eq!(r["files_scanned"], 2);
        assert_eq!(r["cli_logs_scanned"], 1);
    }

    #[test]
    fn a_secret_in_a_cli_step_log_is_found_in_stdout_and_stderr_and_never_printed() {
        let vars = BTreeMap::new();
        for (stdout, stderr) in [(SECRET, ""), ("", SECRET)] {
            let d = Dirs::new();
            let log = d.cli_log("03-cli.log", stdout, stderr);
            let cli = [log];
            let e = run_with(&d.scope(&vars, &cli), &needle()).unwrap_err();
            assert!(
                e.0.contains("03-cli.log") && e.0.contains("raw form"),
                "{}",
                e.0
            );
            assert!(!e.0.contains(SECRET), "{}", e.0);
        }
    }

    /// The sidecar keeps only the last 256 KiB of its stderr in memory. A leak early in a long
    /// call is gone from that buffer but is in the step log, which the scan reads.
    #[test]
    fn a_leak_beyond_a_capped_buffers_reach_is_found_because_the_scan_reads_the_whole_log() {
        let d = Dirs::new();
        let filler = "x".repeat(600 * 1024);
        let log = d.cli_log("02-cli.log", "{}", &format!("{SECRET}\n{filler}"));
        let vars = BTreeMap::new();
        let cli = [log];
        let e = run_with(&d.scope(&vars, &cli), &needle()).unwrap_err();
        assert!(e.0.contains("02-cli.log"), "{}", e.0);
    }

    #[test]
    fn a_log_that_says_it_was_cut_is_refused_because_it_proves_nothing() {
        let d = Dirs::new();
        let log = d.cli_log(
            "04-cli.log",
            "{}",
            "[1500 bytes of earlier stderr dropped]\nlast lines",
        );
        let vars = BTreeMap::new();
        let cli = [log];
        let e = run_with(&d.scope(&vars, &cli), &needle()).unwrap_err();
        assert!(e.0.contains("a cut log proves nothing"), "{}", e.0);
    }

    #[test]
    fn a_registered_cli_log_that_is_not_on_disk_fails_even_when_skipped() {
        let d = Dirs::new();
        std::fs::write(d.data.join("f"), "x").unwrap();
        let vars = BTreeMap::new();
        let cli = [d.log.join("05-cli.log")];
        let e = run_with(
            &d.scope(&vars, &cli),
            &params(json!({"needle": SECRET, "skip": ["05-"]})),
        )
        .unwrap_err();
        assert!(e.0.contains("is missing"), "{}", e.0);
    }

    #[test]
    fn encoded_copies_are_found_in_the_data_directory() {
        let vars = BTreeMap::new();
        let cases: Vec<(String, &str)> = vec![
            (base64_encode(SECRET.as_bytes()), "base64"),
            (rightkit_fs::digest::hex(SECRET.as_bytes()), "hex_lower"),
            (sha256_hex(SECRET.as_bytes()), "sha256"),
        ];
        for (copy, form) in cases {
            let d = Dirs::new();
            std::fs::create_dir_all(d.data.join("nested")).unwrap();
            std::fs::write(d.data.join("nested").join("wal"), format!("..{copy}..")).unwrap();
            let e = run_with(&d.scope(&vars, &[]), &needle()).unwrap_err();
            assert!(
                e.0.contains("nested") && !e.0.contains(SECRET),
                "{form}: {}",
                e.0
            );
        }
        // A form left out of `forms` is not searched.
        let d = Dirs::new();
        std::fs::write(d.data.join("wal"), base64_encode(SECRET.as_bytes())).unwrap();
        let ok = run_with(
            &d.scope(&vars, &[]),
            &params(json!({"needle": SECRET, "forms": ["raw"]})),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn a_saved_result_that_holds_the_secret_fails_by_name() {
        let d = Dirs::new();
        std::fs::write(d.data.join("f"), "x").unwrap();
        let mut vars = BTreeMap::new();
        vars.insert("enrol".to_string(), json!({"code": SECRET}));
        let e = run_with(
            &d.scope(&vars, &[]),
            &params(json!({"needle": SECRET, "vars": ["enrol"]})),
        )
        .unwrap_err();
        assert!(
            e.0.contains("saved result `enrol`") && !e.0.contains(SECRET),
            "{}",
            e.0
        );
        let e = run_with(
            &d.scope(&vars, &[]),
            &params(json!({"needle": SECRET, "vars": ["nope"]})),
        )
        .unwrap_err();
        assert!(e.0.contains("no saved step result named `nope`"), "{}", e.0);
    }

    #[test]
    fn skip_leaves_a_non_engine_log_out_of_the_log_scan_only() {
        let vars = BTreeMap::new();
        let d = Dirs::new();
        std::fs::write(d.data.join("f"), "x").unwrap();
        std::fs::write(d.log.join("spawn-mock-platform-1.stderr.log"), SECRET).unwrap();
        // Scanned by default.
        assert!(run_with(&d.scope(&vars, &[]), &needle()).is_err());
        // Skipped when the scenario says so.
        let ok = run_with(
            &d.scope(&vars, &[]),
            &params(json!({"needle": SECRET, "skip": ["spawn-mock-platform"]})),
        );
        assert!(ok.is_ok(), "{:?}", ok.err());
        // The data directory has no skip: the same name there is scanned.
        std::fs::write(d.data.join("spawn-mock-platform-1.stderr.log"), SECRET).unwrap();
        assert!(run_with(
            &d.scope(&vars, &[]),
            &params(json!({"needle": SECRET, "skip": ["spawn-mock-platform"]}))
        )
        .is_err());
    }

    #[test]
    fn minimums_and_input_checks_refuse_a_scan_that_would_prove_nothing() {
        let vars = BTreeMap::new();
        let d = Dirs::new();
        let scope = d.scope(&vars, &[]);
        // Nothing to scan: min_files defaults to 1.
        assert!(run_with(&scope, &needle())
            .unwrap_err()
            .0
            .contains("fewer than the 1 required"));
        std::fs::write(d.data.join("f"), "x").unwrap();
        assert!(run_with(
            &scope,
            &params(json!({"needle": SECRET, "min_cli_logs": 1}))
        )
        .is_err());
        assert!(run_with(&scope, &params(json!({"needle": "short"}))).is_err());
        assert!(run_with(&scope, &params(json!({"needle": SECRET, "forms": []}))).is_err());
        assert!(run_with(
            &scope,
            &params(json!({"needle": SECRET, "forms": ["rot13"]}))
        )
        .is_err());
        assert!(run_with(&scope, &params(json!({"needle": SECRET, "typo": 1}))).is_err());
        assert!(run_with(&scope, &params(json!({}))).is_err());
        assert!(run_with(&scope, &needle()).is_ok());
    }
}
