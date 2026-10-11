//! Verifying the host from the client side: a real host process, real impostor processes (the
//! compiled examples `svc_host` and `svc_fakehost`), real sockets. The platform check itself is
//! per platform; the Windows tests host in the test process itself.

#[cfg(unix)]
mod unix_tests {
    use rightkit_service::*;
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// A compiled example next to the test binary (cargo builds examples for `cargo test`).
    fn example(name: &str) -> PathBuf {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples")
            .join(name)
    }

    /// A child process whose stdout lines arrive on a channel; killed on drop.
    struct Proc {
        child: Child,
        lines: mpsc::Receiver<String>,
    }

    impl Proc {
        fn start(bin: &Path, args: &[&str]) -> Proc {
            let mut child = Command::new(bin)
                .args(args)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| panic!("spawn {} (cargo builds examples): {e}", bin.display()));
            let out = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
            Proc { child, lines: rx }
        }

        /// The next line of stdout, or an empty string if none arrives in 20 seconds.
        fn line(&self) -> String {
            self.lines
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_default()
        }
    }

    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        suite: Suite,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let suite = Suite::at(dir.path().join("suite")).unwrap();
            Fixture { dir, suite }
        }
        fn root(&self) -> &str {
            self.suite.root().to_str().unwrap()
        }
        fn marker(&self) -> &str {
            self.dir.path().to_str().unwrap()
        }
        fn config(&self) -> ClientConfig {
            ClientConfig::new("demo", "alpha").suite(self.suite.clone())
        }
        /// A real `svc_host` run from `bin` (the genuine example, or a copy of it).
        fn start_host(&self, bin: &Path) -> (Proc, u32) {
            let host = Proc::start(bin, &[self.root(), "demo", "headless", "2", self.marker()]);
            let ready = host.line();
            assert!(ready.starts_with("READY"), "host did not start: {ready:?}");
            let pid = ready.split_whitespace().nth(1).unwrap().parse().unwrap();
            (host, pid)
        }
        /// An impostor `svc_fakehost` publishing `demo` in this fixture's suite.
        fn start_impostor(&self) -> Proc {
            let fake = Proc::start(&example("svc_fakehost"), &[self.root(), "demo"]);
            assert_eq!(fake.line(), "READY");
            fake
        }
    }

    fn canonical(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    fn refused(config: &ClientConfig, expect: HostExpectation) -> HostIdentityError {
        match Client::connect_expecting(config, &expect)
            .err()
            .expect("the host must be refused")
        {
            ConnectError::HostIdentity(e) => e,
            other => panic!("expected a host identity refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_expected_binary_is_accepted_and_plain_connect_stays_unauthenticated() {
        let fx = Fixture::new();
        let host_bin = example("svc_host");
        let (_host, host_pid) = fx.start_host(&host_bin);

        let client =
            Client::connect_expecting(&fx.config(), &HostExpectation::executable(host_bin.clone()))
                .unwrap_or_else(|e| panic!("the real host must be accepted: {e}"));
        let verified = client.verified_host().expect("the verified host is kept");
        assert_eq!(verified.pid, host_pid);
        assert_eq!(verified.executable, canonical(&host_bin));
        assert_eq!(
            client.call("echo", json!({"x": 1})).unwrap(),
            json!({"x": 1})
        );

        // `connect` is unchanged: it checks nothing about the program, and says so.
        let plain = Client::connect(&fx.suite, "demo", "alpha").unwrap();
        assert!(plain.verified_host().is_none());
        assert_eq!(plain.call("echo", json!(2)).unwrap(), json!(2));
    }

    #[test]
    fn an_impostor_is_refused_before_any_byte_is_sent() {
        let fx = Fixture::new();
        let fake = fx.start_impostor();
        let wanted = fx.dir.path().join("the-real-host");
        std::fs::write(&wanted, b"not a program").unwrap();

        let err = refused(&fx.config(), HostExpectation::executable(wanted.clone()));
        assert_eq!(err.reason, HostIdentityReason::ExecutableMismatch);
        assert_eq!(err.found_pid, Some(fake.child.id()));
        assert_eq!(
            err.found_executable,
            Some(canonical(&example("svc_fakehost")))
        );
        assert_eq!(err.expected, vec![wanted]);
        // What the impostor saw: a connection that closed without a single byte (no hello).
        assert_eq!(fake.line(), "CONN bytes=0");
    }

    #[test]
    fn an_empty_expectation_matches_nothing_and_sends_nothing() {
        let fx = Fixture::new();
        let fake = fx.start_impostor();
        let err = refused(&fx.config(), HostExpectation::any_of(Vec::<PathBuf>::new()));
        assert_eq!(err.reason, HostIdentityReason::NoExpectation);
        assert!(err.expected.is_empty());
        assert_eq!(fake.line(), "CONN bytes=0");
    }

    #[test]
    fn a_copy_of_the_real_host_speaks_the_protocol_and_is_still_refused() {
        let fx = Fixture::new();
        let real = example("svc_host");
        let copy = fx.dir.path().join("svc_host_copy");
        std::fs::copy(&real, &copy).unwrap();
        let (_host, copy_pid) = fx.start_host(&copy);

        // A perfect protocol implementation at the wrong path is an impostor.
        let err = refused(&fx.config(), HostExpectation::executable(real.clone()));
        assert_eq!(err.reason, HostIdentityReason::ExecutableMismatch);
        assert_eq!(err.found_pid, Some(copy_pid));
        assert_eq!(err.found_executable, Some(canonical(&copy)));

        // The check is about the program, so naming the copy accepts it, and any one of several
        // acceptable programs is enough.
        for expect in [
            HostExpectation::executable(copy.clone()),
            HostExpectation::any_of([real.clone(), copy.clone()]),
            HostExpectation::executable(real).or_executable(copy),
        ] {
            let client = Client::connect_expecting(&fx.config(), &expect)
                .unwrap_or_else(|e| panic!("the named program must be accepted: {e}"));
            assert_eq!(client.call("echo", json!(1)).unwrap(), json!(1));
        }
    }

    #[test]
    fn a_caller_verifier_can_still_refuse_a_host_whose_executable_matched() {
        let fx = Fixture::new();
        let host_bin = example("svc_host");
        let (_host, host_pid) = fx.start_host(&host_bin);

        let strict = HostExpectation::executable(host_bin.clone())
            .with_verifier(|host| Err(format!("pid {} has the wrong team", host.pid)));
        let err = refused(&fx.config(), strict);
        assert_eq!(
            err.reason,
            HostIdentityReason::Refused(format!("pid {host_pid} has the wrong team"))
        );
        assert_eq!(err.found_executable, Some(canonical(&host_bin)));

        let lenient = HostExpectation::executable(host_bin).with_verifier(|_| Ok(()));
        assert!(Client::connect_expecting(&fx.config(), &lenient).is_ok());
    }

    #[test]
    fn a_host_in_this_very_process_is_this_binary_and_a_stranger_is_not() {
        struct Echo;
        impl Handler for Echo {
            fn call(
                &self,
                _cx: &CallContext,
                method: &str,
                params: &Value,
                _emit: &Emit,
            ) -> Result<Value, ServiceError> {
                match method {
                    "echo" => Ok(params.clone()),
                    _ => Err(ServiceError::new("unknown_method", method)),
                }
            }
        }
        let fx = Fixture::new();
        let cfg = ServiceConfig::new(
            "demo",
            "test",
            5,
            "test",
            Allowlist::new().allow("*", ["echo"]),
        );
        let _hosted = host(&fx.suite, cfg, Arc::new(Echo)).expect("host starts");

        let me = std::env::current_exe().unwrap();
        let client = Client::connect_expecting(&fx.config(), &HostExpectation::executable(me))
            .unwrap_or_else(|e| panic!("this binary hosts the service: {e}"));
        assert_eq!(client.verified_host().unwrap().pid, std::process::id());
        assert_eq!(client.call("echo", json!("hi")).unwrap(), json!("hi"));

        let err = refused(
            &fx.config(),
            HostExpectation::executable(fx.dir.path().join("elsewhere")),
        );
        assert_eq!(err.reason, HostIdentityReason::ExecutableMismatch);
        assert_eq!(err.found_pid, Some(std::process::id()));
    }

    #[test]
    fn a_refusal_is_not_retried_and_collapses_to_the_host_identity_code() {
        let fx = Fixture::new();
        let _fake = fx.start_impostor();
        let policy = RetryPolicy {
            max_attempts: 5,
            backoff: Duration::from_millis(1),
            ..RetryPolicy::default()
        };
        let err = Client::call_retrying_expecting(
            &fx.config(),
            &HostExpectation::executable(fx.dir.path().join("the-real-host")),
            "echo",
            json!({}),
            policy,
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ERROR_HOST_IDENTITY);
        assert!(!err.retryable);
    }

    #[test]
    fn open_stream_expecting_refuses_an_impostor_before_writing() {
        let fx = Fixture::new();
        let fake = fx.start_impostor();
        let endpoint = read_record(&fx.suite, "demo").unwrap().endpoint;

        let err = open_stream_expecting(
            &endpoint,
            &HostExpectation::executable(fx.dir.path().join("the-real-host")),
        )
        .err()
        .expect("the impostor must be refused");
        assert!(matches!(err, ConnectError::HostIdentity(_)), "{err:?}");
        assert_eq!(fake.line(), "CONN bytes=0");

        // Naming the impostor's own program opens the stream; the caller may then write.
        let mut stream = open_stream_expecting(
            &endpoint,
            &HostExpectation::executable(example("svc_fakehost")),
        )
        .unwrap_or_else(|e| panic!("the named program must be accepted: {e}"));
        stream.writer.write_all(b"hello\n").unwrap();
        stream.writer.flush().unwrap();
        assert_eq!(fake.line(), "CONN bytes=6");
    }

    /// Run `svc_verify_client` as a launcher would: a clean base environment plus `env`.
    fn run_client(mode: &str, service: &str, extra: &[&str], env: &[(&str, &Path)]) -> String {
        let mut cmd = Command::new(example("svc_verify_client"));
        cmd.arg(mode).arg(service).args(extra);
        for key in [
            "RIGHTKIT_SUITE_ROOT",
            "RIGHT_SUITE_DIR",
            "ORTHIC_SHARED_DIR",
            "XDG_DATA_HOME",
        ] {
            cmd.env_remove(key);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("run svc_verify_client");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_hardened_client_ignores_every_environment_redirect_of_the_suite_root() {
        let fx = Fixture::new();
        let evil_root = fx.dir.path().join("evil-suite");
        let evil_home = fx.dir.path().join("evil-home");
        let default_root = Suite::per_user_default().unwrap().root().to_path_buf();

        for key in [
            "RIGHTKIT_SUITE_ROOT",
            "RIGHT_SUITE_DIR",
            "ORTHIC_SHARED_DIR",
        ] {
            let env = [(key, evil_root.as_path()), ("HOME", evil_home.as_path())];
            // Without the switch the launcher wins: this is the redirection.
            assert_eq!(
                run_client("resolve", "demo", &[], &env),
                format!("ROOT {}", evil_root.display()),
                "{key}"
            );
            // With it the root comes only from the account database.
            assert_eq!(
                run_client("resolve-hardened", "demo", &[], &env),
                format!("ROOT {}", default_root.display()),
                "{key}"
            );
            // An environment-derived suite passed in explicitly is refused, not trusted.
            let handed_in = run_client("resolve-env-suite-hardened", "demo", &[], &env);
            assert!(handed_in.starts_with("ERR suite"), "{key}: {handed_in}");
        }

        // HOME alone redirects the unhardened default and not the hardened one.
        let env = [("HOME", evil_home.as_path())];
        let moved = run_client("resolve", "demo", &[], &env);
        assert!(
            moved.starts_with(&format!("ROOT {}", evil_home.display())),
            "{moved}"
        );
        assert_eq!(
            run_client("resolve-hardened", "demo", &[], &env),
            format!("ROOT {}", default_root.display())
        );
    }

    #[test]
    fn a_redirecting_launcher_never_receives_what_a_hardened_or_expecting_client_sends() {
        let fx = Fixture::new();
        // Unique, so the user's own suite has no record for it and nothing real is touched.
        let service = format!("vh-{}", std::process::id());
        let attacker_root = fx.dir.path().join("attacker");
        let evil_home = fx.dir.path().join("evil-home");
        let fake = Proc::start(
            &example("svc_fakehost"),
            &[attacker_root.to_str().unwrap(), &service],
        );
        assert_eq!(fake.line(), "READY");
        let endpoint = read_record(&Suite::at(&attacker_root).unwrap(), &service)
            .expect("the impostor published a record")
            .endpoint;
        // The program the client means to talk to. It is not running.
        let real_host = example("svc_host");
        let real_host = real_host.to_str().unwrap();
        let env = [
            ("RIGHTKIT_SUITE_ROOT", attacker_root.as_path()),
            ("HOME", evil_home.as_path()),
        ];

        // 1. A release client ignores the environment, looks in the user's own suite, finds no
        //    host, and never touches the impostor's socket.
        let out = run_client("hardened", &service, &[real_host], &env);
        assert!(out.starts_with("ERR service service_unavailable"), "{out}");
        // A sentinel connection proves it: had (1) connected, its line would come first.
        let mut sentinel = UnixStream::connect(&endpoint).unwrap();
        sentinel.write_all(b"SENTINEL\n").unwrap();
        assert_eq!(fake.line(), "CONN bytes=9");
        drop(sentinel);

        // 2. A client that honours the environment but names its host follows the redirect, sees
        //    an impostor, and closes without a byte.
        let out = run_client("expect", &service, &[real_host], &env);
        assert!(out.starts_with("ERR host_identity"), "{out}");
        assert_eq!(fake.line(), "CONN bytes=0");

        // 3. A plain client follows the redirect and hands the impostor its hello.
        let out = run_client("connect", &service, &[], &env);
        assert!(out.starts_with("ERR service "), "{out}");
        let line = fake.line();
        let sent: usize = line
            .strip_prefix("CONN bytes=")
            .unwrap_or_else(|| panic!("unexpected line {line:?}"))
            .parse()
            .unwrap();
        assert!(sent > 0, "the plain client wrote nothing: {line}");
    }
}

#[cfg(windows)]
mod windows_tests {
    use rightkit_service::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Echo;
    impl Handler for Echo {
        fn call(
            &self,
            _cx: &CallContext,
            _method: &str,
            params: &Value,
            _emit: &Emit,
        ) -> Result<Value, ServiceError> {
            Ok(params.clone())
        }
    }

    /// A service hosted by this test process, so the pipe's server process is this executable.
    fn hosted(name: &str) -> (tempfile::TempDir, ClientConfig, impl Sized) {
        let dir = tempfile::tempdir().unwrap();
        let suite = Suite::at(dir.path().join("suite")).unwrap();
        let cfg = ServiceConfig::new(
            name,
            "test",
            5,
            "test",
            Allowlist::new().allow("*", ["echo"]),
        );
        let hosted = host(&suite, cfg, Arc::new(Echo)).expect("host starts");
        (dir, ClientConfig::new(name, "app").suite(suite), hosted)
    }

    fn refusal(config: &ClientConfig, expect: &HostExpectation) -> HostIdentityError {
        match Client::connect_expecting(config, expect).err() {
            Some(ConnectError::HostIdentity(e)) => e,
            other => panic!("expected a host identity refusal, got {other:?}"),
        }
    }

    fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
        let canonical = |p: &std::path::Path| {
            std::fs::canonicalize(p)
                .unwrap()
                .to_string_lossy()
                .to_lowercase()
        };
        canonical(a) == canonical(b)
    }

    #[test]
    fn the_expected_program_is_accepted_and_described() {
        let (_dir, config, _hosted) = hosted("vh-win-ok");
        let me = std::env::current_exe().unwrap();
        let client = Client::connect_expecting(&config, &HostExpectation::executable(&me))
            .expect("this process hosts the pipe and is the expected executable");
        let host = client
            .verified_host()
            .expect("a verified connection records its host");
        assert_eq!(host.pid, std::process::id());
        assert!(same_file(&host.executable, &me), "{:?}", host.executable);
        assert!(
            !host.executable.to_string_lossy().starts_with(r"\\?\"),
            "{:?}",
            host.executable
        );
        assert_eq!(host.uid, None);
        assert!(host
            .sid
            .as_deref()
            .is_some_and(|sid| sid.starts_with("S-1-")));
        // The verified connection carries requests.
        assert_eq!(
            client.call("echo", json!({ "n": 1 })).unwrap(),
            json!({ "n": 1 })
        );

        // Another spelling of the same path is the same program.
        let upper = PathBuf::from(me.to_string_lossy().to_uppercase());
        Client::connect_expecting(&config, &HostExpectation::executable(upper))
            .expect("NTFS ignores case");
    }

    #[test]
    fn another_program_is_refused_before_anything_is_sent() {
        let (_dir, config, _hosted) = hosted("vh-win-other");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let other = PathBuf::from(system).join("System32").join("cmd.exe");
        let e = refusal(&config, &HostExpectation::executable(&other));
        assert_eq!(e.reason, HostIdentityReason::ExecutableMismatch, "{e}");
        assert_eq!(e.found_pid, Some(std::process::id()));
        assert_eq!(e.expected, vec![other]);
        let found = e
            .found_executable
            .expect("the refusal names what was found");
        assert!(
            same_file(&found, &std::env::current_exe().unwrap()),
            "{found:?}"
        );

        // An expected path that does not exist matches nothing.
        let missing = std::env::temp_dir().join("rightkit-no-such-host.exe");
        let e = refusal(&config, &HostExpectation::executable(missing));
        assert_eq!(e.reason, HostIdentityReason::ExecutableMismatch, "{e}");
    }

    #[test]
    fn no_expectation_and_a_refusing_verifier_both_fail_closed() {
        let (_dir, config, _hosted) = hosted("vh-win-closed");
        let e = refusal(&config, &HostExpectation::default());
        assert_eq!(e.reason, HostIdentityReason::NoExpectation, "{e}");

        let me = std::env::current_exe().unwrap();
        let picky = HostExpectation::executable(&me)
            .with_verifier(|host| Err(format!("pid {} is not signed by us", host.pid)));
        let e = refusal(&config, &picky);
        assert_eq!(
            e.reason,
            HostIdentityReason::Refused(format!("pid {} is not signed by us", std::process::id())),
            "{e}"
        );
        assert_eq!(e.found_pid, Some(std::process::id()));

        // A verifier that accepts sees the described host.
        let seen = Arc::new(std::sync::Mutex::new(None));
        let record = seen.clone();
        let accepting = HostExpectation::executable(&me).with_verifier(move |host| {
            *record.lock().unwrap() = Some(host.pid);
            Ok(())
        });
        Client::connect_expecting(&config, &accepting).expect("accepted");
        assert_eq!(*seen.lock().unwrap(), Some(std::process::id()));
    }
}
