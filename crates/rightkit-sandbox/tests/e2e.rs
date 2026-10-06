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

#[cfg(unix)]
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

#[cfg(target_os = "macos")]
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
