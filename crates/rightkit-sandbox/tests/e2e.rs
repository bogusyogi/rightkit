//! End-to-end: real processes, real files, validated by content.
#[cfg(unix)]
use rightkit_sandbox::*;

#[cfg(unix)]
use std::fs;

#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const T: Duration = Duration::from_secs(20);

#[cfg(unix)]
fn scratch(name: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!(
        "rk-sandbox-{name}-{}-{nanos}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&d).unwrap();
    fs::canonicalize(&d).unwrap()
}

#[cfg(unix)]
fn sh(script: &str) -> SandboxCommand {
    SandboxCommand::new("/bin/sh").arg("-c").arg(script)
}

#[cfg(unix)]
fn expanded() -> Sandbox {
    Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded))
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_write_and_read_outside_allowed_dir_but_allows_inside() {
    let root = scratch("fs");
    let (allowed, outside) = (root.join("allowed"), root.join("outside"));
    fs::create_dir_all(&allowed).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("private.txt"), "OUTSIDE-DATA").unwrap();
    let policy = SandboxPolicy::new().allow_write(&allowed);
    let script = format!(
        "echo inside > {a}/in.txt; echo outside > {o}/out.txt; cat {o}/private.txt",
        a = allowed.display(),
        o = outside.display()
    );
    let child = expanded()
        .spawn(&policy, sh(&script).cwd(&allowed))
        .unwrap();
    let out = child.wait_with_output(T).unwrap();

    assert_eq!(
        fs::read_to_string(allowed.join("in.txt")).unwrap(),
        "inside\n",
        "write inside must succeed"
    );
    assert!(
        !outside.join("out.txt").exists(),
        "write outside must NOT create the file"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("OUTSIDE-DATA"),
        "read outside must be denied"
    );
    assert!(!out.status.unwrap().success());
    let denial = out.denial.expect("denial reported");
    assert_eq!(denial.category, DenialCategory::Filesystem);
    assert_eq!(denial.source, DenialSource::MacosSeatbelt);
    let _ = fs::remove_dir_all(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn network_deny_blocks_loopback_connect_and_allow_permits_it() {
    let root = scratch("net");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "/usr/bin/nc -z 127.0.0.1 {port} && echo reached > {}/marker",
        root.display()
    );
    for (net, expect) in [(NetworkPolicy::Deny, false), (NetworkPolicy::Allow, true)] {
        let _ = fs::remove_file(root.join("marker"));
        let policy = SandboxPolicy::new().allow_write(&root).network(net);
        expanded()
            .spawn(&policy, sh(&script))
            .unwrap()
            .wait_with_output(T)
            .unwrap();
        assert_eq!(root.join("marker").exists(), expect, "network {net:?}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn env_is_scrubbed_but_explicit_values_pass() {
    std::env::set_var("RK_SANDBOX_PROBE", "parent-only-value");
    let mut env = EnvPolicy::default();
    env.set.insert("RK_VISIBLE".into(), "yes".into());
    let out = expanded()
        .spawn_supervised(SandboxCommand::new("/usr/bin/env"), &env)
        .unwrap()
        .wait_with_output(T)
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("parent-only-value") && !text.contains("RK_SANDBOX_PROBE"),
        "{text}"
    );
    assert!(text.contains("RK_VISIBLE=yes"), "{text}");
}

#[cfg(all(unix, feature = "pty"))]
#[test]
fn pty_echo_with_lease_epoch_fencing_rejects_stale_epoch() {
    let pty = expanded()
        .spawn_pty(None, SandboxCommand::new("/bin/cat"), PtySize::default())
        .unwrap();
    let lease = pty.lease();
    assert_eq!((lease.epoch, lease.attached), (1, true));

    pty.write_input(1, 0, b"hello-pty\n").unwrap();
    assert!(
        pty.wait_for_output(b"hello-pty", T),
        "child echoed input: {:?}",
        String::from_utf8_lossy(&pty.output())
    );
    // Out-of-order sequence is rejected.
    assert!(matches!(
        pty.write_input(1, 5, b"x\n"),
        Err(PtyError::BadSequence {
            expected: 1,
            got: 5
        })
    ));
    // Unacknowledged input blocks a silent reattach.
    pty.detach();
    assert!(matches!(
        pty.write_input(1, 1, b"x\n"),
        Err(PtyError::Detached)
    ));
    assert!(matches!(
        pty.attach(),
        Err(PtyError::UnacknowledgedInput { from: 0, to: 1 })
    ));
    pty.acknowledge_input(1, 1).unwrap();
    let fenced = pty.attach().unwrap();
    assert_eq!(fenced.epoch, 2);

    // The stale epoch is rejected and its bytes never reach the child.
    assert!(matches!(
        pty.write_input(1, 1, b"STALE-BYTES\n"),
        Err(PtyError::StaleEpoch {
            presented: 1,
            current: 2
        })
    ));
    pty.write_input(2, 1, b"fresh-bytes\n").unwrap();
    assert!(pty.wait_for_output(b"fresh-bytes", T));
    let all = String::from_utf8_lossy(&pty.output()).to_string();
    assert!(!all.contains("STALE-BYTES"), "stale input leaked: {all}");
}

#[cfg(all(target_os = "macos", feature = "pty"))]
#[test]
fn sandboxed_pty_child_is_denied_outside_write() {
    let root = scratch("pty-sbx");
    let (allowed, outside) = (root.join("allowed"), root.join("outside"));
    fs::create_dir_all(&allowed).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let policy = SandboxPolicy::new().allow_write(&allowed);
    let script = format!(
        "echo in > {}/in.txt; echo out > {}/out.txt; echo done-marker",
        allowed.display(),
        outside.display()
    );
    let pty = expanded()
        .spawn_pty(Some(&policy), sh(&script), PtySize::default())
        .unwrap();
    assert!(
        pty.wait_for_output(b"done-marker", T),
        "{:?}",
        String::from_utf8_lossy(&pty.output())
    );
    assert_eq!(fs::read_to_string(allowed.join("in.txt")).unwrap(), "in\n");
    assert!(!outside.join("out.txt").exists());
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
fn wait_pid_file(p: &Path) -> u32 {
    for _ in 0..400 {
        if let Ok(s) = fs::read_to_string(p) {
            if let Ok(n) = s.trim().parse() {
                return n;
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("grandchild pid file never appeared");
}

#[cfg(unix)]
#[test]
fn process_tree_kill_kills_grandchildren_including_group_escapers() {
    let root = scratch("tree");
    let (plain, escaper) = (root.join("plain.pid"), root.join("escaper.pid"));
    // `set -m` gives the background job its own process group, so killpg alone would miss it.
    let script = format!(
        "sleep 300 & echo $! > {p}; (set -m; sleep 301 & echo $! > {e}; wait) & wait",
        p = plain.display(),
        e = escaper.display()
    );
    let mut child = expanded()
        .spawn_supervised(sh(&script), &EnvPolicy::default())
        .unwrap();
    let (gp, ge) = (wait_pid_file(&plain), wait_pid_file(&escaper));
    assert!(process_tree::is_alive(gp) && process_tree::is_alive(ge));
    let tree = process_tree::descendants(child.id()).unwrap();
    assert!(
        tree.contains(&gp) && tree.contains(&ge),
        "descendants {tree:?} missing {gp}/{ge}"
    );

    let report = process_tree::kill_tree(child.id(), Duration::from_millis(300)).unwrap();
    assert!(
        report.survivors.is_empty(),
        "survivors: {:?}",
        report.survivors
    );
    child.terminate_tree().unwrap();
    assert!(!process_tree::is_alive(gp), "plain grandchild survived");
    assert!(!process_tree::is_alive(ge), "escaped grandchild survived");
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn strict_profile_denies_spawn_before_any_process_exists_and_slots_release() {
    let root = scratch("strict");
    let marker = root.join("ran");
    let script = format!("echo ran > {}", marker.display());

    let strict = Sandbox::new(ResourceAuthority::new(ResourceProfile::Strict));
    let policy = SandboxPolicy::new().allow_write(&root);
    match strict.spawn(&policy, sh(&script)) {
        Err(SandboxError::ResourceDenied(d)) => {
            assert_eq!((d.profile, d.limit), (ResourceProfile::Strict, 0));
            assert_eq!(d.code(), "resource_admission_denied");
        }
        other => panic!("expected typed denial, got {:?}", other.map(|c| c.id())),
    }
    assert!(strict
        .spawn_supervised(sh(&script), &EnvPolicy::default())
        .is_err());
    #[cfg(feature = "pty")]
    assert!(strict
        .spawn_pty(None, sh(&script), PtySize::default())
        .is_err());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists(), "a process ran despite Strict denial");

    // Practical: one active execution group; the slot returns when the child is gone.
    let practical = Sandbox::new(ResourceAuthority::new(ResourceProfile::Practical));
    let hold = practical
        .spawn_supervised(
            SandboxCommand::new("/bin/sleep").arg("30"),
            &EnvPolicy::default(),
        )
        .unwrap();
    assert!(matches!(
        practical.spawn_supervised(sh(&script), &EnvPolicy::default()),
        Err(SandboxError::ResourceDenied(_))
    ));
    assert!(!marker.exists());
    drop(hold);
    practical
        .spawn_supervised(sh(&script), &EnvPolicy::default())
        .unwrap()
        .wait_with_output(T)
        .unwrap();
    assert_eq!(fs::read_to_string(&marker).unwrap(), "ran\n");
    let _ = fs::remove_dir_all(&root);
}

/// Windows AppContainer: real confined processes, validated by content.
#[cfg(windows)]
#[path = "common/mod.rs"]
mod common;

#[cfg(windows)]
mod windows {
    use rightkit_sandbox::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    const T: Duration = Duration::from_secs(60);

    /// A fresh directory per call: pid, a timestamp and a counter, so a leftover from an earlier
    /// run (or another test of this run) can never be reused.
    fn scratch(name: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "rk-sandbox-{name}-{}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&d).unwrap();
        let c = fs::canonicalize(&d).unwrap().to_string_lossy().to_string();
        PathBuf::from(c.strip_prefix(r"\\?\").unwrap_or(&c))
    }

    fn cmd(script: &str) -> SandboxCommand {
        SandboxCommand::new("cmd.exe").args(["/d", "/c", script])
    }

    fn expanded() -> Sandbox {
        Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded))
    }

    /// The live DACL of `path` as `icacls` prints it, for a failure message. Call it while the
    /// child that holds the leases is still alive: dropping the child revokes its ACEs.
    fn icacls(path: &std::path::Path) -> String {
        match std::process::Command::new("icacls.exe").arg(path).output() {
            Ok(out) => format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => format!("icacls {} failed to start: {e}", path.display()),
        }
    }

    #[test]
    fn appcontainer_denies_write_and_read_outside_allowed_dir_but_allows_inside() {
        if !crate::common::real_user_stores_allowed(
            "appcontainer_denies_write_and_read_outside_allowed_dir_but_allows_inside",
        ) {
            return;
        }
        let root = scratch("fs");
        let (allowed, outside) = (root.join("allowed"), root.join("outside"));
        fs::create_dir_all(&allowed).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("private.txt"), "OUTSIDE-DATA").unwrap();
        let policy = SandboxPolicy::new().allow_write(&allowed);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let script = format!(
            r"echo inside> {a}\in.txt & echo outside> {o}\out.txt & type {o}\private.txt",
            a = allowed.display(),
            o = outside.display()
        );
        let out = expanded()
            .spawn(&policy, cmd(&script).cwd(&allowed))
            .unwrap()
            .wait_with_output(T)
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            fs::read_to_string(allowed.join("in.txt"))
                .unwrap()
                .trim_end(),
            "inside",
            "write inside must succeed: {stdout} {stderr}"
        );
        assert!(
            !outside.join("out.txt").exists(),
            "write outside must NOT create the file"
        );
        assert!(
            !stdout.contains("OUTSIDE-DATA"),
            "read outside must be denied"
        );
        assert!(!out.status.unwrap().success());
        let denial = out.denial.expect("denial reported");
        assert_eq!(denial.category, DenialCategory::Filesystem, "{stderr}");
        assert_eq!(denial.source, DenialSource::WindowsAppContainer);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cwd_inside_a_grant_is_honoured_and_outside_every_grant_is_refused() {
        if !crate::common::real_user_stores_allowed(
            "cwd_inside_a_grant_is_honoured_and_outside_every_grant_is_refused",
        ) {
            return;
        }
        let root = scratch("cwd");
        let (allowed, nested, outside) = (
            root.join("a"),
            root.join("a").join("deep dir"),
            root.join("o"),
        );
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let policy = SandboxPolicy::new().allow_write(&allowed);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let out = expanded()
            .spawn(&policy, cmd("echo here> marker.txt").cwd(&nested))
            .unwrap()
            .wait_with_output(T)
            .unwrap();
        assert!(
            out.status.unwrap().success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            fs::read_to_string(nested.join("marker.txt")).unwrap(),
            "here\r\n"
        );
        assert!(matches!(
            expanded().spawn(&policy, cmd("echo x").cwd(&outside)),
            Err(SandboxError::InvalidPolicy(_))
        ));
        let _ = fs::remove_dir_all(&root);
    }

    /// (is the token an AppContainer, its capability SIDs) for a live process.
    fn token_capabilities(pid: u32) -> (bool, Vec<String>) {
        use std::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
            fn CloseHandle(h: *mut c_void) -> i32;
            fn LocalFree(m: *mut c_void) -> *mut c_void;
        }
        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn OpenProcessToken(p: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
            fn GetTokenInformation(
                t: *mut c_void,
                class: u32,
                info: *mut c_void,
                len: u32,
                ret: *mut u32,
            ) -> i32;
            fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
        }
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const TOKEN_QUERY: u32 = 0x8;
        const TOKEN_IS_APP_CONTAINER: u32 = 29;
        const TOKEN_CAPABILITIES: u32 = 30;
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            assert!(!process.is_null(), "open confined child");
            let mut token = std::ptr::null_mut();
            assert_ne!(OpenProcessToken(process, TOKEN_QUERY, &mut token), 0);
            let mut is_container = 0u32;
            let mut len = 0u32;
            assert_ne!(
                GetTokenInformation(
                    token,
                    TOKEN_IS_APP_CONTAINER,
                    (&mut is_container as *mut u32).cast(),
                    4,
                    &mut len
                ),
                0
            );
            GetTokenInformation(token, TOKEN_CAPABILITIES, std::ptr::null_mut(), 0, &mut len);
            // u64 storage keeps the TOKEN_GROUPS pointer array aligned.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            assert_ne!(
                GetTokenInformation(
                    token,
                    TOKEN_CAPABILITIES,
                    buf.as_mut_ptr().cast(),
                    len,
                    &mut len
                ),
                0
            );
            // TOKEN_GROUPS { u32 count; SID_AND_ATTRIBUTES groups[] } (pointer-aligned array).
            let count = *(buf.as_ptr() as *const u32) as usize;
            let groups =
                (buf.as_ptr() as *const u8).add(std::mem::size_of::<usize>()) as *const [usize; 2];
            let mut sids = Vec::new();
            for i in 0..count {
                let sid = (*groups.add(i))[0] as *mut c_void;
                let mut text = std::ptr::null_mut();
                assert_ne!(ConvertSidToStringSidW(sid, &mut text), 0);
                let n = (0..).take_while(|&k| *text.add(k) != 0).count();
                sids.push(String::from_utf16_lossy(std::slice::from_raw_parts(
                    text, n,
                )));
                LocalFree(text.cast());
            }
            CloseHandle(token);
            CloseHandle(process);
            (is_container != 0, sids)
        }
    }

    #[test]
    fn network_capability_follows_policy() {
        if !crate::common::real_user_stores_allowed("network_capability_follows_policy") {
            return;
        }
        let root = scratch("net");
        for (net, expect) in [(NetworkPolicy::Deny, false), (NetworkPolicy::Allow, true)] {
            let policy = SandboxPolicy::new().allow_write(&root).network(net);
            let _profile = crate::common::ProfileCleanup::of(&policy);
            let mut child = expanded()
                .spawn(
                    &policy,
                    SandboxCommand::new("powershell.exe").args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        "Start-Sleep 30",
                    ]),
                )
                .unwrap();
            let (is_container, capabilities) = token_capabilities(child.id());
            child.terminate_tree().unwrap();
            assert!(is_container, "child must run in an AppContainer");
            assert_eq!(
                capabilities.iter().any(|s| s == "S-1-15-3-1"),
                expect,
                "network {net:?}: internetClient only on Allow, got {capabilities:?}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn env_is_scrubbed_but_explicit_values_pass_into_the_container() {
        if !crate::common::real_user_stores_allowed(
            "env_is_scrubbed_but_explicit_values_pass_into_the_container",
        ) {
            return;
        }
        std::env::set_var("RK_SANDBOX_PROBE", "parent-only-value");
        let root = scratch("env");
        let mut env = EnvPolicy::default();
        env.set.insert("RK_VISIBLE".into(), "yes".into());
        let policy = SandboxPolicy::new().allow_write(&root).env(env);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let out = expanded()
            .spawn(&policy, cmd("set"))
            .unwrap()
            .wait_with_output(T)
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("parent-only-value") && !text.contains("RK_SANDBOX_PROBE"),
            "{text}"
        );
        assert!(text.contains("RK_VISIBLE=yes"), "{text}");
        assert!(
            text.to_ascii_lowercase().contains(r"\packages"),
            "TEMP is container-rooted: {text}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Not a test on its own: the blocking grandchild of `confined_grandchildren_die_with_the_child`.
    /// A no-op unless `RK_BLOCK_READY` names a file. Then it writes its pid there (temp file and
    /// rename, so a reader never sees a partial pid) and blocks without touching stdin, the
    /// network or a console.
    #[test]
    fn rk_block_helper() {
        let Some(ready) = std::env::var_os("RK_BLOCK_READY") else {
            return;
        };
        let ready = PathBuf::from(ready);
        let temp = ready.with_extension("tmp");
        fs::write(&temp, std::process::id().to_string()).unwrap();
        fs::rename(&temp, &ready).unwrap();
        std::thread::sleep(Duration::from_secs(600));
    }

    /// Not a test on its own: the confined child of the blocking-grandchild tests. A no-op unless
    /// `RK_BLOCK_READY` is set. Then it starts this test binary again as `rk_block_helper` (a
    /// real grandchild of the sandbox's child) and waits for it.
    #[test]
    fn rk_block_parent() {
        if std::env::var_os("RK_BLOCK_READY").is_none() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let _ = std::process::Command::new(exe)
            .args(["--exact", "windows::rk_block_helper", "--nocapture"])
            .status();
    }

    /// The path spelling `AdoptedIdentity::exe` and `current_exe` share once case and the
    /// verbatim prefix are removed.
    fn exe_key(path: &std::path::Path) -> String {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
    }

    /// Launch the confined parent, wait for the grandchild's ready file, and return the confined
    /// child with the verified identities of the parent and the grandchild (a descendant of the
    /// parent whose executable is this test binary). Panics with the diagnostics a hung launch
    /// needs.
    fn spawn_blocker(
        root: &std::path::Path,
        policy: &SandboxPolicy,
    ) -> (
        SandboxedChild,
        rightkit_child_process::AdoptedIdentity,
        rightkit_child_process::AdoptedIdentity,
    ) {
        let exe = std::env::current_exe().unwrap();
        let ready = root.join("ready.txt");
        let mut child = expanded()
            .spawn(
                policy,
                SandboxCommand::new(&exe)
                    .args(["--exact", "windows::rk_block_parent", "--nocapture"])
                    .env("RK_BLOCK_READY", ready.to_string_lossy().to_string())
                    .cwd(root),
            )
            .unwrap();
        let deadline = Instant::now() + T;
        let pid: u32 = loop {
            if let Some(pid) = fs::read_to_string(&ready)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            if Instant::now() >= deadline {
                // Name what the confined parent did, instead of only that nothing appeared.
                let exited = child.has_exited().unwrap_or(false);
                let code = child.observed_exit_code().ok().flatten();
                let acls = icacls(root);
                let _ = child.terminate_tree();
                let out = child.wait_with_output(Duration::from_secs(10));
                let (stdout, stderr) = match &out {
                    Ok(o) => (
                        String::from_utf8_lossy(&o.stdout).into_owned(),
                        String::from_utf8_lossy(&o.stderr).into_owned(),
                    ),
                    Err(e) => (String::new(), format!("collecting output failed: {e}")),
                };
                panic!(
                    "confined grandchild never started: parent_exited={exited} exit_code={code:?}\n\
                     ready file exists={}\nstdout: {stdout}\nstderr: {stderr}\nroot DACL:\n{acls}",
                    ready.exists()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let parent = rightkit_child_process::AdoptedIdentity::capture(
            std::num::NonZeroU32::new(child.id()).unwrap(),
        )
        .unwrap()
        .expect("the confined child is running");
        let helper = parent
            .descendants()
            .unwrap()
            .into_iter()
            .find(|d| d.pid() == pid)
            .unwrap_or_else(|| panic!("pid {pid} is not a descendant of the confined child"));
        assert_eq!(
            helper.exe().map(exe_key),
            Some(exe_key(&exe)),
            "the grandchild is not this test binary"
        );
        assert_eq!(
            helper.verify().unwrap(),
            rightkit_child_process::IdentityCheck::Same
        );
        (child, parent, helper)
    }

    #[test]
    fn confined_grandchildren_die_with_the_child() {
        if !crate::common::real_user_stores_allowed("confined_grandchildren_die_with_the_child") {
            return;
        }
        let root = scratch("tree");
        let policy = SandboxPolicy::new().allow_write(&root);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let (child, parent, helper) = spawn_blocker(&root, &policy);
        // It blocks: a grandchild that exited by itself must not pass for one that was killed.
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            helper.is_running().unwrap(),
            "the blocking grandchild exited by itself"
        );
        drop(child);
        assert!(
            helper.wait_confirmed_gone(Duration::from_secs(10)).unwrap(),
            "grandchild survived its child"
        );
        assert!(
            parent.wait_confirmed_gone(Duration::from_secs(10)).unwrap(),
            "the confined child survived its drop"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Negative control: while the child lives, the blocking grandchild stays up, so the test
    /// above is not satisfied by a helper that dies on its own.
    #[test]
    fn a_blocking_grandchild_outlives_the_wait_while_its_child_lives() {
        if !crate::common::real_user_stores_allowed(
            "a_blocking_grandchild_outlives_the_wait_while_its_child_lives",
        ) {
            return;
        }
        let root = scratch("tree-control");
        let policy = SandboxPolicy::new().allow_write(&root);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let (child, _parent, helper) = spawn_blocker(&root, &policy);
        assert!(
            !helper.wait_confirmed_gone(Duration::from_secs(3)).unwrap(),
            "the grandchild died while its child was alive"
        );
        drop(child);
        assert!(helper.wait_confirmed_gone(Duration::from_secs(10)).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    /// Run the same read the confined script would make, outside any sandbox. Proves the secret
    /// is readable to the user, so a refused or empty confined read means something.
    fn unsandboxed_type(path: &std::path::Path) -> String {
        let out = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "type"])
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "the unsandboxed read failed: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn deny_read_inside_a_grant_is_refused_before_a_profile_exists() {
        if !crate::common::real_user_stores_allowed(
            "deny_read_inside_a_grant_is_refused_before_a_profile_exists",
        ) {
            return;
        }
        // Issue #887: a lowbox token ignores DENY ACEs, so a deny cannot be carved out of a grant.
        // Windows fails closed: the spawn is refused with a typed error and no profile exists.
        let root = scratch("deny");
        fs::write(root.join("public.txt"), "PUBLIC").unwrap();
        fs::create_dir_all(root.join("secrets")).unwrap();
        let key = root.join("secrets").join("key.txt");
        fs::write(&key, "TOP-SECRET").unwrap();
        assert!(unsandboxed_type(&key).contains("TOP-SECRET"));
        let policy = SandboxPolicy::new()
            .allow_write(&root)
            .deny_read(root.join("secrets"));
        let name = appcontainer_profile_name_for_test(&policy).unwrap();
        match expanded().spawn(&policy, cmd("type secrets\\key.txt").cwd(&root)) {
            Err(SandboxError::Unsupported(why)) => {
                assert!(why.contains("887"), "{why}");
            }
            other => panic!("a deny inside a grant must be refused: {:?}", other.err()),
        }
        // Existence is read from the profile's registry mapping: on current Windows a delete of a
        // name that was never registered also reports success, so it cannot prove absence.
        assert!(
            !appcontainer_profile_exists_for_test(&name).unwrap(),
            "no profile may have been created"
        );
        let _ = delete_appcontainer_profile_for_test(&name);
        assert_eq!(fs::read_to_string(&key).unwrap(), "TOP-SECRET");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deny_outside_every_grant_is_refused_too() {
        if !crate::common::real_user_stores_allowed("a_deny_outside_every_grant_is_refused_too") {
            return;
        }
        let root = scratch("deny-out");
        let other = scratch("deny-out-secret");
        let key = other.join("key.txt");
        fs::write(&key, "TOP-SECRET").unwrap();
        assert!(unsandboxed_type(&key).contains("TOP-SECRET"));
        let policy = SandboxPolicy::new().allow_write(&root).deny_read(&other);
        let name = appcontainer_profile_name_for_test(&policy).unwrap();
        match expanded().spawn(&policy, cmd("echo x").cwd(&root)) {
            Err(SandboxError::Unsupported(_)) => {}
            other => panic!("any deny_read entry must be refused: {:?}", other.err()),
        }
        assert!(
            !appcontainer_profile_exists_for_test(&name).unwrap(),
            "no profile may have been created"
        );
        let _ = delete_appcontainer_profile_for_test(&name);
        assert!(!deny_read_enforced(), "Windows must not claim a carve-out");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other);
    }

    #[test]
    fn a_piped_child_reads_its_stdin_until_the_parent_closes_it() {
        use std::io::Write;
        if !crate::common::real_user_stores_allowed(
            "a_piped_child_reads_its_stdin_until_the_parent_closes_it",
        ) {
            return;
        }
        let root = scratch("piped");
        let policy = SandboxPolicy::new().allow_write(&root);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let mut child = expanded()
            .spawn_piped(
                &policy,
                SandboxCommand::new("findstr.exe").arg("^").cwd(&root),
            )
            .unwrap();
        let mut stdin = child.take_stdin().expect("a piped child has stdin");
        stdin.write_all(b"through-the-pipe\r\n").unwrap();
        drop(stdin);
        let out = child.wait_with_output(T).unwrap();
        assert!(!out.timed_out, "findstr must see end of input");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("through-the-pipe"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_workspace_profile_is_reusable_and_refuses_protected_grants() {
        if !crate::common::real_user_stores_allowed(
            "a_workspace_profile_is_reusable_and_refuses_protected_grants",
        ) {
            return;
        }
        let root = scratch("workspace-profile");
        let profile = AppContainerProfile::for_workspace(&root).expect("profile");
        crate::common::assert_test_name(profile.name(), "rightkit.sandbox.");
        let _cleanup = ProfileNameCleanup(profile.name().to_string());
        assert!(profile.sid_string().starts_with("S-1-15-2-"));
        assert!(!profile.network_allowed());
        for round in 0..2 {
            let out = expanded()
                .spawn_in_profile(
                    &profile,
                    cmd(&format!("echo round{round}> marker{round}.txt")).cwd(&root),
                    &EnvPolicy::default(),
                )
                .unwrap()
                .wait_with_output(T)
                .unwrap();
            assert!(
                out.status.unwrap().success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(root.join(format!("marker{round}.txt")).exists());
        }
        if let Some(secret) = secret_roots().into_iter().find(|p| p.exists()) {
            assert!(
                profile.lease_grant(&secret, false).is_err(),
                "a secret is never grantable"
            );
        }
        let filesystem_root = PathBuf::from(r"C:\");
        assert!(profile.lease_grant(&filesystem_root, false).is_err());
        // The profile (and its leases) go before its registration is deleted.
        drop(profile);
        drop(_cleanup);
        let _ = fs::remove_dir_all(&root);
    }

    /// Deletes a profile by name when the test ends.
    struct ProfileNameCleanup(String);

    impl Drop for ProfileNameCleanup {
        fn drop(&mut self) {
            let _ = delete_appcontainer_profile_for_test(&self.0);
        }
    }
}
