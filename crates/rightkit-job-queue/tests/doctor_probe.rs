//! Actual executable probes on Unix; parser & registry unit tests are platform independent.
#![cfg(unix)]

use rightkit_job_queue::{run_doctor, DoctorOptions, DoctorProbe, ErrorOrigin};
use serde_json::json;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DOC: &str = r#"{"app_version":"1.2.3","contract_version":1,"capabilities":[{"name":"render","version":1}],"readiness":{"ready":false}}"#;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        format!("#!/bin/sh\n[ \"$1\" = doctor ] || exit 71\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn options(timeout: Duration, max_stdout_bytes: usize) -> DoctorOptions {
    DoctorOptions {
        timeout,
        max_stdout_bytes,
    }
}

#[test]
fn nonzero_doctor_is_valid_and_spawn_and_parse_errors_are_typed() {
    let dir = tempfile::tempdir().unwrap();
    let exe = script(
        dir.path(),
        "engine with spaces",
        &format!("printf '%s' '{DOC}'\nexit 1"),
    );
    let doc = run_doctor(&exe, Duration::from_secs(3)).unwrap();
    assert_eq!(doc["readiness"]["ready"], false);
    assert_eq!(doc["app_version"], "1.2.3");
    let missing = run_doctor(dir.path().join("missing"), Duration::from_secs(1)).unwrap_err();
    assert_eq!(missing.code, "spawn_failed");
    assert_eq!(missing.origin, ErrorOrigin::Supervisor);
    let bad = script(dir.path(), "bad", "printf 'not JSON'");
    assert_eq!(
        run_doctor(&bad, Duration::from_secs(3)).unwrap_err().code,
        "doctor_bad_json"
    );
    let bad = script(dir.path(), "bad-shape", "printf '{}'");
    assert_eq!(
        run_doctor(&bad, Duration::from_secs(3)).unwrap_err().code,
        "doctor_bad_shape"
    );
    let bad = script(dir.path(), "bad-utf8", r"printf '\377'");
    assert_eq!(
        run_doctor(&bad, Duration::from_secs(3)).unwrap_err().code,
        "doctor_bad_json"
    );
}

#[test]
fn stdout_cap_accepts_exact_size_and_rejects_truncation_even_of_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let exe = script(dir.path(), "engine", &format!("printf '%s' '{DOC}'"));
    let probe = DoctorProbe::new(options(Duration::from_secs(3), DOC.len()));
    assert!(probe.probe(&exe).is_ok());
    let small = DoctorProbe::new(options(Duration::from_secs(3), DOC.len() - 1));
    assert_eq!(small.probe(&exe).unwrap_err().code, "doctor_output_limit");
    assert!(small.cached_capabilities(&exe).is_none());
    let padded = script(dir.path(), "padded", &format!("printf '%s' '{DOC} '\n"));
    assert_eq!(
        probe.probe(&padded).unwrap_err().code,
        "doctor_output_limit"
    );
    let flood = script(dir.path(), "flood", "i=0; while [ $i -lt 20000 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; i=$((i+1)); done");
    let probe = DoctorProbe::new(options(Duration::from_secs(5), 64));
    assert_eq!(probe.probe(&flood).unwrap_err().code, "doctor_output_limit");
}

#[test]
fn deadlines_cover_hung_process_closed_stdout_and_inherited_stdout() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("hung", "exec sleep 30"),
        ("closed", "exec 1>&-; exec sleep 30"),
        ("inherited", "sleep 30 &\nexit 0"),
    ] {
        let exe = script(dir.path(), name, body);
        let started = Instant::now();
        assert_eq!(
            run_doctor(&exe, Duration::from_millis(100))
                .unwrap_err()
                .code,
            "doctor_timeout",
            "{name}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{name}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn cache_is_binary_scoped_and_readiness_always_runs_live() {
    let dir = tempfile::tempdir().unwrap();
    // Payload changes without changing executable identity.
    fs::write(dir.path().join("payload"), DOC).unwrap();
    let exe = script(
        dir.path(),
        "engine",
        "exec cat \"$(dirname \"$0\")/payload\"",
    );
    let probe = DoctorProbe::default();
    assert!(probe.cached_capabilities(&exe).is_none());
    let doc = probe.probe(&exe).unwrap();
    assert_eq!(
        probe.cached_capabilities(&exe),
        Some(doc["capabilities"].clone())
    );
    let alias = dir.path().join("alias");
    symlink(&exe, &alias).unwrap();
    assert_eq!(
        probe.cached_capabilities(&alias),
        Some(doc["capabilities"].clone())
    );
    let mut ready = doc.clone();
    ready["readiness"]["ready"] = json!(true);
    fs::write(dir.path().join("payload"), ready.to_string()).unwrap();
    assert_eq!(probe.probe(&exe).unwrap()["readiness"]["ready"], true);
    assert!(probe.cached_capabilities(&exe).unwrap().is_array());
    // Same path, same length & restored mtime still changes Unix inode identity.
    let mtime = fs::metadata(&exe).unwrap().modified().unwrap();
    let replacement = script(
        dir.path(),
        "replacement",
        "exec cat \"$(dirname \"$0\")/payload\"",
    );
    fs::File::open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    fs::rename(replacement, &exe).unwrap();
    assert!(probe.cached_capabilities(&exe).is_none());
    assert!(probe.probe(&exe).is_ok());
    probe.clear_cache();
    assert!(probe.cached_capabilities(&exe).is_none());
    assert!(probe.probe(&exe).is_ok());
    fs::remove_file(&exe).unwrap();
    assert!(probe.cached_capabilities(&exe).is_none());
}

#[test]
fn replacement_during_probe_is_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    let replacement = script(dir.path(), "next", &format!("printf '%s' '{DOC}'"));
    let exe = script(
        dir.path(),
        "engine",
        &format!("mv \"$(dirname \"$0\")/next\" \"$0\"\nprintf '%s' '{DOC}'"),
    );
    let probe = DoctorProbe::default();
    assert!(probe.probe(&exe).is_ok());
    assert!(!replacement.exists());
    assert!(probe.cached_capabilities(&exe).is_none());
    assert!(probe.probe(&exe).is_ok());
    assert!(probe.cached_capabilities(&exe).is_some());
}
