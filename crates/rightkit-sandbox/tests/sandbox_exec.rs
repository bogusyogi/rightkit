//! `rightkit-sandbox-exec` end to end on Windows: the AppContainer child can write its
//! writable grant and is denied writes into its read-only grant.

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXEC: &str = env!("CARGO_BIN_EXE_rightkit-sandbox-exec");

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-sandbox-exec-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn run_exec(read_only: &Path, writable: &Path, cwd: &Path, script: &str) -> Output {
    Command::new(EXEC)
        .arg("--read-only")
        .arg(read_only)
        .arg("--writable")
        .arg(writable)
        .arg("--cwd")
        .arg(cwd)
        .arg("--")
        .arg("cmd.exe")
        .arg("/c")
        .arg(script)
        .output()
        .expect("rightkit-sandbox-exec must start")
}

#[test]
fn write_into_writable_dir_succeeds() {
    let base = scratch("rw");
    let ro = base.join("ro");
    let rw = base.join("rw");
    fs::create_dir_all(&ro).unwrap();
    fs::create_dir_all(&rw).unwrap();
    let ro = fs::canonicalize(&ro).unwrap();
    let rw = fs::canonicalize(&rw).unwrap();

    let out = run_exec(&ro, &rw, &rw, "echo granted>allowed.txt");
    println!(
        "writable exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "write into writable dir must succeed"
    );
    let written = fs::read_to_string(rw.join("allowed.txt")).expect("file written by child");
    assert!(written.contains("granted"));
}

#[test]
fn write_into_read_only_dir_is_denied() {
    let base = scratch("ro");
    let ro = base.join("ro");
    let rw = base.join("rw");
    fs::create_dir_all(&ro).unwrap();
    fs::create_dir_all(&rw).unwrap();
    let ro = fs::canonicalize(&ro).unwrap();
    let rw = fs::canonicalize(&rw).unwrap();

    let out = run_exec(&ro, &rw, &ro, "echo denied>blocked.txt");
    println!(
        "read-only exit={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(
        out.status.code(),
        Some(0),
        "write into read-only dir must fail"
    );
    assert_ne!(
        out.status.code(),
        Some(125),
        "must reach the child, not fail setup"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Access is denied"),
        "child must report access denied"
    );
    assert!(
        !ro.join("blocked.txt").exists(),
        "read-only dir must not gain the file"
    );
}
