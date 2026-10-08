//! End-to-end: real engine process, real scenario files, real evidence on disk.
//! Proves the suite fails wrong content, fails mandatory skips, binds evidence to
//! binary/config/scenario identity, and that a grandchild holding stdout cannot hang a command.
#![cfg(unix)]

use rightkit_qa::config::QaConfig;
use rightkit_qa::process::{run_owned, RunOptions, Tracker};
use rightkit_qa::suite::{run_suite_in_managed_root, SuiteOptions};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rkqa-it-{name}-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}
fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn app(root: &Path, scenarios: &[(&str, &str)]) -> QaConfig {
    let engine = root.join("engine.sh");
    fs::write(
        &engine,
        "#!/bin/sh\necho '{\"value\":\"actual\",\"n\":3}'\n",
    )
    .unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(root.join("scenarios")).unwrap();
    for (n, body) in scenarios {
        fs::write(
            root.join("scenarios").join(format!("{n}.scenario.toml")),
            body,
        )
        .unwrap();
    }
    let cfg_path = root.join("rightkit-qa.toml");
    fs::write(&cfg_path, "app = \"strict-app\"\nscenarios = [\"scenarios\"]\n[engine]\nbinary = { darwin = \"engine.sh\", windows = \"engine.sh\" }\n").unwrap();
    QaConfig::load(&cfg_path).unwrap()
}

const WRONG: &str = r#"
name = "wrong content"
[[step]]
kind = "cli"
command = "x"
  [[step.expect]]
  path = "json.value"
  exists = true
  eq = "expected"
"#;
const RIGHT: &str = r#"
name = "right content"
[[step]]
kind = "cli"
command = "x"
  [[step.expect]]
  path = "json.value"
  exists = true
  eq = "actual"
"#;
const MANDATORY_SKIP: &str = r#"
name = "mandatory skip"
requires = ["env:RKQA_IT_DEFINITELY_UNSET"]
[[step]]
kind = "cli"
command = "x"
"#;
const OPTIONAL_SKIP: &str = r#"
name = "optional skip"
optional = true
requires = ["env:RKQA_IT_DEFINITELY_UNSET"]
[[step]]
kind = "cli"
command = "x"
"#;

#[test]
fn wrong_content_fails_and_right_content_binds_identity() {
    let wrong = tmp("wrong");
    let cfg = app(&wrong, &[("wrong", WRONG)]);
    let r = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &wrong.join("managed")).unwrap();
    assert_eq!(
        r.exit_code, 1,
        "exists+eq with the wrong value must not pass"
    );

    let ok = tmp("ok");
    let cfg = app(&ok, &[("right", RIGHT), ("opt", OPTIONAL_SKIP)]);
    let r = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &ok.join("managed")).unwrap();
    assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
    let ev: serde_json::Value =
        serde_json::from_slice(&fs::read(&r.evidence_path).unwrap()).unwrap();
    assert_eq!(ev["status"], "passed");
    let engine_sha = ev["identity"]["engine"]["sha256"].as_str().unwrap();
    assert_eq!(engine_sha.len(), 64);
    assert_eq!(engine_sha, sha_of(&ok.join("engine.sh")));
    assert_eq!(
        ev["identity"]["config"]["sha256"].as_str().unwrap(),
        sha_of(&ok.join("rightkit-qa.toml"))
    );
    assert_eq!(ev["identity"]["scenarios"].as_array().unwrap().len(), 2);
    assert!(ev["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["sha256"].as_str().unwrap().len() == 64));
}

#[test]
fn mandatory_skip_fails_the_run_but_optional_skip_does_not() {
    let d = tmp("skip");
    let cfg = app(&d, &[("right", RIGHT), ("mand", MANDATORY_SKIP)]);
    let r = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &d.join("managed")).unwrap();
    assert_eq!(
        r.exit_code, 1,
        "a skipped mandatory scenario must fail the run"
    );
    assert!(r
        .outcomes
        .iter()
        .any(|o| o.name == "mandatory skip"
            && o.reason.as_deref().unwrap_or("").contains("mandatory")));
}

#[test]
fn live_run_lock_holder_is_not_displaced_by_a_second_run() {
    let d = tmp("lock");
    let cfg = app(&d, &[("right", RIGHT)]);
    let managed_root = cfg.app_root.join("managed");
    let managed_app_root = managed_root.join("runs").join(&cfg.app);
    fs::create_dir_all(&managed_app_root).unwrap();
    let lock = rightkit_qa::lock::acquire(
        &managed_app_root.join("native.lock"),
        "first",
        &Default::default(),
    )
    .unwrap();
    let mut quick = cfg.clone();
    quick.lock.timeout_s = Some(1);
    quick.lock.stale_s = Some(0);
    let e = run_suite_in_managed_root(&quick, &SuiteOptions::default(), &managed_root).unwrap_err();
    assert!(e.0.contains("held by first"), "{e}");
    drop(lock);
    assert_eq!(
        run_suite_in_managed_root(&quick, &SuiteOptions::default(), &managed_root)
            .unwrap()
            .exit_code,
        0
    );
}

#[test]
fn grandchild_holding_stdout_cannot_hang_a_finished_command() {
    let t = Tracker::new();
    let started = Instant::now();
    let out = run_owned(
        "/bin/sh",
        &["-c".into(), "sleep 60 & echo done".into()],
        &RunOptions {
            timeout: Some(Duration::from_secs(20)),
            ..Default::default()
        },
        &t,
    )
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert!(out.stdout.contains("done"), "{out:?}");
    assert_eq!(out.code, Some(0));
}

fn sha_of(p: &Path) -> String {
    let out = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[cfg(target_os = "macos")]
#[test]
fn bundle_executable_is_resolved_from_info_plist_not_the_app_directory_name() {
    use rightkit_qa::control::resolve_bundle_executable;
    let root = tmp("bundle");
    let plist = |exe: &str| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>{exe}</string><key>CFBundleIdentifier</key><string>x.y</string></dict></plist>\n")
    };
    let make = |name: &str, exe_key: &str, create: Option<&str>| {
        let app = root.join(name);
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/Info.plist"), plist(exe_key)).unwrap();
        if let Some(f) = create {
            fs::write(app.join("Contents/MacOS").join(f), "#!/bin/sh\n").unwrap();
        }
        app
    };
    // Product.app whose executable is `runner`: the guessed name `Product` must NOT be used.
    let app = make("Product.app", "runner", Some("runner"));
    let got = resolve_bundle_executable(&app).expect("resolved");
    assert_eq!(got, app.join("Contents/MacOS/runner").to_string_lossy());
    assert!(!got.ends_with("/Product"));
    // Fail closed: declared executable missing, traversal, empty, or no plist at all.
    assert!(resolve_bundle_executable(&make("A.app", "runner", None)).is_none());
    assert!(resolve_bundle_executable(&make("B.app", "../../evil", Some("evil"))).is_none());
    assert!(resolve_bundle_executable(&make("C.app", "", Some("c"))).is_none());
    let bare = root.join("D.app");
    fs::create_dir_all(bare.join("Contents/MacOS")).unwrap();
    fs::write(bare.join("Contents/MacOS/D"), "x").unwrap();
    assert!(resolve_bundle_executable(&bare).is_none());
    let _ = fs::remove_dir_all(&root);
}
