//! `confine_command` on a real Linux kernel: `deny_read` inside a granted root, the rule budget
//! and the opt-in signal scope. Every test runs `/bin/sh -c` under Landlock; nothing is mocked.

#![cfg(target_os = "linux")]

use rightkit_sandbox::{
    confine_command, is_landlock_available, signal_scope_enforced, SandboxPolicy, SignalScope,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// A unique scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("rk-confine-{name}-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(fs::canonicalize(&dir).unwrap())
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn landlock_or_skip(test: &str) -> bool {
    if is_landlock_available() {
        return true;
    }
    eprintln!("skipping {test}: Landlock is not available on this host");
    false
}

fn sh(script: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    cmd
}

fn run_confined(script: &str, cwd: &Path, policy: &SandboxPolicy) -> io::Result<Output> {
    let mut cmd = sh(script, cwd);
    confine_command(&mut cmd, policy)?;
    cmd.output()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn deny_read_blocks_a_secret_inside_a_granted_root() {
    if !landlock_or_skip("deny_read_blocks_a_secret_inside_a_granted_root") {
        return;
    }
    let scratch = Scratch::new("deny");
    let root = scratch.path();
    fs::write(root.join("ok.txt"), "ok-content").unwrap();
    fs::create_dir_all(root.join("secret")).unwrap();
    fs::write(root.join("secret/.env"), "SECRET=1").unwrap();
    fs::write(root.join("a.key"), "KEY-MATERIAL").unwrap();

    let policy = SandboxPolicy::new()
        .allow_read(root)
        .deny_read(root.join("secret/.env"))
        .deny_read_glob(format!("{}/*.key", root.display()));

    // Positive control without confinement: both secrets are readable.
    let control = sh("cat secret/.env a.key", root).output().unwrap();
    assert!(
        control.status.success(),
        "unconfined control must read both"
    );
    let control_out = text(&control.stdout);
    assert!(control_out.contains("SECRET=1"), "{control_out}");
    assert!(control_out.contains("KEY-MATERIAL"), "{control_out}");

    let ok = run_confined("cat ok.txt", root, &policy).unwrap();
    assert!(ok.status.success(), "stderr: {}", text(&ok.stderr));
    assert_eq!(text(&ok.stdout), "ok-content");

    let env = run_confined("cat secret/.env", root, &policy).unwrap();
    assert!(
        !env.status.success(),
        "the denied .env must not be readable"
    );
    assert!(
        text(&env.stderr).contains("Permission denied"),
        "stderr: {}",
        text(&env.stderr)
    );
    assert!(!text(&env.stdout).contains("SECRET=1"));

    let key = run_confined("cat a.key", root, &policy).unwrap();
    assert!(
        !key.status.success(),
        "the globbed a.key must not be readable"
    );
    assert!(
        text(&key.stderr).contains("Permission denied"),
        "stderr: {}",
        text(&key.stderr)
    );
    assert!(!text(&key.stdout).contains("KEY-MATERIAL"));
}

#[test]
fn deny_read_beyond_the_rule_budget_refuses_the_launch() {
    if !landlock_or_skip("deny_read_beyond_the_rule_budget_refuses_the_launch") {
        return;
    }
    let scratch = Scratch::new("budget");
    let root = scratch.path();
    let big = root.join("big");
    fs::create_dir_all(&big).unwrap();
    fs::write(big.join("denied.txt"), "DENIED-CONTENT").unwrap();
    for i in 0..4200 {
        fs::write(big.join(format!("sibling-{i}.txt")), "x").unwrap();
    }

    let policy = SandboxPolicy::new()
        .allow_read(&big)
        .deny_read(big.join("denied.txt"));

    let mut cmd = sh("cat denied.txt", &big);
    if confine_command(&mut cmd, &policy).is_err() {
        return; // refused while confining: the command never ran
    }
    match cmd.output() {
        Err(_) => {} // refused at spawn: the command never ran
        Ok(out) => panic!(
            "the launch must be refused beyond the rule budget, but the command ran: status {:?}, stdout {:?}",
            out.status,
            text(&out.stdout)
        ),
    }
}

#[test]
fn signal_scope_stops_a_confined_child_signalling_the_launcher() {
    if !landlock_or_skip("signal_scope_stops_a_confined_child_signalling_the_launcher") {
        return;
    }
    let scratch = Scratch::new("signal");
    let root = scratch.path();
    let parent = std::process::id();
    let probe = format!("kill -0 {parent}");
    let policy = SandboxPolicy::new()
        .allow_read(root)
        .signal_scope(SignalScope::Own);

    if signal_scope_enforced() {
        let out = run_confined(&probe, root, &policy).unwrap();
        assert!(
            !out.status.success(),
            "a scoped child must not signal the launcher"
        );
        assert!(
            text(&out.stderr).contains("not permitted"),
            "expected EPERM, stderr: {}",
            text(&out.stderr)
        );

        let own = run_confined(
            "kill -0 $$ || exit 11; sleep 30 & c=$!; kill -0 $c || exit 12; kill $c || exit 13; wait $c; exit 0",
            root,
            &policy,
        )
        .unwrap();
        assert!(
            own.status.success(),
            "a scoped child can signal itself and its own child, status {:?}, stderr: {}",
            own.status,
            text(&own.stderr)
        );
    } else {
        // Best effort: on a kernel below Landlock ABI 6 the scope is not applied.
        let out = run_confined(&probe, root, &policy).unwrap();
        assert!(
            out.status.success(),
            "without ABI 6 the child can still signal the launcher, stderr: {}",
            text(&out.stderr)
        );
    }
}
