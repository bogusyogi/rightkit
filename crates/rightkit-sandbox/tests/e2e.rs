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
    let d = std::env::temp_dir().join(format!("rk-sandbox-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
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

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rk-sandbox-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
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

    #[test]
    fn confined_grandchildren_die_with_the_child() {
        if !crate::common::real_user_stores_allowed("confined_grandchildren_die_with_the_child") {
            return;
        }
        let root = scratch("tree");
        let policy = SandboxPolicy::new().allow_write(&root);
        let _profile = crate::common::ProfileCleanup::of(&policy);
        let child = expanded()
            .spawn(
                &policy,
                cmd("powershell -NoProfile -NonInteractive -Command $PID ^| Set-Content pid.txt; Start-Sleep 300")
                    .cwd(&root),
            )
            .unwrap();
        let pid_file = root.join("pid.txt");
        let deadline = Instant::now() + T;
        let pid: u32 = loop {
            if let Some(pid) = fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "confined grandchild never started"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let grandchild = rightkit_process::AdoptedProcess::new(pid.try_into().unwrap());
        assert!(grandchild.is_running().unwrap());
        drop(child);
        let killed = Instant::now();
        while grandchild.is_running().unwrap_or(false) {
            assert!(
                killed.elapsed() < Duration::from_secs(5),
                "grandchild survived its child"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&root);
    }
}
