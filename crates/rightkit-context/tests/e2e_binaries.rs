//! End-to-end: the one shipped `membrane` executable as a real child process.
//!
//! Every invocation here is one the binary answers locally, against an isolated state root with
//! no resident, so the suite proves the process-level contract hosts depend on: exit codes and
//! the stdout/stderr split, without touching an installed product.
//!
//! The release binary reads no environment override for its stores, so every call isolates itself
//! the way a user's machine is laid out: its own home folder (the state root is resolved from the
//! per-user data folders under it), and, for the journey that opens the Cortex store, a copy of
//! the binary in a `Helpers` folder, where the host app puts it.

mod common;

use rightkit_child_process::{command_std, OutputCapture, OwnedCommand};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn run(state: &std::path::Path, args: &[&str]) -> Output {
    let mut command = command_std(env!("CARGO_BIN_EXE_membrane"));
    common::isolate_home(&mut command, state);
    command
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn membrane")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_and_help_succeed_on_stdout() {
    let state = tempfile::tempdir().unwrap();
    let version = run(state.path(), &["--version"]);
    assert!(version.status.success());
    assert!(text(&version.stdout).trim().starts_with("membrane "));
    let help = run(state.path(), &["--help"]);
    assert!(help.status.success());
    let help = text(&help.stdout);
    for command in [
        "context",
        "memory",
        "blueprint",
        "status",
        "hook",
        "serve",
        "init",
    ] {
        assert!(
            help.contains(command),
            "{command} missing from help: {help}"
        );
    }
}

#[test]
fn removed_commands_are_usage_errors() {
    let state = tempfile::tempdir().unwrap();
    for command in [
        "install",
        "uninstall",
        "activate",
        "update",
        "stdio-mcp",
        "cli",
    ] {
        let out = run(state.path(), &[command]);
        assert_eq!(
            out.status.code(),
            Some(rightkit_context::EXIT_USER_ERROR),
            "{command}"
        );
        assert!(
            out.stdout.is_empty(),
            "{command}: usage errors never write stdout"
        );
    }
}

#[test]
fn serve_rejects_the_old_parent_arguments() {
    let state = tempfile::tempdir().unwrap();
    let out = run(state.path(), &["serve", "--parent-pid", "1"]);
    assert_eq!(out.status.code(), Some(rightkit_context::EXIT_USER_ERROR));
}

#[test]
fn status_json_reports_an_absent_resident() {
    let state = tempfile::tempdir().unwrap();
    let out = run(state.path(), &["status", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let status: Value = serde_json::from_slice(&out.stdout).expect("status is one JSON object");
    assert_ne!(status["resident"]["running"], Value::Bool(true), "{status}");
}

#[test]
fn a_bad_hook_command_line_still_exits_zero() {
    let state = tempfile::tempdir().unwrap();
    let out = run(state.path(), &["hook", "--wire", "nowhere"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

// G-01 journey (implementation-plan.md "G-01 reproduction & candidate recipe"): one disposable
// fixture, the real binary as a child process per call, no hand-forged grant or identity.

const G01_REPOSITORY: &str = "membrane-g01";
const G01_SCOPE: &str = "scope:qa/membrane-g01";
const G01_RULE: &str = "For membrane-g01, report evidence before completion.";
const G01_PROMPT: &str = "What completion rule applies to membrane-g01?";
const CALL_DEADLINE: Duration = Duration::from_secs(10);
const JOURNEY_BUDGET: Duration = Duration::from_secs(120);
/// How long an output pipe may stay open after its child ended or was terminated.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// The most a call's stdout or stderr keeps (the rest is drained and discarded).
const OUTPUT_CAP: usize = 4 * 1024 * 1024;
const G01_BUDGET: u64 = 512;
/// The caller's request id for the receipt journey step (`--wire coderight --request-id`).
const G01_REQUEST_ID: &str = "g01-request-1";
/// Memories pushed beside the rule to prove what must NOT be delivered: an unrelated memory in the
/// same scope, a strong keyword match under another scope, and a matching memory that is deleted.
const G01_OTHER_SCOPE: &str = "scope:qa/other";
const G01_UNRELATED: &str = "Office coffee beans arrive on Tuesday.";
const G01_OTHER_SCOPE_TEXT: &str =
    "Completion rule for the other scope: skip the evidence report. OTHER-SCOPE-SENTINEL";
const G01_DELETED_TEXT: &str =
    "Retired completion rule: no evidence needed before completion. DELETED-SENTINEL";
/// A prompt whose words match no memory in the populated fixture store: a genuine no-match answer.
const G01_NO_MATCH_PROMPT: &str = "zebra quantum xylophone";
/// Far below what any `--json` document costs, yet text mode (an empty answer is zero bytes) fits.
const G01_TINY_BUDGET: u64 = 4;

struct Called {
    argv: String,
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// The deadline passed first: the child and its descendants were terminated.
    timed_out: bool,
    /// An output pipe was still open `PIPE_GRACE` after the child ended: a descendant holds it.
    pipes_open: bool,
    /// Processes other than the child still running when it ended (recorded, then terminated).
    survivors: Vec<u32>,
}

impl Called {
    fn describe(&self) -> String {
        let mut notes = String::new();
        if self.timed_out {
            notes.push_str("\n(deadline passed: the child and its descendants were terminated)");
        }
        if self.pipes_open {
            notes.push_str("\n(an output pipe is still held open by a descendant)");
        }
        if !self.survivors.is_empty() {
            notes.push_str(&format!(
                "\n(descendants {:?} outlived the child; terminated)",
                self.survivors
            ));
        }
        format!(
            "{}: exit {:?}\nstdout: {}\nstderr: {}{notes}",
            self.argv,
            self.code,
            self.stdout.trim(),
            self.stderr.trim()
        )
    }

    fn json(&self) -> Value {
        serde_json::from_str(self.stdout.trim()).unwrap_or_else(|error| {
            panic!(
                "stdout is not one JSON document ({error}): {}",
                self.describe()
            )
        })
    }
}

/// One child process owned for its whole life: it and everything it starts are contained (a
/// process group on Unix, a job object on Windows) and tied to this test process, so neither a
/// deadline nor a dying test leaves one behind. Output is captured on bounded drains.
///
/// The child gets `limit` to exit on its own. If it does, the descendants it left (recorded
/// first, as evidence) are terminated before the leader is reaped; if it does not, the whole
/// tree is terminated. Pipes a descendant still holds are reported after `PIPE_GRACE`, never
/// waited on.
fn run_owned(argv: String, mut command: Command, stdin: Option<&str>, limit: Duration) -> Called {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut owned = OwnedCommand::from_command(command);
    owned.kill_on_parent_exit(true);
    let mut child = owned
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {argv}: {error}"));
    let mut stdout = OutputCapture::capture(child.take_stdout().expect("stdout pipe"), OUTPUT_CAP);
    let mut stderr = OutputCapture::capture(child.take_stderr().expect("stderr pipe"), OUTPUT_CAP);
    if let Some(input) = stdin {
        let mut pipe = child.take_stdin().expect("stdin pipe");
        // A child that exits before reading is judged by its exit code, not by the write.
        let _ = pipe.write_all(input.as_bytes());
    }
    let exited = child
        .process_handle()
        .and_then(|handle| handle.wait_exited_timeout(limit))
        .unwrap_or_else(|error| panic!("wait for {argv}: {error}"));
    // The leader is observed, not yet reaped, so the group is still reachable: list what else
    // is running, then bring the tree down and reap.
    let survivors = if exited {
        child.tree_members().unwrap_or_default()
    } else {
        Vec::new()
    };
    let status = child
        .terminate_tree()
        .unwrap_or_else(|error| panic!("terminate {argv}: {error}"));
    let out = stdout.finish_within(PIPE_GRACE);
    let err = stderr.finish_within(PIPE_GRACE);
    Called {
        argv,
        code: if exited { status.code() } else { None },
        stdout: text(&out.bytes),
        stderr: text(&err.bytes),
        timed_out: !exited,
        pipes_open: !stdout.is_finished() || !stderr.is_finished(),
        survivors,
    }
}

struct G01 {
    _parent: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    catalog: PathBuf,
    /// The journey's wall clock starts here; `JOURNEY_BUDGET` is a deadline on every call.
    started: Instant,
    /// Every call's captured output, so any later failure can carry it.
    log: Mutex<Vec<String>>,
}

impl G01 {
    fn transcript(&self) -> String {
        self.log
            .lock()
            .map(|log| log.join("\n---\n"))
            .unwrap_or_default()
    }

    fn budget_left(&self) -> Duration {
        (self.started + JOURNEY_BUDGET).saturating_duration_since(Instant::now())
    }

    fn registry(&self) -> PathBuf {
        self.catalog.join("project-registry.json")
    }

    /// The one Cortex store under the state root (`cortex_db_path`: the state root's
    /// `tools/.cache/memory/cortex-engine.db`).
    fn cortex_db(&self) -> PathBuf {
        self.state
            .join("tools")
            .join(".cache")
            .join("memory")
            .join("cortex-engine.db")
    }

    /// The retained receipt file a `--wire coderight --request-id <id>` pull would write.
    fn receipt_file(&self, id: &str) -> PathBuf {
        self.state
            .join("receipts")
            .join("coderight")
            .join(format!("{id}.json"))
    }

    /// Every file the fixture's state root and catalog hold, relative and prefixed by which: a
    /// read-only call must leave this exactly as it found it.
    fn footprint(&self) -> BTreeSet<PathBuf> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.insert(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
                }
            }
        }
        let mut out = BTreeSet::new();
        for (name, dir) in [("state", &self.state), ("catalog", &self.catalog)] {
            let mut files = BTreeSet::new();
            walk(dir, dir, &mut files);
            out.extend(files.into_iter().map(|file| Path::new(name).join(file)));
        }
        out
    }

    /// The time one call may take: its own deadline, capped by what is left of the journey
    /// budget. A call that would start with none left fails the journey.
    fn call_limit(&self, what: &str) -> Duration {
        let limit = CALL_DEADLINE.min(self.budget_left());
        assert!(
            !limit.is_zero(),
            "journey wall clock {JOURNEY_BUDGET:?} exhausted before {what}\n{}",
            self.transcript()
        );
        limit
    }

    /// One owned child process under a 10 s deadline, itself capped by what is left of the
    /// journey budget. A deadline terminates the child and everything it started, and fails
    /// with the output so far.
    fn call(&self, args: &[&str], stdin: Option<&str>) -> Called {
        let argv = format!("membrane {}", args.join(" "));
        let limit = self.call_limit(&argv);
        // The copy under `Helpers` is the binary the host app runs; it resolves its state root from
        // the home folder below and reads no environment override for any store.
        let mut command = command_std(common::installed_binary(&self.home));
        common::isolate_home(&mut command, &self.home);
        command
            .args(args)
            .env("RIGHTKIT_NO_KEYCHAIN", "1")
            .env("CFFIXED_USER_HOME", &self.home)
            .env("RIGHTKIT_QA_HOME", &self.home);
        let called = run_owned(argv, command, stdin, limit);
        println!("[g01] {}", called.describe());
        if let Ok(mut log) = self.log.lock() {
            log.push(called.describe());
        }
        assert!(
            !called.timed_out,
            "deadline of {limit:?} exceeded (call {CALL_DEADLINE:?}, journey {JOURNEY_BUDGET:?}): {}",
            called.describe()
        );
        assert!(!called.pipes_open, "{}", called.describe());
        assert!(
            called.survivors.is_empty(),
            "a descendant outlived the call: {}",
            called.describe()
        );
        called
    }
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Pids of every process named `membrane` in a process listing (`ps` on Unix, `tasklist` on
/// Windows) read under the journey's deadlines like any other call.
fn membrane_pids(journey: &G01) -> BTreeSet<u32> {
    let mut command = command_std(if cfg!(windows) { "tasklist" } else { "ps" });
    if cfg!(windows) {
        command.args(["/FO", "CSV", "/NH"]);
    } else {
        command.args(["-axo", "pid=,comm="]);
    }
    let argv = if cfg!(windows) { "tasklist" } else { "ps" }.to_owned();
    let limit = journey.call_limit(&argv);
    let listing = run_owned(argv, command, None, limit);
    assert!(
        !listing.timed_out && !listing.pipes_open && listing.code == Some(0),
        "{}\n{}",
        listing.describe(),
        journey.transcript()
    );
    let mut pids = BTreeSet::new();
    for line in listing.stdout.lines() {
        let (pid, name) = if cfg!(windows) {
            let mut fields = line.split(',').map(|field| field.trim().trim_matches('"'));
            match (fields.next(), fields.next()) {
                (Some(name), Some(pid)) => (pid.to_owned(), name.to_owned()),
                _ => continue,
            }
        } else {
            match line.trim().split_once(char::is_whitespace) {
                Some((pid, name)) => (pid.to_owned(), name.trim().to_owned()),
                None => continue,
            }
        };
        let image = Path::new(&name)
            .file_name()
            .map(|file| file.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if let (Ok(pid), "membrane" | "membrane.exe") = (pid.parse::<u32>(), image.as_str()) {
            pids.insert(pid);
        }
    }
    pids
}

/// A memory that must not reach the consumer: what it is, its id and a marker of its text.
type Forbidden<'a> = (&'a str, &'a str, &'a str);

/// A pull that delivered: exit 0, `membrane.context.pull.v1`, the pushed Cortex memory among the
/// packet sources (so a rule file cannot mask a broken Cortex read), its exact body in the
/// context, none of the `forbidden` memories among the sources or in the context, and the declared
/// 512-token budget met by the COMPLETE response: the child's whole stdout (the `--json` document
/// with its detail, accounting and receipt) is recounted here, independently of the binary, with
/// the same exact o200k_base/1 counter, and must equal the reported whole-response accounting.
fn assert_pull_delivers(step: &str, pull: &Called, pushed: &Value, forbidden: &[Forbidden<'_>]) {
    assert_eq!(
        pull.code,
        Some(rightkit_context::EXIT_OK),
        "{step}: {}",
        pull.describe()
    );
    let doc = pull.json();
    assert_eq!(
        doc["schema"].as_str(),
        Some("membrane.context.pull.v1"),
        "{step}: {}",
        pull.describe()
    );
    let context = doc["context"].as_str().unwrap_or_default();
    let hint = if doc["detail"].as_str() == Some("no memory store yet") {
        " (pull found no memory store: it does not read the store push wrote)"
    } else {
        ""
    };
    assert!(
        context.contains(G01_RULE),
        "{step}: delivered context does not contain the exact pushed body{hint}: {}",
        pull.describe()
    );
    let reason = doc["reason"].as_str().unwrap_or_default();
    assert!(
        !reason.is_empty() && reason != "empty" && reason != "membrane_no_matches",
        "{step}: reason {reason:?} says no context was found: {}",
        pull.describe()
    );
    let detail = &doc["detail"];
    let sources = detail["sources"].as_array().unwrap_or_else(|| {
        panic!(
            "{step}: detail.sources is not an array: {}",
            pull.describe()
        )
    });
    assert!(
        !sources.is_empty(),
        "{step}: detail.sources is empty: {}",
        pull.describe()
    );
    for source in sources {
        for field in ["id", "sourceRef", "sourceHash"] {
            assert!(
                source[field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "{step}: detail.sources[].{field} missing: {}",
                pull.describe()
            );
        }
    }
    // The Cortex candidate id is `memory:role:<memory id>` (federation.rs, memory candidates).
    let pushed_id = pushed["id"].as_str().unwrap_or_default();
    assert!(
        !pushed_id.is_empty()
            && sources.iter().any(|source| {
                source["id"]
                    .as_str()
                    .is_some_and(|id| id.contains(pushed_id))
            }),
        "{step}: no detail.sources[].id carries the pushed memory id {pushed_id:?}: {}",
        pull.describe()
    );
    for (what, id, marker) in forbidden {
        assert!(
            !sources.iter().any(|source| source["id"]
                .as_str()
                .is_some_and(|source_id| source_id.contains(id))),
            "{step}: the {what} memory {id} was delivered as a source: {}",
            pull.describe()
        );
        assert!(
            !context.contains(marker),
            "{step}: the {what} memory's text ({marker:?}) was delivered in the context: {}",
            pull.describe()
        );
    }
    assert!(
        detail["omissions"].is_array(),
        "{step}: detail.omissions is not an array: {}",
        pull.describe()
    );
    assert_eq!(
        detail["budgetPolicy"].as_str(),
        Some("configured_cap"),
        "{step}: {}",
        pull.describe()
    );
    // Reported accounting of the complete response, measured under o200k_base/1.
    let delivered = &detail["deliveredResponse"];
    assert_eq!(
        delivered["estimatorBasis"].as_str(),
        Some("o200k_base/1"),
        "{step}: detail.deliveredResponse missing or on another estimator: {}",
        pull.describe()
    );
    assert_eq!(
        delivered["mode"].as_str(),
        Some("json"),
        "{step}: {}",
        pull.describe()
    );
    // The recount: the exact bytes the child wrote to stdout, counted by the independent test-side
    // counter. `wholeResponseTokens`/`wholeResponseBytes` must be exactly these, and the declared
    // budget bounds them; `contextTokens` counts the context inside, `packetTokens` the
    // serialized packet JSON (metadata, accounting, omission records) that is reported beside it
    // and not delivered.
    assert!(
        !pull.stdout.contains('\u{FFFD}') && pull.stdout.ends_with('\n'),
        "{step}: stdout is not one complete UTF-8 line: {}",
        pull.describe()
    );
    let whole_tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(&pull.stdout)
        .unwrap_or_else(|error| panic!("{step}: recount stdout: {error}"))
        as u64;
    assert!(
        (1..=G01_BUDGET).contains(&whole_tokens),
        "{step}: the complete response is {whole_tokens} tokens against the declared {G01_BUDGET}: {}",
        pull.describe()
    );
    assert_eq!(
        delivered["wholeResponseTokens"].as_u64(),
        Some(whole_tokens),
        "{step}: reported wholeResponseTokens differs from the recount of stdout: {}",
        pull.describe()
    );
    assert_eq!(
        delivered["wholeResponseBytes"].as_u64(),
        Some(pull.stdout.len() as u64),
        "{step}: reported wholeResponseBytes differs from the bytes written: {}",
        pull.describe()
    );
    assert_eq!(
        delivered["fitsBudget"],
        Value::Bool(true),
        "{step}: {}",
        pull.describe()
    );
    let context_tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(context)
        .unwrap_or_else(|error| panic!("{step}: recount context: {error}"))
        as u64;
    assert_eq!(
        delivered["contextTokens"].as_u64(),
        Some(context_tokens),
        "{step}: {}",
        pull.describe()
    );
    let packet_tokens = delivered["packetTokens"].as_u64().unwrap_or(0);
    assert!(
        packet_tokens >= context_tokens,
        "{step}: packetTokens {packet_tokens} missing or below contextTokens {context_tokens}: {}",
        pull.describe()
    );
    for (field, expected) in [
        ("scope", "whole_response"),
        ("selectedBy", "whole_response"),
    ] {
        assert_eq!(
            delivered[field].as_str(),
            Some(expected),
            "{step}: deliveredResponse.{field}: {}",
            pull.describe()
        );
    }
    // The prompt's words are not all in the rule, so the FTS AND query finds nothing and the
    // receipt must say the registry scan answered, for an explicit pull.
    let recall = &detail["cortexRecall"];
    assert_eq!(
        recall["explicitPull"],
        Value::Bool(true),
        "{step}: detail.cortexRecall missing: {}",
        pull.describe()
    );
    assert!(
        recall["lexicalMode"]
            .as_str()
            .is_some_and(|mode| mode.starts_with("scan_fallback:")),
        "{step}: the receipt does not report the scan fallback that answered: {}",
        pull.describe()
    );
    println!(
        "[g01] {step}: packetId {} sources {} wholeResponseTokens {whole_tokens} bytes {} contextTokens {context_tokens} packetTokens {packet_tokens} recall {}",
        detail["packetId"],
        sources.len(),
        pull.stdout.len(),
        recall["lexicalMode"]
    );
}

/// A `--json` pull that answered without context (no memory store yet, or no match): exit 0,
/// `membrane.context.pull.v1`, `context` null, the expected `reason` (and `detail` text when the
/// answer's detail is one), and the declared budget met by the COMPLETE response: the child's whole
/// stdout is recounted here, independently of the binary, with the same exact o200k_base/1 counter.
/// An empty answer has no selection, so it reports no whole-response accounting of its own; if it
/// ever does, that accounting must equal the recount.
fn assert_empty_answer(
    step: &str,
    pull: &Called,
    reason: &str,
    detail: Option<&str>,
    budget: u64,
) -> Value {
    assert_eq!(
        pull.code,
        Some(rightkit_context::EXIT_OK),
        "{step}: {}",
        pull.describe()
    );
    let doc = pull.json();
    assert_eq!(
        doc["schema"].as_str(),
        Some("membrane.context.pull.v1"),
        "{step}: {}",
        pull.describe()
    );
    assert!(
        doc["context"].is_null(),
        "{step}: context was delivered: {}",
        pull.describe()
    );
    assert_eq!(
        doc["reason"].as_str(),
        Some(reason),
        "{step}: {}",
        pull.describe()
    );
    if let Some(detail) = detail {
        assert_eq!(
            doc["detail"].as_str(),
            Some(detail),
            "{step}: {}",
            pull.describe()
        );
    }
    assert!(
        !pull.stdout.contains('\u{FFFD}') && pull.stdout.ends_with('\n'),
        "{step}: stdout is not one complete UTF-8 line: {}",
        pull.describe()
    );
    let whole_tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(&pull.stdout)
        .unwrap_or_else(|error| panic!("{step}: recount stdout: {error}"))
        as u64;
    assert!(
        (1..=budget).contains(&whole_tokens),
        "{step}: the complete response is {whole_tokens} tokens against the declared {budget}: {}",
        pull.describe()
    );
    let accounted = &doc["detail"]["deliveredResponse"];
    if accounted.is_object() {
        assert_eq!(
            accounted["wholeResponseTokens"].as_u64(),
            Some(whole_tokens),
            "{step}: {}",
            pull.describe()
        );
        assert_eq!(
            accounted["wholeResponseBytes"].as_u64(),
            Some(pull.stdout.len() as u64),
            "{step}: {}",
            pull.describe()
        );
    }
    println!(
        "[g01] {step}: reason {reason} wholeResponseTokens {whole_tokens} bytes {}",
        pull.stdout.len()
    );
    doc
}

/// A pull refused because the COMPLETE response of its mode exceeds the declared budget: exit 3,
/// nothing on stdout, the typed `budget_insufficient` reason on stderr naming the mode and the
/// declared budget.
fn assert_budget_refused(step: &str, pull: &Called, mode: &str, declared: u64) {
    assert_eq!(
        pull.code,
        Some(rightkit_context::EXIT_NOT_AVAILABLE),
        "{step}: an empty answer larger than the declared budget must be a typed not-available refusal: {}",
        pull.describe()
    );
    assert!(
        pull.stdout.is_empty(),
        "{step}: a document or context was delivered under a {declared}-token budget: {}",
        pull.describe()
    );
    assert!(
        pull.stderr.lines().any(|line| {
            line.starts_with(&format!(
                "membrane: request_time_selection_refused:budget_insufficient:complete {mode} response "
            )) && line.ends_with(&format!(" tokens exceeds declared {declared}"))
        }),
        "{step}: the refusal does not name the complete {mode} response and the declared budget: {}",
        pull.describe()
    );
}

#[test]
fn g01_identity_enrolment_push_pull_fault_and_recovery() {
    let journey = Instant::now();
    // 1. Disposable fixture: nothing real is copied, and no Git metadata is needed (init and pull
    //    read the registry and the stores; a non-repository is only a Git-provider omission).
    let parent = tempfile::tempdir().unwrap();
    let home = parent.path().join("home");
    let fixture = G01 {
        repo: parent.path().join("repo"),
        // Where the product resolves its state root and its project registry from `home`.
        state: common::state_root_under(&home),
        catalog: home.join("Cortex"),
        home,
        started: journey,
        log: Mutex::new(Vec::new()),
        _parent: parent,
    };
    for dir in [
        &fixture.repo,
        &fixture.state,
        &fixture.home,
        &fixture.catalog,
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    common::install_binary(&fixture.home);
    std::fs::write(fixture.repo.join("AGENTS.md"), G01_RULE).unwrap();
    assert_eq!(std::fs::read_dir(&fixture.state).unwrap().count(), 0);
    let processes_before = membrane_pids(&fixture);
    let mut deficits: Vec<String> = Vec::new();

    // 3. Identity. Recorded first, asserted below; a snapshot build without identity is the
    //    reproduced G-01 gap and fails the journey at its end, after the rest has run.
    let where_ = fixture.call(&["where", "--json"], None);
    assert_eq!(
        where_.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        where_.describe()
    );
    let where_doc = where_.json();
    assert_eq!(
        where_doc["schema"].as_str(),
        Some("membrane.where.v1"),
        "{}",
        where_.describe()
    );
    let status = fixture.call(&["status", "--json"], None);
    assert_eq!(
        status.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        status.describe()
    );
    let status_doc = status.json();
    assert_eq!(
        status_doc["schema"].as_str(),
        Some("membrane.status.v1"),
        "{}",
        status.describe()
    );
    let target = rightkit_context::runtime::release_identity::target_triple();
    assert_ne!(
        target, "unsupported",
        "the journey runs only on a known release target"
    );
    for (name, doc) in [("where", &where_doc), ("status", &status_doc)] {
        assert_eq!(doc["target"].as_str(), Some(target), "{name}: {doc}");
    }
    let reported_state = Path::new(status_doc["stateRoot"].as_str().unwrap_or_default());
    assert!(
        reported_state
            .canonicalize()
            .ok()
            .is_some_and(|reported| Some(reported) == fixture.state.canonicalize().ok()),
        "the reported state root is not the one resolved from the fixture home {}: {status_doc}",
        fixture.state.display()
    );
    assert_eq!(
        status_doc["mode"].as_str(),
        Some("in_process"),
        "{status_doc}"
    );
    assert_eq!(
        status_doc["resident"]["state"].as_str(),
        Some("absent"),
        "{status_doc}"
    );
    let commit = status_doc["membrane_source_commit"]
        .as_str()
        .unwrap_or("<absent>");
    let tree = status_doc["source_tree_sha256"]
        .as_str()
        .unwrap_or("<absent>");
    let generation = status_doc["release_generation"]
        .as_str()
        .unwrap_or("<absent>");
    println!("[g01] identity: commit {commit} tree {tree} release {generation} target {target}");
    assert_eq!(
        where_doc["membrane_source_commit"].as_str(),
        Some(commit),
        "where and status disagree"
    );
    if !is_lower_hex(commit, 40) {
        deficits.push(format!(
            "membrane_source_commit {commit:?} is not 40 lowercase hex"
        ));
    }
    if !is_lower_hex(tree, 64) {
        deficits.push(format!(
            "source_tree_sha256 {tree:?} is not 64 lowercase hex"
        ));
    }
    if generation
        .strip_prefix("sha256:")
        .is_none_or(|digest| !is_lower_hex(digest, 64))
    {
        deficits.push(format!(
            "release_generation {generation:?} does not match ^sha256:[0-9a-f]{{64}}$"
        ));
    }
    if generation != format!("sha256:{tree}") {
        deficits.push(format!(
            "release_generation {generation:?} is not sha256 of source_tree_sha256 {tree:?}"
        ));
    }
    for deficit in &deficits {
        println!("[g01] DEFICIT: {deficit}");
    }

    // 4. Enrolment through the public init; the binding is read back from the registry file.
    let repo = fixture.repo.to_str().expect("utf-8 fixture path");
    let init = fixture.call(
        &[
            "init",
            repo,
            "--repository",
            G01_REPOSITORY,
            "--scope",
            G01_SCOPE,
        ],
        None,
    );
    assert_eq!(
        init.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        init.describe()
    );
    let receipt = init.json();
    assert_eq!(
        receipt["action"].as_str(),
        Some("enroll"),
        "{}",
        init.describe()
    );
    let original_registry = std::fs::read(fixture.registry()).unwrap_or_else(|error| {
        panic!(
            "init exited 0 but the project registry {} was not written ({error}): {}",
            fixture.registry().display(),
            init.describe()
        )
    });
    let registry: Value = serde_json::from_slice(&original_registry).expect("registry is JSON");
    let bindings = registry["bindings"]
        .as_object()
        .expect("registry bindings object");
    assert_eq!(bindings.len(), 1, "{registry}");
    let repo_real = fixture.repo.canonicalize().unwrap();
    let (binding_key, binding) = bindings
        .iter()
        .find(|entry| {
            Path::new(entry.0.as_str()).canonicalize().ok().as_deref() == Some(repo_real.as_path())
        })
        .unwrap_or_else(|| {
            panic!(
                "no registry binding for {}: {registry}",
                repo_real.display()
            )
        });
    let binding_key = binding_key.clone();
    assert_eq!(
        binding["repository_id"].as_str(),
        Some(G01_REPOSITORY),
        "{binding}"
    );
    assert_eq!(binding["scope_id"].as_str(), Some(G01_SCOPE), "{binding}");

    // 4b. A fresh install has no memory store: the pull is an empty answer, held to the declared
    //     budget as a COMPLETE response of the requested mode like any other, and it is read-only
    //     (no store, no new identity, no resident record, no receipt).
    let after_init = fixture.footprint();
    let no_store = fixture.call(
        &[
            "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "512", "--json",
        ],
        None,
    );
    assert_empty_answer(
        "no-store pull",
        &no_store,
        "empty",
        Some("no memory store yet"),
        G01_BUDGET,
    );
    let no_store_wired = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_PROMPT,
            "--budget",
            "512",
            "--json",
            "--wire",
            "coderight",
            "--request-id",
            "g01-nostore-1",
        ],
        None,
    );
    let doc = assert_empty_answer(
        "no-store pull with receipt",
        &no_store_wired,
        "empty",
        Some("no memory store yet"),
        G01_BUDGET,
    );
    assert_eq!(
        doc["receipt"],
        serde_json::json!({"id": "g01-nostore-1", "session": "coderight", "state": "empty"}),
        "{}",
        no_store_wired.describe()
    );
    // Undersized: the receipt-bearing document does not fit 4 tokens, so no empty answer is
    // delivered, and nothing is recorded for it. Text mode projects zero bytes and fits any budget.
    let no_store_tiny = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_PROMPT,
            "--budget",
            "4",
            "--json",
            "--wire",
            "coderight",
            "--request-id",
            "g01-nostore-refused",
        ],
        None,
    );
    assert_budget_refused(
        "no-store pull, 4 tokens",
        &no_store_tiny,
        "json",
        G01_TINY_BUDGET,
    );
    let no_store_text = fixture.call(
        &[
            "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "4",
        ],
        None,
    );
    assert_eq!(
        no_store_text.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        no_store_text.describe()
    );
    assert!(
        no_store_text.stdout.is_empty(),
        "text mode printed something for an empty answer: {}",
        no_store_text.describe()
    );
    assert!(
        !fixture.cortex_db().exists()
            && !fixture.state.join("resident.json").exists()
            && !fixture.receipt_file("g01-nostore-1").exists()
            && !fixture.receipt_file("g01-nostore-refused").exists()
            && fixture.footprint() == after_init,
        "a read-only no-store pull changed the fixture: before {after_init:?}, after {:?}\n{}",
        fixture.footprint(),
        fixture.transcript()
    );

    // 5. Push the rule; the id comes from the JSON document.
    let push = fixture.call(
        &["memory", "push", "--stdin", "--scope", G01_SCOPE, "--json"],
        Some(G01_RULE),
    );
    assert_eq!(
        push.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        push.describe()
    );
    let pushed = push.json();
    assert_eq!(
        pushed["schema"].as_str(),
        Some("membrane.memory.push.v1"),
        "{}",
        push.describe()
    );
    assert!(
        pushed["id"].as_str().is_some_and(|id| !id.is_empty()),
        "{}",
        push.describe()
    );
    assert!(
        pushed["scope"]
            .as_str()
            .is_some_and(|scope| scope.contains(G01_REPOSITORY)),
        "{}",
        push.describe()
    );
    assert!(
        fixture.cortex_db().is_file(),
        "memory push did not create the Cortex store {}: {}",
        fixture.cortex_db().display(),
        push.describe()
    );

    // 5b. Memories that must not be delivered: an unrelated one in the same scope, a strong match
    //     under another scope, and a matching one that is pushed and then deleted.
    let push_id = |text: &str, extra: &[&str]| -> String {
        let mut args = vec!["memory", "push", "--stdin", "--json"];
        args.extend_from_slice(extra);
        let call = fixture.call(&args, Some(text));
        assert_eq!(
            call.code,
            Some(rightkit_context::EXIT_OK),
            "{}",
            call.describe()
        );
        let doc = call.json();
        assert_eq!(
            doc["schema"].as_str(),
            Some("membrane.memory.push.v1"),
            "{}",
            call.describe()
        );
        doc["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| panic!("push returned no id: {}", call.describe()))
            .to_owned()
    };
    let unrelated_id = push_id(G01_UNRELATED, &["--scope", G01_SCOPE]);
    let other_scope_id = push_id(
        G01_OTHER_SCOPE_TEXT,
        &[
            "--scope",
            G01_OTHER_SCOPE,
            "--keyword",
            "completion",
            "--keyword",
            "rule",
        ],
    );
    let deleted_id = push_id(
        G01_DELETED_TEXT,
        &["--scope", G01_SCOPE, "--keyword", "completion"],
    );
    let db = fixture.cortex_db();
    let delete = fixture.call(
        &[
            "memory",
            "delete",
            &deleted_id,
            "--db",
            db.to_str().expect("utf-8 fixture path"),
            "--json",
        ],
        None,
    );
    assert_eq!(
        delete.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        delete.describe()
    );
    let forbidden: [Forbidden<'_>; 3] = [
        (
            "unrelated same-scope",
            unrelated_id.as_str(),
            "coffee beans",
        ),
        (
            "other-scope",
            other_scope_id.as_str(),
            "OTHER-SCOPE-SENTINEL",
        ),
        ("deleted", deleted_id.as_str(), "DELETED-SENTINEL"),
    ];

    // 6. Bounded manual Pull with the fixture evidence: the complete `--json` response fits the
    //    declared 512 tokens (asserted by a recount of the child's stdout).
    let pull_argv = [
        "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "512", "--json",
    ];
    let first = fixture.call(&pull_argv, None);
    assert_pull_delivers("pull", &first, &pushed, &forbidden);
    let delivered_context = first.json()["context"]
        .as_str()
        .unwrap_or_default()
        .to_owned();

    // 6b. Text mode: exactly what the CLI prints, the context and one newline, under the same
    //     declared budget.
    let text_argv = [
        "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "512",
    ];
    let text_pull = fixture.call(&text_argv, None);
    assert_eq!(
        text_pull.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        text_pull.describe()
    );
    assert_eq!(
        text_pull.stdout,
        format!("{delivered_context}\n"),
        "text mode must print exactly the context and a newline: {}",
        text_pull.describe()
    );
    let text_tokens = rightkit_memory_core::ContextTokenAccounting::count_exact(&text_pull.stdout)
        .unwrap_or_else(|error| panic!("recount text stdout: {error}"))
        as u64;
    assert!(
        (1..=G01_BUDGET).contains(&text_tokens),
        "text response is {text_tokens} tokens against the declared {G01_BUDGET}: {}",
        text_pull.describe()
    );
    println!(
        "[g01] text mode: {text_tokens} tokens, {} bytes",
        text_pull.stdout.len()
    );

    // 6c. The budget bounds the COMPLETE response of the requested mode. Far too small for any
    //     response (8 tokens): a typed refusal, exit 3, nothing on stdout, never "no matches".
    let tiny_argv = [
        "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "8", "--json",
    ];
    let tiny = fixture.call(&tiny_argv, None);
    assert_eq!(
        tiny.code,
        Some(rightkit_context::EXIT_NOT_AVAILABLE),
        "a budget no response fits must be a typed not-available refusal: {}",
        tiny.describe()
    );
    assert!(
        tiny.stdout.trim().is_empty(),
        "context or a document was delivered under an 8-token budget: {}",
        tiny.describe()
    );
    assert!(
        tiny.stderr.lines().any(|line| line
            .starts_with("membrane: request_time_selection_refused:budget_insufficient:")),
        "the refusal does not carry the typed budget_insufficient reason: {}",
        tiny.describe()
    );
    // Between the two: 100 tokens hold the context (text mode delivers it) but not the whole
    // `--json` document, whose detail alone is several hundred tokens (refused, naming the size).
    let mid_json = fixture.call(
        &[
            "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "100", "--json",
        ],
        None,
    );
    assert_eq!(
        mid_json.code,
        Some(rightkit_context::EXIT_NOT_AVAILABLE),
        "{}",
        mid_json.describe()
    );
    assert!(
        mid_json.stdout.trim().is_empty()
            && mid_json.stderr.lines().any(|line| line.starts_with(
                "membrane: request_time_selection_refused:budget_insufficient:complete json response "
            )),
        "the --json refusal does not name the complete response: {}",
        mid_json.describe()
    );
    let mid_text = fixture.call(
        &[
            "context", "pull", "--cwd", repo, "--prompt", G01_PROMPT, "--budget", "100",
        ],
        None,
    );
    assert_eq!(
        mid_text.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        mid_text.describe()
    );
    assert_eq!(
        mid_text.stdout,
        format!("{delivered_context}\n"),
        "{}",
        mid_text.describe()
    );

    // 6d. `--wire coderight --request-id`: the receipt is part of the projected document, counted
    //     inside the declared budget, and the retained receipt file is written after it.
    let wired = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_PROMPT,
            "--budget",
            "512",
            "--json",
            "--wire",
            "coderight",
            "--request-id",
            G01_REQUEST_ID,
        ],
        None,
    );
    assert_pull_delivers("pull with receipt", &wired, &pushed, &forbidden);
    let wired_doc = wired.json();
    assert_eq!(
        wired_doc["receipt"],
        serde_json::json!({"id": G01_REQUEST_ID, "session": "coderight", "state": "emitted"}),
        "{}",
        wired.describe()
    );
    let receipt_file = fixture
        .state
        .join("receipts")
        .join("coderight")
        .join(format!("{G01_REQUEST_ID}.json"));
    let retained: Value =
        serde_json::from_slice(&std::fs::read(&receipt_file).unwrap_or_else(|error| {
            panic!("retained receipt {}: {error}", receipt_file.display())
        }))
        .expect("the retained receipt is JSON");
    for (field, expected) in [
        ("id", G01_REQUEST_ID),
        ("session", "coderight"),
        ("harness", "coderight"),
        ("event", "pull"),
        ("state", "emitted"),
    ] {
        assert_eq!(
            retained[field].as_str(),
            Some(expected),
            "retained receipt {field}: {retained}"
        );
    }

    // 6e. A genuine no-match on the populated store is an empty answer held to the same budget
    //     rule: its complete `--json` document is recounted, an undersized budget refuses it with
    //     no receipt, and its text mode is empty (zero bytes, so it fits any budget).
    let no_match = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_NO_MATCH_PROMPT,
            "--budget",
            "512",
            "--json",
        ],
        None,
    );
    let doc = assert_empty_answer(
        "no-match pull",
        &no_match,
        "membrane_no_matches",
        None,
        G01_BUDGET,
    );
    assert!(
        doc["detail"]["sources"]
            .as_array()
            .is_some_and(|sources| sources.is_empty()),
        "no-match pull reported sources: {}",
        no_match.describe()
    );
    let no_match_tiny = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_NO_MATCH_PROMPT,
            "--budget",
            "4",
            "--json",
            "--wire",
            "coderight",
            "--request-id",
            "g01-nomatch-refused",
        ],
        None,
    );
    assert_budget_refused(
        "no-match pull, 4 tokens",
        &no_match_tiny,
        "json",
        G01_TINY_BUDGET,
    );
    assert!(
        !fixture.receipt_file("g01-nomatch-refused").exists(),
        "a receipt was written for a refused no-match pull\n{}",
        fixture.transcript()
    );
    let no_match_text = fixture.call(
        &[
            "context",
            "pull",
            "--cwd",
            repo,
            "--prompt",
            G01_NO_MATCH_PROMPT,
            "--budget",
            "4",
        ],
        None,
    );
    assert_eq!(
        no_match_text.code,
        Some(rightkit_context::EXIT_OK),
        "{}",
        no_match_text.describe()
    );
    assert!(
        no_match_text.stdout.is_empty(),
        "text mode printed something for a no-match answer: {}",
        no_match_text.describe()
    );

    // 7. Invalid binding: only this binding's repository_id becomes empty, then the exact bytes return.
    let mut faulted = registry.clone();
    faulted["bindings"][binding_key.as_str()]["repository_id"] = Value::String(String::new());
    std::fs::write(
        fixture.registry(),
        serde_json::to_vec_pretty(&faulted).unwrap(),
    )
    .unwrap();
    let refused = fixture.call(&pull_argv, None);
    std::fs::write(fixture.registry(), &original_registry).unwrap();
    assert_eq!(
        std::fs::read(fixture.registry()).unwrap(),
        original_registry,
        "registry bytes not restored"
    );
    assert_eq!(
        refused.code,
        Some(rightkit_context::EXIT_NOT_AVAILABLE),
        "an invalid binding must be a typed not-available refusal: {}",
        refused.describe()
    );
    let delivered = serde_json::from_str::<Value>(refused.stdout.trim())
        .ok()
        .and_then(|doc| {
            doc["context"]
                .as_str()
                .map(|context| !context.trim().is_empty())
        })
        .unwrap_or(false);
    assert!(
        !delivered && !refused.stdout.contains(G01_RULE),
        "context delivered despite an invalid binding: {}",
        refused.describe()
    );
    // The malformed binding fails the registry load at the installation-grant gate; the refusal
    // names that gate and the field, not a generic omission.
    assert!(
        refused.stderr.lines().any(|line| {
            line.starts_with("membrane: authorization_denied:installation_grant_denied:")
                && line.contains("repository_id")
        }),
        "refusal does not carry the authorization gate code for the altered binding: {}",
        refused.describe()
    );
    assert_pull_delivers(
        "pull after restoring the binding",
        &fixture.call(&pull_argv, None),
        &pushed,
        &forbidden,
    );

    // 8. One-shot calls leave nothing behind.
    let leftover: Vec<u32> = membrane_pids(&fixture)
        .difference(&processes_before)
        .copied()
        .collect();
    assert!(
        leftover.is_empty(),
        "membrane processes survived the journey: {leftover:?}\n{}",
        fixture.transcript()
    );
    assert!(
        !fixture.state.join("resident.json").exists(),
        "a resident record appeared in the state root\n{}",
        fixture.transcript()
    );

    // 9. The wall clock was a deadline on every call; the identity deficits recorded at step 3.
    assert!(
        !fixture.budget_left().is_zero(),
        "journey wall clock {JOURNEY_BUDGET:?} exhausted\n{}",
        fixture.transcript()
    );
    assert!(
        deficits.is_empty(),
        "G-01 identity deficits: {deficits:#?}\n{}",
        fixture.transcript()
    );
}
