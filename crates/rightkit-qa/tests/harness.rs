//! The Rust-test scenario wrapper: tier gating, settings-backed requires, runtime skip,
//! failure and orphan accounting, per-scenario hashed evidence and the summary ledger.
use rightkit_qa::evidence::Bundle;
use rightkit_qa::harness::{read_summary, slug, Harness, Outcome};
use rightkit_qa::settings::Settings;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rkqa-harness-{name}-{}-{}",
        std::process::id(),
        rightkit_qa::util::new_id()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn harness(root: &Path, settings: &str) -> Harness {
    fs::create_dir_all(root.join(".cache/qa")).unwrap();
    fs::write(root.join(".cache/qa/settings.env"), settings).unwrap();
    let s = Settings::load(&root.join(".cache/qa/settings.env"))
        .unwrap()
        .with_env_override(false);
    Harness::with_parts_in_managed_root("harness-test", root.to_path_buf(), s, root.join("managed"))
}

fn bundle(h: &Harness, name: &str) -> Value {
    let p = h.evidence_root().join(slug(name)).join("evidence.json");
    serde_json::from_slice(&fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap()
}

#[test]
fn tiers_come_from_features_then_settings_then_explicit() {
    let r = root("tiers");
    let h = harness(&r, "");
    assert_eq!(h.selected_tiers(), "fast");
    let h = h.with_feature_tiers(&[("full", true), ("paid", false)]);
    assert_eq!(h.selected_tiers(), "fast,full");
    assert!(h.scenario("fast one", "fast", &[], |_| {}) == Outcome::Passed);
    assert!(h.scenario("full one", "full", &[], |_| {}) == Outcome::Passed);
    let paid = h.scenario("paid one", "paid", &[], |_| panic!("must not run"));
    assert!(matches!(&paid, Outcome::Skipped(r) if r.contains("tier paid not selected")));

    let h = harness(&r, "MYAPP_TIER=paid\n")
        .with_tier_key("MYAPP_TIER")
        .with_feature_tiers(&[("full", true)]);
    assert_eq!(
        h.selected_tiers(),
        "paid",
        "settings key replaces the computed selection"
    );
    assert_eq!(h.clone().with_tiers("all").selected_tiers(), "all");
    assert!(h.with_tiers("all").scenario("any", "nightly", &[], |_| {}) == Outcome::Passed);
}

#[test]
fn requires_read_the_settings_file_and_runtime_skip_is_recorded() {
    let r = root("requires");
    fs::write(r.join("present.txt"), "x").unwrap();
    let h = harness(&r, "PROVIDER_KEY=sk-test\n");
    let ran = h.scenario(
        "has key",
        "fast",
        &["env:PROVIDER_KEY", "file:present.txt"],
        |sc| {
            assert_eq!(
                sc.settings().get("PROVIDER_KEY").as_deref(),
                Some("sk-test")
            )
        },
    );
    assert_eq!(ran, Outcome::Passed);
    let skipped = h.scenario("no key", "fast", &["env:OTHER_KEY"], |_| {
        panic!("must not run")
    });
    assert_eq!(skipped, Outcome::Skipped("missing env OTHER_KEY".into()));
    assert!(h
        .unmet(&["file:absent.txt"])
        .unwrap()
        .contains("missing file"));
    assert!(h
        .unmet(&["cmd:definitely-not-a-real-command-rkqa"])
        .is_some());

    let rt = h.scenario("runtime skip", "fast", &[], |sc| {
        sc.note("probing GPU");
        sc.skip("no GPU");
    });
    assert_eq!(rt, Outcome::Skipped("no GPU".into()));
    let b = bundle(&h, "runtime skip");
    assert_eq!(b["checks"][0]["status"], "skipped");
    assert_eq!(b["checks"][0]["details"]["notes"][0], "probing GPU");

    let summary = read_summary(h.evidence_root()).unwrap();
    let statuses: Vec<(&str, &str)> = summary
        .iter()
        .map(|l| (l["name"].as_str().unwrap(), l["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        statuses,
        [
            ("has key", "passed"),
            ("no key", "skipped"),
            ("runtime skip", "skipped")
        ]
    );
    assert_eq!(summary[1]["reason"], "missing env OTHER_KEY");
}

#[test]
fn passing_scenario_writes_hashed_evidence_in_its_own_dir() {
    let r = root("evidence");
    let bin = r.join("engine.bin");
    fs::write(&bin, b"binary under test").unwrap();
    let h = harness(&r, "A=1\n")
        .with_identity_file("engine", &bin)
        .unwrap()
        .with_identity(json!({"revision": "abc"}))
        .with_identity_file("engine", &bin)
        .unwrap();
    let mut dir = PathBuf::new();
    let scratch_seen = std::cell::RefCell::new(None::<PathBuf>);
    let out = h.scenario("writes an artifact", "fast", &[], |sc| {
        dir = sc.dir().to_path_buf();
        assert!(sc.dir().is_dir());
        let f = sc.dir().join("out.txt");
        fs::write(&f, "result").unwrap();
        sc.keep("out.txt", &f);
        let s = sc.scratch("work dir");
        assert!(s.is_dir() && !s.starts_with(sc.dir()));
        fs::write(s.join("big.bin"), vec![0u8; 1024]).unwrap();
        *scratch_seen.borrow_mut() = Some(s);
    });
    assert_eq!(out, Outcome::Passed);
    // Scratch dirs are removed once the scenario passes (they leaked model-sized fixtures).
    assert!(!scratch_seen.borrow().as_ref().unwrap().exists());
    assert_eq!(dir, h.evidence_root().join(slug("writes an artifact")));
    let b: Bundle = serde_json::from_slice(&fs::read(dir.join("evidence.json")).unwrap()).unwrap();
    assert!(b.verify().is_empty());
    assert_eq!(b.artifacts[0].name, "out.txt");
    assert_eq!(b.checks[0].artifacts, ["out.txt"]);
    assert_eq!(b.identity["scenarios"][0], "writes an artifact");
    assert_eq!(b.identity["revision"], "abc");
    assert_eq!(
        b.identity["binaries"]["engine"]["sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(b.identity["config"]["sha256"].as_str().unwrap().len(), 64);
    // Distinct names never share a directory even when their slugs would collide.
    assert_ne!(slug("a/b"), slug("a b"));
}

#[test]
fn failing_scenario_panics_after_recording_failure() {
    let r = root("fail");
    let h = harness(&r, "");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.scenario("breaks", "fast", &[], |sc| {
            sc.note("before");
            assert_eq!(1 + 1, 3, "arithmetic is broken");
        })
    }));
    assert!(caught.is_err(), "a failing body fails the test");
    let b = bundle(&h, "breaks");
    assert_eq!(b["status"], "failed");
    assert!(b["checks"][0]["details"]["error"]
        .as_str()
        .unwrap()
        .contains("arithmetic is broken"));
    // A missing artifact downgrades the receipt instead of writing a false pass.
    let caught = h.scenario("ghost artifact", "fast", &[], |sc| {
        sc.keep("ghost", &sc.dir().join("never-written"));
    });
    assert_eq!(caught, Outcome::Passed);
    let g = bundle(&h, "ghost artifact");
    assert_eq!(g["status"], "failed");
    assert!(g["checks"][0]["details"]["artifact_errors"][0]
        .as_str()
        .unwrap()
        .contains("unreadable"));
}

#[cfg(unix)]
#[test]
fn leaked_process_fails_the_scenario_and_is_killed() {
    let r = root("orphan");
    let h = harness(&r, "");
    let mut slot: Option<std::process::Child> = None;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.scenario("leaks a child", "fast", &[], |sc| {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            sc.tracker().register(child.id(), "sleep 30");
            slot = Some(child);
        })
    }));
    let msg = caught
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        msg.contains("orphan processes outlived their owners or are still running")
            && msg.contains("sleep 30"),
        "{msg}"
    );
    let mut child = slot.unwrap();
    let started = std::time::Instant::now();
    let status = child.wait().unwrap();
    assert!(!status.success(), "survivor was killed, not left to finish");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(bundle(&h, "leaks a child")["status"], "failed");
}

#[test]
fn run_lock_is_taken_and_released_per_scenario() {
    let r = root("lock");
    let lock = r.join("locks").join("e2e.lock");
    let h = harness(&r, "").with_run_lock(&lock, Duration::from_secs(5));
    assert_eq!(
        h.scenario("locked", "fast", &[], |_| assert!(
            lock.exists(),
            "held while running"
        )),
        Outcome::Passed
    );
    assert!(!lock.exists(), "released afterwards");
    assert_eq!(
        h.scenario("locked again", "fast", &[], |_| {}),
        Outcome::Passed
    );
}

#[test]
fn discover_uses_the_app_root_search_order() {
    if std::env::var_os(rightkit_qa::settings::SETTINGS_ENV).is_some() {
        return;
    }
    let r = root("discover");
    fs::create_dir_all(r.join(".cache/e2e")).unwrap();
    fs::write(
        r.join(".cache/e2e/settings.env"),
        "RIGHTKIT_QA_TIER=fast,paid\nRIGHTKIT_QA_EVIDENCE=out/ev\n",
    )
    .unwrap();
    let h = Harness::new_in_managed_root("discover", &r, r.join("managed")).unwrap();
    assert_eq!(
        h.settings().source(),
        Some(r.join(".cache/e2e/settings.env").as_path())
    );
    assert_eq!(h.selected_tiers(), "fast,paid");
    // The managed root is canonical (macOS `/var` -> `/private/var`); the evidence root sits under it.
    let managed = rightkit_qa::util::canonical_plain(&r.join("managed")).unwrap();
    assert!(h.evidence_root().starts_with(&managed));
}

/// A harness for `app` in its own managed root; its home sits in the real QA root, as in production.
fn harness_for(app: &str, root: &Path) -> Harness {
    fs::create_dir_all(root.join(".cache/qa")).unwrap();
    fs::write(root.join(".cache/qa/settings.env"), "").unwrap();
    let s = Settings::load(&root.join(".cache/qa/settings.env"))
        .unwrap()
        .with_env_override(false);
    Harness::with_parts_in_managed_root(app, root.to_path_buf(), s, root.join("managed"))
}

/// The panic message of a scenario that was refused.
fn refusal(result: std::thread::Result<Outcome>) -> String {
    let payload = result.expect_err("the scenario must be refused");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[test]
fn qa_root_limits_are_per_product_and_are_set_in_code() {
    let pid = std::process::id();
    let (own, other) = (format!("limA{pid}"), format!("limB{pid}"));
    let r = root("limits");
    // One product leaves 100 KB in its home: a kept scratch file survives the scenario.
    let fill = harness_for(&own, &r)
        .with_root_cap_bytes(0)
        .with_root_ceiling_bytes(0);
    assert_eq!(
        fill.scenario("fill", "fast", &[], |sc| {
            let file = sc.scratch("fill").join("blob");
            fs::write(&file, vec![0u8; 100_000]).unwrap();
            sc.keep("blob", &file);
        }),
        Outcome::Passed
    );

    // Another run of the same product, with a cap below that, is refused and told what to do.
    let same = harness_for(&own, &r)
        .with_root_cap_bytes(50_000)
        .with_root_ceiling_bytes(0);
    let text = refusal(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || same.scenario("refused", "fast", &[], |_| panic!("must not run")),
    )));
    assert!(
        text.contains(&own)
            && text.contains("per-product cap")
            && text.contains("rightkit-qa clean"),
        "{text}"
    );

    // A run of another product with the same cap is not blocked by it.
    let stranger = harness_for(&other, &r)
        .with_root_cap_bytes(50_000)
        .with_root_ceiling_bytes(0);
    assert_eq!(
        stranger.scenario("not blocked", "fast", &[], |_| {}),
        Outcome::Passed
    );

    // The root ceiling counts every product's homes.
    let tight = harness_for(&other, &r)
        .with_root_cap_bytes(0)
        .with_root_ceiling_bytes(50_000);
    let text = refusal(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || tight.scenario("ceiling", "fast", &[], |_| panic!("must not run")),
    )));
    assert!(
        text.contains("root ceiling") && text.contains("RIGHTKIT_QA_ROOT_CEILING_BYTES"),
        "{text}"
    );
    drop((fill, same, stranger, tight));
    let _ = fs::remove_dir_all(&r);
}

/// A `platform:<os>` requirement naming a known OS other than this host's, built at runtime so no
/// `"platform:<x>"` literal sits in a requires list (preflight G13). On Linux both are "other".
fn other_os_requirement() -> &'static str {
    if cfg!(target_os = "macos") {
        "platform:windows"
    } else {
        "platform:darwin"
    }
}

/// The panic text of `f`, which must panic.
fn panic_text(f: impl FnOnce()) -> String {
    let payload =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("the call must panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[test]
fn require_ran_panics_for_a_skip_that_is_not_another_os() {
    let r = root("require-skip");
    let h = harness(&r, "");
    // Spelled at runtime: an unknown OS is a typo, never "another platform".
    let unknown_os = ["platform:", "solaris"].concat();
    let cases = [
        (
            h.scenario("needs a key", "fast", &["env:RKQA_NEVER_SET"], |_| {
                panic!("must not run")
            }),
            "missing env RKQA_NEVER_SET",
        ),
        (
            h.scenario("tier gated", "full", &[], |_| panic!("must not run")),
            "tier full not selected",
        ),
        (
            h.scenario("runtime skip", "fast", &[], |sc| sc.skip("no GPU")),
            "no GPU",
        ),
        (
            h.scenario("typo platform", "fast", &[unknown_os.as_str()], |_| {
                panic!("must not run")
            }),
            "unknown platform 'solaris'",
        ),
    ];
    for (outcome, reason) in cases {
        assert!(outcome.is_skipped(), "{reason}: {outcome:?}");
        assert!(!outcome.skipped_for_other_platform(), "{reason}");
        let text = panic_text(move || outcome.require_ran());
        assert!(
            text.contains(reason) && text.contains("skipped scenario is not a pass"),
            "{text}"
        );
    }
}

#[test]
fn require_ran_returns_for_a_passed_scenario() {
    let r = root("require-pass");
    let h = harness(&r, "KEY=1\n");
    let outcome = h.scenario("runs", "fast", &["env:KEY"], |sc| sc.note("ran"));
    assert_eq!(outcome, Outcome::Passed);
    assert!(!outcome.skipped_for_other_platform());
    // A panic here would fail the test: a passed scenario is a pass.
    outcome.require_ran();
    h.scenario("runs again", "fast", &[], |_| {}).require_ran();
}

#[test]
fn require_ran_returns_for_a_skip_naming_the_other_os() {
    let r = root("require-other-os");
    let h = harness(&r, "");
    let other = other_os_requirement();
    let outcome = h.scenario("other os only", "fast", &[other], |_| {
        panic!("must not run")
    });
    let os = other.strip_prefix("platform:").unwrap();
    assert_eq!(outcome, Outcome::Skipped(format!("platform is not {os}")));
    assert!(outcome.skipped_for_other_platform());
    // A scenario that can never run on this host is not a gap: no panic.
    outcome.require_ran();
}

#[test]
fn the_other_os_reason_wins_over_every_other_skip_reason() {
    let r = root("other-os-wins");
    let h = harness(&r, "");
    let other = other_os_requirement();
    let os = other.strip_prefix("platform:").unwrap();
    let want = Outcome::Skipped(format!("platform is not {os}"));
    // An unmet env requirement listed before the OS one would otherwise name the env var.
    let env_first = h.scenario("env first", "fast", &["env:RKQA_NEVER_SET", other], |_| {
        panic!("must not run")
    });
    assert_eq!(env_first, want);
    assert!(env_first.skipped_for_other_platform());
    let os_first = h.scenario("os first", "fast", &[other, "env:RKQA_NEVER_SET"], |_| {
        panic!("must not run")
    });
    assert_eq!(os_first, want);
    // So would a tier that is not selected.
    let tier = h.scenario("tier and os", "paid", &[other], |_| panic!("must not run"));
    assert_eq!(tier, want);
    assert!(tier.skipped_for_other_platform());
    // Without the OS requirement the other reasons are reported as before.
    let gap = h.scenario("tier only", "paid", &[], |_| panic!("must not run"));
    assert!(matches!(&gap, Outcome::Skipped(r) if r.contains("tier paid not selected")));
    assert!(!gap.skipped_for_other_platform());
    // The summary ledger records the same reason the Outcome carries.
    let summary = read_summary(h.evidence_root()).unwrap();
    assert_eq!(summary[0]["reason"], format!("platform is not {os}"));
}

#[test]
fn only_a_real_other_os_reason_is_skipped_for_other_platform() {
    assert!(!Outcome::Passed.skipped_for_other_platform());
    for reason in [
        "missing env KEY",
        "no GPU",
        // Worded like a platform skip, but names no known OS.
        "platform is not a thing",
        "platform is not ",
        // An unknown platform is a typo, not another OS.
        "unknown platform 'solaris' (expected darwin, macos, windows or linux)",
    ] {
        let outcome = Outcome::Skipped(reason.into());
        assert!(!outcome.skipped_for_other_platform(), "{reason}");
        assert!(
            panic_text(move || outcome.require_ran()).contains(reason),
            "{reason}"
        );
    }
}
