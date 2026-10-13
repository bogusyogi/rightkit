//! Test-only fake vendor CLI. The test binary re-executes itself and replays a recorded
//! fixture over stdio, so spawn and protocol handling run end to end without a vendor
//! binary. Nothing here ships: it is a test helper, compiled only into test targets.

#![allow(dead_code)]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Name of the `#[test]` fn each test file defines to host the child mode.
pub const CHILD_TEST: &str = "fake_cli_child";
const KIND_ENV: &str = "RK_FAKE_CLI_KIND";
const FIXTURE_ENV: &str = "RK_FAKE_CLI_FIXTURE";

pub fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

pub fn fixture_lines(relative: &str) -> Vec<String> {
    std::fs::read_to_string(fixture_path(relative))
        .expect("fixture is readable")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Spawn this test binary as a fake `claude` or `codex` replaying `fixture`.
pub fn spawn_fake(kind: &str, fixture: &str) -> Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(KIND_ENV, kind)
        .env(FIXTURE_ENV, fixture_path(fixture))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fake cli")
}

/// The libtest harness prints its own banner lines on stdout; only JSON objects count.
pub fn is_wire_line(line: &str) -> bool {
    line.trim_start().starts_with('{')
}

/// Body of each file's `fake_cli_child` test. A no-op unless spawned by [`spawn_fake`].
pub fn run_child_if_requested() {
    let Ok(kind) = std::env::var(KIND_ENV) else {
        return;
    };
    let fixture = std::env::var(FIXTURE_ENV).expect("fixture env");
    let lines: Vec<String> = std::fs::read_to_string(fixture)
        .expect("fixture readable")
        .lines()
        .map(str::to_string)
        .collect();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if kind == "claude" {
        let mut first = String::new();
        let _ = input.read_line(&mut first);
        for line in &lines {
            writeln!(out, "{line}").unwrap();
        }
        out.flush().unwrap();
    } else {
        replay_codex(&lines, &mut input, &mut out);
    }
    std::process::exit(0);
}

fn is_response(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .map(|v| v.get("id").is_some() && v.get("method").is_none())
        .unwrap_or(false)
}

fn replay_codex(lines: &[String], input: &mut dyn BufRead, out: &mut dyn Write) {
    let mut next = 0usize;
    loop {
        let mut request = String::new();
        if input.read_line(&mut request).unwrap_or(0) == 0 {
            break;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(request.trim()) else {
            continue;
        };
        let Some(id) = parsed.get("id").cloned() else {
            continue;
        };
        if parsed.get("method").is_none() {
            // A response to a server request: echo it so the test can observe it.
            let echo =
                serde_json::json!({"method": "fake/echo", "params": {"line": request.trim()}});
            writeln!(out, "{echo}").unwrap();
            out.flush().unwrap();
            continue;
        }
        while next < lines.len() {
            let line = &lines[next];
            next += 1;
            if is_response(line) {
                let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["id"] = id.clone();
                writeln!(out, "{value}").unwrap();
                if !lines[next..].iter().any(|l| is_response(l)) {
                    for rest in &lines[next..] {
                        writeln!(out, "{rest}").unwrap();
                    }
                    next = lines.len();
                }
                break;
            }
            writeln!(out, "{line}").unwrap();
        }
        out.flush().unwrap();
    }
}
