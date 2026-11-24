//! End-to-end tests for the exec-boundary check `exec_reason` (CodeRight #457 item 3, decision D6): the file
//! about to be run is inspected at launch, so write-then-run forms the command text hides are refused.

use rightkit_agent_policy::exec_reason;
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rk-exec-boundary-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn done(d: &Path) {
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_script_holding_a_catastrophic_command_is_refused_when_run_by_a_shell() {
    let d = dir("shell");
    std::fs::write(d.join("x.sh"), "rm -rf /*\n").unwrap();
    assert!(exec_reason("sh", &["x.sh".into()], &d).is_some());
    assert!(exec_reason("bash", &["-e".into(), "x.sh".into()], &d).is_some());
    done(&d);
}

#[test]
fn a_script_run_by_its_own_path_is_refused() {
    let d = dir("own-path");
    std::fs::write(d.join("x.sh"), "rm -rf /*\n").unwrap();
    assert!(exec_reason(&d.join("x.sh").to_string_lossy(), &[], &d).is_some());
    done(&d);
}

#[test]
fn a_split_derived_file_is_refused() {
    let d = dir("split");
    std::fs::write(d.join("xaa"), "rm -rf /*\n").unwrap();
    assert!(exec_reason("sh", &["xaa".into()], &d).is_some());
    done(&d);
}

#[test]
fn an_awk_written_file_is_refused() {
    let d = dir("awk");
    // What `awk 'BEGIN{print "rm -rf /*"}' > x.sh` leaves behind.
    std::fs::write(d.join("x.sh"), "rm -rf /*\n").unwrap();
    assert!(exec_reason("sh", &["x.sh".into()], &d).is_some());
    done(&d);
}

#[test]
fn a_direct_catastrophic_argv_is_refused() {
    let d = dir("argv");
    assert!(exec_reason("rm", &["-rf".into(), "/".into()], &d).is_some());
    done(&d);
}

#[test]
fn harmless_launches_stay_allowed() {
    let d = dir("harmless");
    std::fs::write(d.join("ok.sh"), "echo hello\n").unwrap();
    assert_eq!(exec_reason("sh", &["ok.sh".into()], &d), None);
    assert_eq!(exec_reason("ls", &["-la".into()], &d), None);
    assert_eq!(
        exec_reason("sh", &["-c".into(), "echo hi".into()], &d),
        None
    );
    done(&d);
}

#[test]
fn a_missing_file_and_a_binary_file_are_not_inspected() {
    let d = dir("missing-binary");
    assert_eq!(exec_reason("sh", &["nope.sh".into()], &d), None);
    let mut bytes = vec![0x7f, b'E', b'L', b'F', 0, 0, 0, 0];
    bytes.extend_from_slice(b"rm -rf /*");
    std::fs::write(d.join("bin"), bytes).unwrap();
    assert_eq!(exec_reason(&d.join("bin").to_string_lossy(), &[], &d), None);
    done(&d);
}

#[test]
fn an_oversized_script_is_refused_not_half_checked() {
    let d = dir("oversized");
    let mut text = String::new();
    while text.len() < 1_048_576 + 100 {
        text.push_str("echo ok\n");
    }
    std::fs::write(d.join("x.sh"), text).unwrap();
    assert_eq!(
        exec_reason("sh", &["x.sh".into()], &d),
        Some("script too large for the destructive guard to inspect")
    );
    done(&d);
}
