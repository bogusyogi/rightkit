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
    fs::write(&cfg_path, "app = \"strict-app\"\nscenarios = [\"scenarios\"]\n[engine]\nbinary = { darwin = \"engine.sh\", windows = \"engine.sh\" }\ndata_dir_env = \"STRICT_APP_DATA\"\n").unwrap();
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
    let r =
        run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &wrong.join("managed")).unwrap();
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

const OTHER_OS: &str = r#"
name = "other os"
requires = ["platform:__OTHER__"]
[[step]]
kind = "cli"
command = "x"
"#;
const STRAY: &str = r#"
name = "stray"
[[step]]
kind = "cli"
command = "x"
"#;

#[test]
fn other_platform_scenario_is_skipped_not_failed() {
    let d = tmp("otheros");
    let other = if cfg!(windows) { "darwin" } else { "windows" };
    let body = OTHER_OS.replace("__OTHER__", other);
    let cfg = app(&d, &[("right", RIGHT), ("other", &body)]);
    let r = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &d.join("managed")).unwrap();
    assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
    let o = r.outcomes.iter().find(|o| o.name == "other os").unwrap();
    assert_eq!(o.status, rightkit_qa::evidence::CheckStatus::Skipped);
}

fn stray_hooks(
    pid: std::sync::Arc<std::sync::Mutex<Option<u32>>>,
) -> rightkit_qa::suite::SuiteHooks {
    rightkit_qa::suite::SuiteHooks::default().setup(move |ctx, _| {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .map_err(|e| rightkit_qa::util::Error(e.to_string()))?;
        ctx.tracker.register(child.id(), "stray");
        *pid.lock().unwrap() = Some(child.id());
        std::mem::forget(child);
        Ok(())
    })
}

#[test]
fn kill_orphans_reaps_strays_after_teardown_without_failing_the_scenario() {
    // Default: a stray fails its scenario.
    let d = tmp("stray-off");
    let cfg = app(&d, &[("stray", STRAY)]);
    let pid = std::sync::Arc::new(std::sync::Mutex::new(None));
    let opts = SuiteOptions {
        hooks: stray_hooks(pid.clone()),
        ..Default::default()
    };
    let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
    assert_eq!(r.exit_code, 1, "{:?}", r.outcomes);

    // Option on: the scenario passes and the process is gone.
    let d = tmp("stray-on");
    let cfg = app(&d, &[("stray", STRAY), ("after", RIGHT)]);
    let pid = std::sync::Arc::new(std::sync::Mutex::new(None));
    let opts = SuiteOptions {
        hooks: stray_hooks(pid.clone()),
        kill_orphans: true,
        ..Default::default()
    };
    let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
    assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
    let p = pid.lock().unwrap().unwrap();
    assert!(!rightkit_qa::process::is_alive(p), "stray {p} survived");
}

#[test]
fn only_matches_the_file_stem_exactly_and_comma_lists() {
    let d = tmp("only");
    let cfg = app(&d, &[("alpha-one", RIGHT), ("beta-two", WRONG)]);
    let run = |only: &str| {
        let opts = SuiteOptions {
            only: Some(only.into()),
            ..Default::default()
        };
        run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap()
    };
    let r = run("alpha-one");
    assert_eq!(r.outcomes.len(), 1);
    assert_eq!(r.exit_code, 0);
    let r = run("alpha-one,beta-two");
    assert_eq!(r.outcomes.len(), 2);
    assert_eq!(r.exit_code, 1);
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

const CUSTOM: &str = r#"
name = "custom kinds"
[[step]]
kind = "mock_admin"
action = "arm"
save = "armed"
  [[step.expect]]
  path = "url"
  eq = "http://mock.test"
[[step]]
kind = "mock_admin"
action = "${armed.action}"
  [[step.expect]]
  path = "action"
  eq = "arm"
"#;
const CUSTOM_WRONG: &str = r#"
name = "custom wrong"
[[step]]
kind = "mock_admin"
action = "arm"
  [[step.expect]]
  path = "action"
  eq = "disarm"
"#;

fn hooks(log: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> rightkit_qa::suite::SuiteHooks {
    let (a, b, c) = (log.clone(), log.clone(), log.clone());
    rightkit_qa::suite::SuiteHooks::default()
        .step("mock_admin", move |ctx, p| {
            a.lock().unwrap().push(format!("step {}", ctx.scenario));
            Ok(serde_json::json!({
                "action": p.get("action"),
                "url": ctx.vars.get("mock_url"),
            }))
        })
        .setup(move |ctx, sc| {
            b.lock().unwrap().push(format!("setup {}", sc.name));
            ctx.vars
                .insert("mock_url".into(), serde_json::json!("http://mock.test"));
            Ok(())
        })
        .teardown(move |_, sc| {
            c.lock().unwrap().push(format!("teardown {}", sc.name));
            Ok(())
        })
}

#[test]
fn custom_step_kinds_and_scenario_setup_teardown_run_through_the_suite() {
    let d = tmp("custom");
    let cfg = app(&d, &[("custom", CUSTOM), ("wrong", CUSTOM_WRONG)]);
    let log = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let opts = SuiteOptions {
        hooks: hooks(&log),
        ..Default::default()
    };
    let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
    assert_eq!(
        r.exit_code, 1,
        "the wrong expectation on a custom kind must fail the run"
    );
    let by = |n: &str| r.outcomes.iter().find(|o| o.name == n).unwrap();
    assert_eq!(
        by("custom kinds").status,
        rightkit_qa::evidence::CheckStatus::Passed,
        "{:?}",
        by("custom kinds")
    );
    assert_eq!(
        by("custom wrong").status,
        rightkit_qa::evidence::CheckStatus::Failed
    );
    let log = log.lock().unwrap().clone();
    // Hooks run per scenario, in order: setup, the steps, teardown.
    assert_eq!(
        log,
        vec![
            "setup custom kinds",
            "step custom kinds",
            "step custom kinds",
            "teardown custom kinds",
            "setup custom wrong",
            "step custom wrong",
            "teardown custom wrong",
        ]
    );
}

#[test]
fn a_failing_setup_fails_the_scenario_runs_no_steps_and_still_tears_down() {
    let d = tmp("setupfail");
    let cfg = app(&d, &[("custom", CUSTOM)]);
    let log = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let mut h = hooks(&log);
    h = h.setup(|_, _| rightkit_qa::util::err("mock would not start"));
    let opts = SuiteOptions {
        hooks: h,
        ..Default::default()
    };
    let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
    assert_eq!(r.exit_code, 1);
    let reason = r.outcomes[0].reason.clone().unwrap();
    assert!(
        reason.contains("scenario setup: mock would not start"),
        "{reason}"
    );
    assert!(r.outcomes[0].steps.is_empty());
    assert_eq!(*log.lock().unwrap(), vec!["teardown custom kinds"]);
}

#[test]
fn a_failing_teardown_fails_an_otherwise_passing_scenario() {
    let d = tmp("tdfail");
    let cfg = app(&d, &[("custom", CUSTOM)]);
    let log = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let h = hooks(&log).teardown(|_, _| rightkit_qa::util::err("mock would not stop"));
    let opts = SuiteOptions {
        hooks: h,
        ..Default::default()
    };
    let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
    assert_eq!(r.exit_code, 1);
    assert!(r.outcomes[0]
        .reason
        .clone()
        .unwrap()
        .contains("scenario teardown: mock would not stop"));
}
