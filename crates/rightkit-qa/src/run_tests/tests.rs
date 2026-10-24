//! Tests of the compile plan (every platform) and of the whole run against fake test binaries (Unix: the
//! "binaries" are symlinks to `/bin/sh`, told what to do by the libtest arguments and by the
//! `CARGO_PKG_NAME` the runner sets, so no file is written and then executed).
use super::*;
use std::ffi::OsString;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "rkqa-rt-{name}-{}-{}",
        std::process::id(),
        crate::util::new_id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

// ------------------------------------------------------------------------------ plan_compile

fn path_with_broker() -> (PathBuf, OsString) {
    let dir = temp("broker");
    std::fs::write(dir.join("rustright"), b"#!/bin/sh\n").unwrap();
    let path = std::env::join_paths([dir.clone()]).unwrap();
    (dir, path)
}

#[test]
fn a_host_with_the_broker_compiles_through_it() {
    let (dir, path) = path_with_broker();
    let plan = plan_compile(
        &argv("-p fx --test ui"),
        BuildWith::Auto,
        false,
        &path,
        false,
        None,
    )
    .unwrap();
    assert!(plan.managed);
    assert_eq!(plan.program, dir.join("rustright"));
    assert_eq!(
        plan.args,
        argv("cargo test -p fx --test ui --no-run --message-format=json")
    );
    assert!(plan.note.is_none());
    // Forcing the broker works the same; plain cargo is still allowed by name when not required.
    assert!(
        plan_compile(&[], BuildWith::Rustright, true, &path, false, None)
            .unwrap()
            .managed
    );
    let cargo = plan_compile(&[], BuildWith::Cargo, false, &path, false, Some("/x/cargo")).unwrap();
    assert!(!cargo.managed);
    assert_eq!(cargo.program, PathBuf::from("/x/cargo"));
    assert_eq!(cargo.args, argv("test --no-run --message-format=json"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_host_without_the_broker_falls_back_to_plain_cargo_and_says_why() {
    let empty = temp("nobroker");
    let path = std::env::join_paths([empty.clone()]).unwrap();
    let plan = plan_compile(&argv("-p fx"), BuildWith::Auto, false, &path, false, None).unwrap();
    assert!(!plan.managed);
    assert_eq!(plan.program, PathBuf::from("cargo"));
    assert!(plan
        .note
        .as_deref()
        .unwrap()
        .contains("does not resolve on PATH"));
    assert_eq!(plan.args, argv("test -p fx --no-run --message-format=json"));
    let _ = std::fs::remove_dir_all(empty);
}

#[test]
fn require_broker_leaves_no_fallback_to_plain_cargo() {
    let empty = temp("required");
    let path = std::env::join_paths([empty.clone()]).unwrap();
    let e = plan_compile(&[], BuildWith::Auto, true, &path, false, None).unwrap_err();
    assert!(e.0.contains("require_broker"), "{e}");
    let e = plan_compile(&[], BuildWith::Cargo, true, &path, false, None).unwrap_err();
    assert!(e.0.contains("plain cargo is refused"), "{e}");
    assert!(plan_compile(&[], BuildWith::Rustright, false, &path, false, None).is_err());
    let _ = std::fs::remove_dir_all(empty);
}

#[test]
fn the_compile_command_passes_the_journeys_build_validator() {
    let (dir, path) = path_with_broker();
    for bad in [
        "--target-dir /t",
        "--target-dir=/t",
        "--out-dir /o",
        "--artifact-dir=/o",
        "--message-format short",
        "--message-format=human",
    ] {
        let e = plan_compile(&argv(bad), BuildWith::Auto, false, &path, false, None).unwrap_err();
        assert!(e.0.contains("run-tests:"), "{bad}: {e}");
    }
    // A caller's own --no-run is not doubled, and a JSON format they name is kept as theirs.
    let p = plan_compile(
        &argv("--no-run --message-format=json-render-diagnostics -p fx"),
        BuildWith::Auto,
        false,
        &path,
        false,
        None,
    )
    .unwrap();
    assert_eq!(p.args.iter().filter(|a| *a == "--no-run").count(), 1);
    assert_eq!(
        p.args
            .iter()
            .filter(|a| a.starts_with("--message-format"))
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------------------ pure helpers

#[test]
fn classify_marks_a_zero_exit_over_a_red_summary_as_failed() {
    use libtest::LibtestSummary;
    let green = LibtestSummary {
        results: 1,
        ok: true,
        passed: 2,
        ..Default::default()
    };
    let red = LibtestSummary {
        results: 1,
        ok: false,
        failed: 1,
        ..Default::default()
    };
    assert_eq!(classify(false, Some(0), Some(&green)).0, Status::Passed);
    assert_eq!(classify(false, Some(0), None).0, Status::Passed);
    let (status, note) = classify(false, Some(0), Some(&red));
    assert_eq!(status, Status::Failed);
    assert!(note
        .unwrap()
        .contains("exited 0 but its summary reports 1 failed"));
    assert_eq!(classify(false, Some(101), Some(&green)).0, Status::Failed);
    assert_eq!(classify(false, None, None).0, Status::Failed);
    assert_eq!(classify(true, None, Some(&green)).0, Status::TimedOut);
}

#[test]
fn summary_lines_name_the_binary_its_exit_its_counts_and_its_time() {
    let bin = TestBinary {
        package: "fx".into(),
        target: "ui".into(),
        kind: vec!["test".into()],
        executable: PathBuf::from("/t/ui"),
        manifest_dir: PathBuf::from("/w/fx"),
    };
    let mut rec = BinaryRecord::pending(0, &bin, &[], Path::new("/w/fx"));
    rec.status = Status::Passed;
    rec.exit = Some(0);
    rec.duration_ms = Some(12_340);
    rec.summary = Some(libtest::LibtestSummary {
        results: 1,
        ok: true,
        passed: 3,
        failed: 0,
        ignored: 1,
        measured: 0,
        filtered_out: 0,
    });
    assert_eq!(
        summary_line(&rec),
        "run-tests: fx::ui exit=0 passed=3 failed=0 ignored=1 12.3s"
    );
    rec.status = Status::TimedOut;
    rec.exit = None;
    rec.summary = None;
    rec.duration_ms = Some(2_000);
    assert_eq!(summary_line(&rec), "run-tests: fx::ui exit=timeout 2.0s");
    rec.status = Status::Failed;
    rec.signal = Some(9);
    assert_eq!(summary_line(&rec), "run-tests: fx::ui exit=signal-9 2.0s");
    rec.status = Status::NotRun;
    rec.duration_ms = None;
    assert_eq!(summary_line(&rec), "run-tests: fx::ui exit=not-run");
}

#[test]
fn log_names_are_indexed_and_safe() {
    let bin = TestBinary {
        package: "my pkg".into(),
        target: "a/b:c".into(),
        kind: vec![],
        executable: PathBuf::new(),
        manifest_dir: PathBuf::new(),
    };
    assert_eq!(log_stem(7, &bin), "007-my_pkg-a_b_c");
}

// ------------------------------------------------------------------------------ whole runs (Unix)

#[cfg(unix)]
mod runs {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fx {
        dir: PathBuf,
        root: PathBuf,
        managed: PathBuf,
        bins: PathBuf,
    }

    impl Fx {
        fn new(name: &str) -> Self {
            let dir = temp(name);
            let root = dir.join("app");
            let managed = dir.join("managed");
            let bins = dir.join("bins");
            for d in [&root, &managed, &bins] {
                std::fs::create_dir_all(d).unwrap();
            }
            Fx {
                dir,
                root,
                managed,
                bins,
            }
        }

        /// A listing naming one test binary per package, each a distinct symlink to `/bin/sh`, its
        /// package directory `<root>/crates/<package>`.
        fn listing(&self, packages: &[&str]) -> PathBuf {
            let mut text = String::new();
            for p in packages {
                let exe = self.bins.join(format!("{p}-test"));
                let _ = std::fs::remove_file(&exe);
                symlink("/bin/sh", &exe).unwrap();
                let pkg_dir = self.root.join("crates").join(p);
                std::fs::create_dir_all(&pkg_dir).unwrap();
                text.push_str(&format!(
                    "{{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file://{dir}#{p}@0.1.0\",\"manifest_path\":\"{dir}/Cargo.toml\",\"target\":{{\"kind\":[\"test\"],\"name\":\"t\"}},\"profile\":{{\"test\":true}},\"executable\":\"{exe}\"}}\n",
                    dir = pkg_dir.display(),
                    exe = exe.display()
                ));
            }
            text.push_str("{\"reason\":\"build-finished\",\"success\":true}\n");
            let path = self.dir.join("listing.jsonl");
            std::fs::write(&path, text).unwrap();
            path
        }

        fn options(&self, listing: &Path, script: &str) -> Options {
            Options {
                root: self.root.clone(),
                app: "fx".into(),
                managed_root: Some(self.managed.clone()),
                run_id: None,
                source: Source::Listing(ListingSource::File(listing.to_path_buf())),
                test_args: vec!["-c".into(), script.into(), "name".into()],
                settings: Settings::default(),
                receipt_copy: None,
                config_path: None,
            }
        }
    }

    const OK_LINE: &str = "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s";

    #[test]
    fn a_passing_run_writes_the_receipt_the_logs_and_a_passed_marker() {
        let fx = Fx::new("pass");
        let listing = fx.listing(&["alpha", "beta"]);
        let o = run(&fx.options(
            &listing,
            &format!("echo 'running 3 tests'; echo '{OK_LINE}'"),
        ))
        .unwrap();
        assert_eq!(o.exit_code, 0, "{:?}", o.receipt.verdict);
        assert!(o.receipt.verdict.passed && o.receipt.verdict.reasons.is_empty());
        assert_eq!(o.receipt.schema, RECEIPT_SCHEMA);
        assert_eq!(o.receipt.app, "fx");
        assert_eq!(o.receipt.compile.source, "listing");
        assert_eq!(o.receipt.compile.test_binaries, 2);
        assert_eq!(o.receipt.compile.build_finished, Some(true));
        assert!(o.receipt.compile.listing_sha256.is_some());
        assert_eq!(o.receipt.binaries.len(), 2);
        for b in &o.receipt.binaries {
            assert_eq!(b.status, Status::Passed);
            assert_eq!(b.exit, Some(0));
            assert!(b.sha256.as_deref().is_some_and(|h| h.len() == 64));
            let s = b.summary.as_ref().unwrap();
            assert_eq!((s.passed, s.failed, s.ignored), (2, 0, 1));
            assert!(Path::new(b.stdout.as_ref().unwrap()).is_file());
            assert!(Path::new(b.stderr.as_ref().unwrap()).is_file());
            assert_eq!(
                b.stdout_bytes.unwrap() as usize,
                std::fs::read(b.stdout.as_ref().unwrap()).unwrap().len()
            );
        }
        assert_eq!(o.receipt.totals.tests.passed, 4);
        assert_eq!(o.receipt.totals.tests.ignored, 2);
        // The receipt on disk is the receipt returned, in the run storage under <app>-run-tests.
        let on_disk: Receipt =
            serde_json::from_slice(&std::fs::read(&o.receipt_path).unwrap()).unwrap();
        assert_eq!(on_disk, o.receipt);
        assert_eq!(
            o.run_dir.parent().unwrap(),
            fx.managed.join("runs").join("fx-run-tests")
        );
        assert!(o.run_dir.join(LISTING_FILE).is_file());
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(o.run_dir.join(".rightkit-run.json")).unwrap())
                .unwrap();
        assert_eq!(marker["outcome"], "passed");
        assert_eq!(marker["app"], "fx-run-tests");
        assert!(marker["completedAt"].is_number());
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_failing_binary_fails_the_run_and_names_its_failed_tests() {
        let fx = Fx::new("fail");
        let listing = fx.listing(&["good", "bad"]);
        let script = format!(
            "if [ \"$CARGO_PKG_NAME\" = bad ]; then echo 'test t::broken ... FAILED'; echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'; exit 101; fi; echo '{OK_LINE}'"
        );
        let o = run(&fx.options(&listing, &script)).unwrap();
        assert_eq!(o.exit_code, 1);
        assert_eq!(o.receipt.verdict.reasons, ["bad::t failed (exit 101)"]);
        let bad = o
            .receipt
            .binaries
            .iter()
            .find(|b| b.package == "bad")
            .unwrap();
        assert_eq!(bad.status, Status::Failed);
        assert_eq!(bad.exit, Some(101));
        assert_eq!(bad.failed_tests, ["t::broken"]);
        let good = o
            .receipt
            .binaries
            .iter()
            .find(|b| b.package == "good")
            .unwrap();
        assert_eq!(
            good.status,
            Status::Passed,
            "one failure does not stop the others"
        );
        assert_eq!(o.receipt.totals.failed, 1);
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(o.run_dir.join(".rightkit-run.json")).unwrap())
                .unwrap();
        assert_eq!(marker["outcome"], "failed");
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_binary_over_its_deadline_is_killed_and_the_run_fails_in_time() {
        let fx = Fx::new("timeout");
        let listing = fx.listing(&["slow", "quick"]);
        let script =
            format!("if [ \"$CARGO_PKG_NAME\" = slow ]; then sleep 60; fi; echo '{OK_LINE}'");
        let mut opts = fx.options(&listing, &script);
        opts.settings.binary_timeout = Duration::from_secs(1);
        let started = Instant::now();
        let o = run(&opts).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(o.exit_code, 1);
        let slow = o
            .receipt
            .binaries
            .iter()
            .find(|b| b.package == "slow")
            .unwrap();
        assert_eq!(slow.status, Status::TimedOut);
        assert!(slow.timed_out);
        assert_eq!(slow.exit, None);
        assert_eq!(slow.timeout_s, Some(1));
        assert_eq!(slow.timeout_kind.as_deref(), Some("binary"));
        assert!(o.receipt.verdict.reasons[0].contains("timed out after 1s (binary deadline)"));
        let quick = o
            .receipt
            .binaries
            .iter()
            .find(|b| b.package == "quick")
            .unwrap();
        assert_eq!(quick.status, Status::Passed);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn the_total_deadline_cuts_the_running_binary_and_leaves_the_rest_not_run() {
        let fx = Fx::new("total");
        let listing = fx.listing(&["a", "b", "c"]);
        let mut opts = fx.options(&listing, "sleep 60");
        opts.settings.parallel = 1;
        opts.settings.total_timeout = Duration::from_secs(1);
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 1);
        let statuses: Vec<Status> = o.receipt.binaries.iter().map(|b| b.status).collect();
        assert_eq!(statuses, [Status::TimedOut, Status::NotRun, Status::NotRun]);
        assert_eq!(o.receipt.binaries[0].timeout_kind.as_deref(), Some("total"));
        assert!(o.receipt.binaries[1]
            .error
            .as_deref()
            .unwrap()
            .contains("total deadline"));
        assert_eq!(o.receipt.totals.not_run, 2);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn two_binaries_run_at_the_same_time_with_parallel_two() {
        let fx = Fx::new("parallel");
        let listing = fx.listing(&["a", "b"]);
        // Each binary announces itself, then waits (up to 10 s) until the other has too. Run one after the
        // other, the first would wait out the loop and exit 7.
        let sync = fx.dir.join("sync");
        std::fs::create_dir_all(&sync).unwrap();
        let script = format!(
            "touch '{d}'/$CARGO_PKG_NAME.up; n=0; while [ ! -e '{d}'/a.up ] || [ ! -e '{d}'/b.up ]; do n=$((n+1)); [ $n -gt 200 ] && exit 7; sleep 0.05; done; echo '{OK_LINE}'",
            d = sync.display()
        );
        let mut opts = fx.options(&listing, &script);
        opts.settings.parallel = 2;
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 0, "{:?}", o.receipt.verdict);
        let starts: Vec<u64> = o
            .receipt
            .binaries
            .iter()
            .map(|b| b.started_after_ms.unwrap())
            .collect();
        assert!(
            starts.iter().all(|s| *s < 5_000),
            "both started together: {starts:?}"
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn parallel_one_runs_the_binaries_in_listing_order_one_at_a_time() {
        let fx = Fx::new("serial");
        let listing = fx.listing(&["a", "b", "c"]);
        let order = fx.dir.join("order.txt");
        let script = format!(
            "echo $CARGO_PKG_NAME >> '{o}'; sleep 0.2; echo '{OK_LINE}'",
            o = order.display()
        );
        let mut opts = fx.options(&listing, &script);
        opts.settings.parallel = 1;
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 0);
        assert_eq!(std::fs::read_to_string(&order).unwrap(), "a\nb\nc\n");
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn libtest_arguments_cwd_and_environment_reach_each_binary() {
        let fx = Fx::new("passthrough");
        let listing = fx.listing(&["alpha"]);
        let mut opts = fx.options(
            &listing,
            "echo \"args=$1,$2,$3\"; echo \"cwd=$PWD\"; echo \"manifest=$CARGO_MANIFEST_DIR\"; echo \"mine=$RK_MINE\"; echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;'",
        );
        opts.test_args = [
            "-c",
            opts.test_args[1].as_str(),
            "name",
            "--skip",
            "slow",
            "--exact",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        opts.settings.env.insert("RK_MINE".into(), "yes".into());
        let read = |o: &Outcome| {
            std::fs::read_to_string(o.receipt.binaries[0].stdout.as_ref().unwrap()).unwrap()
        };

        // The default is the package directory, as cargo does.
        let pkg = fx.root.join("crates").join("alpha");
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 0, "{:?}", o.receipt.verdict);
        let out = read(&o);
        assert!(out.contains("args=--skip,slow,--exact"), "{out}");
        assert!(out.contains("mine=yes"), "{out}");
        assert!(
            out.contains(&format!("manifest={}", pkg.display())),
            "{out}"
        );
        let printed_cwd = out.lines().find_map(|l| l.strip_prefix("cwd=")).unwrap();
        assert_eq!(
            std::fs::canonicalize(printed_cwd).unwrap(),
            std::fs::canonicalize(&pkg).unwrap()
        );
        assert_eq!(
            o.receipt.binaries[0].args[3..],
            ["--skip", "slow", "--exact"]
        );
        assert_eq!(o.receipt.settings.env_keys, ["RK_MINE"]);
        assert!(
            !std::fs::read_to_string(&o.receipt_path)
                .unwrap()
                .contains("yes"),
            "env values are never recorded"
        );

        // The workspace and an explicit directory.
        opts.settings.cwd = CwdMode::Workspace;
        let out = read(&run(&opts).unwrap());
        let printed = out.lines().find_map(|l| l.strip_prefix("cwd=")).unwrap();
        assert_eq!(
            std::fs::canonicalize(printed).unwrap(),
            std::fs::canonicalize(&fx.root).unwrap()
        );
        std::fs::create_dir_all(fx.root.join("sub")).unwrap();
        opts.settings.cwd = CwdMode::Path(PathBuf::from("sub"));
        let out = read(&run(&opts).unwrap());
        let printed = out.lines().find_map(|l| l.strip_prefix("cwd=")).unwrap();
        assert_eq!(
            std::fs::canonicalize(printed).unwrap(),
            std::fs::canonicalize(fx.root.join("sub")).unwrap()
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn isolate_tmp_gives_each_binary_its_own_directory_and_removes_it() {
        let fx = Fx::new("tmp");
        let listing = fx.listing(&["a", "b"]);
        let mut opts = fx.options(
            &listing,
            &format!("echo \"tmp=$TMPDIR\"; touch \"$TMPDIR/left-behind\"; echo '{OK_LINE}'"),
        );
        opts.settings.isolate_tmp = true;
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 0);
        let dirs: Vec<String> = o
            .receipt
            .binaries
            .iter()
            .map(|b| b.tmpdir.clone().unwrap())
            .collect();
        assert_ne!(dirs[0], dirs[1]);
        for (b, dir) in o.receipt.binaries.iter().zip(&dirs) {
            let out = std::fs::read_to_string(b.stdout.as_ref().unwrap()).unwrap();
            assert!(out.contains(&format!("tmp={dir}")), "{out}");
            assert!(!Path::new(dir).exists(), "{dir} was not removed");
        }
        assert!(
            !Path::new(&dirs[0]).parent().unwrap().exists(),
            "the run's temp root was not removed"
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn the_default_unset_list_removes_build_variables_and_the_caller_can_set_them_back() {
        let fx = Fx::new("unset");
        let listing = fx.listing(&["a"]);
        let mut opts = fx.options(
            &listing,
            "echo \"wrapper=[$RUSTC_WRAPPER] forward=[$RIGHTKIT_FORWARD_ENV]\"; echo 'test result: ok. 1 passed; 0 failed;'",
        );
        opts.settings
            .env
            .insert("RUSTC_WRAPPER".into(), "kept".into());
        let o = run(&opts).unwrap();
        let out = std::fs::read_to_string(o.receipt.binaries[0].stdout.as_ref().unwrap()).unwrap();
        assert!(out.contains("wrapper=[kept] forward=[]"), "{out}");
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_listing_with_no_test_executable_is_a_setup_error_with_a_receipt() {
        let fx = Fx::new("none");
        let listing = fx.dir.join("none.jsonl");
        std::fs::write(
            &listing,
            "{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///w#x@0.1.0\",\"target\":{\"kind\":[\"lib\"],\"name\":\"x\"},\"profile\":{\"test\":false},\"executable\":null}\n{\"reason\":\"build-finished\",\"success\":true}\n",
        )
        .unwrap();
        let o = run(&fx.options(&listing, "exit 0")).unwrap();
        assert_eq!(o.exit_code, 2);
        assert!(o.receipt.verdict.reasons[0].contains("listed no test executable"));
        assert!(o.receipt.binaries.is_empty());
        assert!(
            o.receipt_path.is_file(),
            "a failed run still leaves its receipt"
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_listing_that_is_not_a_complete_compile_is_refused() {
        let fx = Fx::new("partial");
        let full = fx.listing(&["a"]);
        // Cut short: the build-finished record is gone.
        let text = std::fs::read_to_string(&full).unwrap();
        let cut = fx.dir.join("cut.jsonl");
        std::fs::write(
            &cut,
            text.lines()
                .filter(|l| !l.contains("build-finished"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let o = run(&fx.options(&cut, "exit 0")).unwrap();
        assert_eq!(o.exit_code, 2);
        assert!(o.receipt.verdict.reasons[0].contains("no build-finished record"));
        // A recorded failed build is a compile failure.
        let failed = fx.dir.join("failed.jsonl");
        std::fs::write(
            &failed,
            text.replace("\"success\":true", "\"success\":false"),
        )
        .unwrap();
        let o = run(&fx.options(&failed, "exit 0")).unwrap();
        assert_eq!(o.exit_code, 3);
        assert!(o.receipt.binaries.is_empty(), "nothing ran");
        // A listing that is not there is an error before a run exists.
        let missing = run(&fx.options(&fx.dir.join("missing.jsonl"), "exit 0")).unwrap();
        assert_eq!(missing.exit_code, 2);
        assert!(missing.receipt.verdict.reasons[0].contains("cannot read the listing"));
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_missing_executable_is_an_error_status_not_a_pass() {
        let fx = Fx::new("gone");
        let listing = fx.listing(&["a", "b"]);
        std::fs::remove_file(fx.bins.join("b-test")).unwrap();
        let o = run(&fx.options(&listing, &format!("echo '{OK_LINE}'"))).unwrap();
        assert_eq!(o.exit_code, 1);
        let b = o
            .receipt
            .binaries
            .iter()
            .find(|b| b.package == "b")
            .unwrap();
        assert_eq!(b.status, Status::Error);
        assert!(b.error.as_deref().unwrap().contains("missing"));
        assert_eq!(o.receipt.totals.errors, 1);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn deny_ignored_and_the_empty_run_rule_are_enforced() {
        let fx = Fx::new("rules");
        let listing = fx.listing(&["a"]);
        let mut opts = fx.options(&listing, &format!("echo '{OK_LINE}'"));
        opts.settings.deny_ignored = true;
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 1);
        assert!(o.receipt.verdict.reasons[0].contains("1 ignored test(s) did not run"));

        let empty = "echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.00s'";
        let mut opts = fx.options(&listing, empty);
        let o = run(&opts).unwrap();
        assert_eq!(o.exit_code, 1);
        assert!(o.receipt.verdict.reasons[0].starts_with("no test ran"));
        opts.settings.allow_empty = true;
        assert_eq!(run(&opts).unwrap().exit_code, 0);
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_zero_exit_over_a_red_summary_is_not_a_pass() {
        let fx = Fx::new("liar");
        let listing = fx.listing(&["a"]);
        let o = run(&fx.options(
            &listing,
            "echo 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;'; exit 0",
        ))
        .unwrap();
        assert_eq!(o.exit_code, 1);
        assert_eq!(o.receipt.binaries[0].status, Status::Failed);
        assert!(o.receipt.binaries[0]
            .error
            .as_deref()
            .unwrap()
            .contains("exited 0 but"));
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn the_receipt_can_be_copied_and_an_explicit_run_id_names_the_directory() {
        let fx = Fx::new("copy");
        let listing = fx.listing(&["a"]);
        let mut opts = fx.options(&listing, &format!("echo '{OK_LINE}'"));
        opts.run_id = Some("gate-1".into());
        opts.receipt_copy = Some(fx.dir.join("out").join("receipt.json"));
        let o = run(&opts).unwrap();
        assert_eq!(o.run_dir.file_name().unwrap(), "gate-1");
        assert_eq!(
            std::fs::read(fx.dir.join("out").join("receipt.json")).unwrap(),
            std::fs::read(&o.receipt_path).unwrap()
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn retention_keeps_a_failed_run_and_the_newest_pass_only() {
        let fx = Fx::new("retention");
        let listing = fx.listing(&["a"]);
        let pass = fx.options(&listing, &format!("echo '{OK_LINE}'"));
        let first = run(&pass).unwrap();
        let second = run(&pass).unwrap();
        let fail = run(&fx.options(&listing, "exit 3")).unwrap();
        assert!(!first.run_dir.exists(), "an older pass is pruned");
        assert!(second.run_dir.exists(), "the newest pass is kept");
        assert!(fail.run_dir.exists(), "a failed run is always kept");
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_missing_app_root_and_an_unsafe_app_key_are_refused_before_a_run_exists() {
        let fx = Fx::new("refuse");
        let listing = fx.listing(&["a"]);
        let mut opts = fx.options(&listing, "exit 0");
        opts.root = fx.dir.join("no-such-root");
        assert!(run(&opts).is_err());
        let mut opts = fx.options(&listing, "exit 0");
        opts.app = "../escape".into();
        assert!(run(&opts).is_err());
        assert!(!fx.managed.join("runs").exists(), "nothing was created");
        let _ = std::fs::remove_dir_all(&fx.dir);
    }

    #[test]
    fn a_compile_the_host_must_not_make_fails_before_anything_is_created() {
        let fx = Fx::new("nocompile");
        let mut opts = fx.options(&fx.dir.join("unused"), "exit 0");
        opts.source = Source::Compile {
            cargo_args: vec![],
            build_with: BuildWith::Cargo,
        };
        opts.settings.require_broker = true;
        let e = run(&opts).unwrap_err();
        assert!(e.0.contains("plain cargo is refused"), "{e}");
        assert!(
            !fx.managed.join("runs").exists(),
            "no run directory was left behind"
        );
        let _ = std::fs::remove_dir_all(&fx.dir);
    }
}
