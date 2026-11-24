//! Real-file, real-process acceptance for `CompletedCallJournal` (RightKit #1656).
//! No mocks: a real tempdir, real journal files and real child processes.

use rightkit_agent_tools::{CompletedCall, CompletedCallJournal};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = "example.cli.journal.v1";
const MAX_BYTES: u64 = 1_048_576;

fn call<'a>(tool: &'a str) -> CompletedCall<'a> {
    CompletedCall {
        tool,
        effect: "read",
        approval_state: "none",
        caller: "ci",
        request_id: None,
        exit_code: 0,
        duration_ms: 1,
        host_route: "local",
    }
}

/// The current journal plus every suffix-numbered predecessor that exists (`.1` to `.9`).
fn journal_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let current = root.join("cli-journal.jsonl");
    if current.is_file() {
        files.push(current);
    }
    for n in 1..=9 {
        let p = root.join(format!("cli-journal.jsonl.{n}"));
        if p.is_file() {
            files.push(p);
        }
    }
    files
}

fn assert_every_line_parses(path: &Path) -> usize {
    let text = fs::read_to_string(path).unwrap();
    let mut lines = 0;
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("{} holds a non-JSON line {line:?}: {e}", path.display()));
        lines += 1;
    }
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "{} ends in a partial line",
        path.display()
    );
    lines
}

#[test]
fn shape_is_the_nine_v1_keys_and_content_free() {
    let root = tempfile::tempdir().unwrap();
    let journal = CompletedCallJournal::new(root.path(), SCHEMA);
    let long_id = "r".repeat(300);
    let mut c = call("status");
    c.caller = " ci\u{7}bot ";
    c.request_id = Some(&long_id);
    journal.append(&c).unwrap();

    let text = fs::read_to_string(root.path().join("cli-journal.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    let object = record.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "schema_version",
        "tool",
        "effect",
        "approval_state",
        "caller",
        "request_id",
        "exit_code",
        "duration_ms",
        "host_route",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(object["caller"], "cibot");
    assert_eq!(object["request_id"].as_str().unwrap().chars().count(), 256);

    let dir = root.path().to_string_lossy().into_owned();
    assert!(!text.contains(&dir));
    assert!(!text.contains("PROMPT"));
    assert!(!text.contains("/Users"));
}

#[test]
fn invalid_metadata_is_refused_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let journal = CompletedCallJournal::new(root.path(), SCHEMA);

    let bad_tool = call("a/b");
    let mut bad_effect = call("status");
    bad_effect.effect = "exec";
    let mut bad_approval = call("status");
    bad_approval.approval_state = "maybe";
    let mut bad_route = call("status");
    bad_route.host_route = "remote";

    for bad in [&bad_tool, &bad_effect, &bad_approval, &bad_route] {
        let err = journal.append(bad).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
    assert!(journal_files(root.path()).is_empty());
}

#[test]
fn rotation_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let journal = CompletedCallJournal::new(root.path(), SCHEMA);
    let c = call("status");
    for _ in 0..30_000 {
        journal.append(&c).unwrap();
    }

    assert!(
        !root.path().join("cli-journal.jsonl.5").exists(),
        "a fifth predecessor was retained"
    );
    let files = journal_files(root.path());
    assert!(!files.is_empty());
    for file in &files {
        assert_every_line_parses(file);
    }
    // At least one rotation must have happened for the bound to mean anything.
    assert!(
        root.path().join("cli-journal.jsonl.1").exists(),
        "30000 calls never rotated the journal"
    );
    let one_line_slack = 1024;
    let current = fs::metadata(root.path().join("cli-journal.jsonl")).unwrap();
    assert!(
        current.len() <= MAX_BYTES + one_line_slack,
        "current journal is {} bytes",
        current.len()
    );
}

#[test]
fn concurrent_processes_do_not_interleave() {
    let root = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut children = Vec::new();
    for _ in 0..3 {
        children.push(
            Command::new(&exe)
                .env("RK_JOURNAL_CHILD", root.path())
                .args(["--exact", "concurrent_child", "--nocapture"])
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        let status = child.wait().unwrap();
        assert!(status.success(), "child exited with {status}");
    }

    let files = journal_files(root.path());
    assert!(!files.is_empty(), "no journal file exists");
    for file in &files {
        assert_every_line_parses(file);
    }
}

/// Child half of `concurrent_processes_do_not_interleave`; returns at once when run on its own.
#[test]
fn concurrent_child() {
    let Some(root) = std::env::var_os("RK_JOURNAL_CHILD") else {
        return;
    };
    let journal = CompletedCallJournal::new(&root, SCHEMA);
    let c = call("status");
    for _ in 0..2000 {
        journal.append(&c).unwrap();
    }
}

#[test]
fn write_failure_returns_err_and_never_panics() {
    let c = call("status");

    // A state root that is a regular file.
    let scratch = tempfile::tempdir().unwrap();
    let file_root = scratch.path().join("not-a-directory");
    fs::write(&file_root, b"x").unwrap();
    let journal = CompletedCallJournal::new(&file_root, SCHEMA);
    assert!(journal.append(&c).is_err());

    // A read-only directory.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = scratch.path().join("read-only");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
        // Permission bits do not bind a privileged user; skip that half there.
        let enforced = fs::write(ro.join("probe"), b"x").is_err();
        if enforced {
            let journal = CompletedCallJournal::new(&ro, SCHEMA);
            assert!(journal.append(&c).is_err());
        }
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
