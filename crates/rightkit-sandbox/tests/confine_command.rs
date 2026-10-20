//! `confine_command` and the new policy knobs against real children (Linux and macOS). Each test
//! that needs a kernel facility the host may lack (Landlock in a container, say) says so and
//! passes, rather than pretending to have confined anything.
#![cfg(unix)]

use rightkit_sandbox::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rk-confine-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(script);
    cmd
}

/// Whether this host can confine the filesystem at all.
fn can_confine() -> bool {
    cfg!(target_os = "macos") || is_landlock_available()
}

#[test]
fn a_caller_owned_command_is_confined_to_the_write_root() {
    if !can_confine() {
        println!("skipped: this host has no filesystem confinement (no Landlock)");
        return;
    }
    let root = scratch("fs");
    let (allowed, outside) = (root.join("allowed"), root.join("outside"));
    fs::create_dir_all(&allowed).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let policy = SandboxPolicy::new()
        .allow_write(&allowed)
        .network(NetworkPolicy::Allow);
    let mut cmd = sh(&format!(
        "echo in > {a}/in.txt; echo out > {o}/out.txt",
        a = allowed.display(),
        o = outside.display()
    ));
    cmd.current_dir(&allowed);
    confine_command(&mut cmd, &policy).expect("confine");
    cmd.status().expect("spawn");
    assert_eq!(fs::read_to_string(allowed.join("in.txt")).unwrap(), "in\n");
    assert!(
        !outside.join("out.txt").exists(),
        "write outside must be denied"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_invalid_policy_never_reaches_a_spawn() {
    let mut cmd = Command::new("/bin/true");
    let error = confine_command(&mut cmd, &SandboxPolicy::new().allow_read("relative"))
        .expect_err("relative path");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn truncating_a_file_outside_the_grants_is_denied_when_the_kernel_can_say_so() {
        if landlock_abi_version().is_none_or(|abi| abi < 3) {
            println!(
                "skipped: needs Landlock ABI 3 (TRUNCATE); host reports {:?}",
                landlock_abi_version()
            );
            return;
        }
        let root = scratch("truncate");
        let (allowed, outside) = (root.join("allowed"), root.join("outside"));
        fs::create_dir_all(&allowed).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("keep.txt");
        fs::write(&victim, "KEEP-ME").unwrap();
        // The outside directory is readable, not writable: the content must survive a truncate.
        let policy = SandboxPolicy::new()
            .allow_write(&allowed)
            .allow_read(&outside)
            .network(NetworkPolicy::Allow);
        let mut cmd = sh(&format!(": > {}", victim.display()));
        confine_command(&mut cmd, &policy).expect("confine");
        let _ = cmd.status().expect("spawn");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "KEEP-ME");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pathless_policy_skips_landlock_only_when_asked() {
        // Network allowed, no seccomp, no paths, skip mode: nothing to apply, the command runs.
        let root = scratch("skip");
        let policy = SandboxPolicy::new()
            .network(NetworkPolicy::Allow)
            .landlock(LandlockMode::SkipWhenNoPaths);
        let mut cmd = sh(&format!("echo ok > {}/f.txt", root.display()));
        confine_command(&mut cmd, &policy).expect("confine");
        assert!(cmd.status().unwrap().success());
        assert_eq!(fs::read_to_string(root.join("f.txt")).unwrap(), "ok\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_only_mode_is_refused_on_linux() {
        let mut cmd = Command::new("/bin/true");
        let policy = SandboxPolicy::new().mode(ConfinementMode::WriteOnly);
        let error = confine_command(&mut cmd, &policy).expect_err("refused");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn deny_read_is_not_enforced_on_linux_and_says_so() {
        assert!(!deny_read_enforced());
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::time::Duration;

    /// A directory the write-only profile does NOT grant, for the "writes outside are denied"
    /// probes. The profile's `WRITE_ONLY_TEMP` set (src/seatbelt.rs) always lets a write-only
    /// child write under `/private/var/folders` (the per-user `$TMPDIR` tree) and the other
    /// system temp roots, so a probe under `std::env::temp_dir()` is writable by design and
    /// cannot show a denial. The probe lives under `$HOME` instead, and the precondition that it
    /// lies outside every fixed temp root is asserted, never assumed.
    fn outside_the_write_only_grant(name: &str) -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("precondition: HOME must name a directory for the outside-the-grant probe");
        let dir = home.join(format!(".rk-confine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        for fixed in [
            "/private/tmp",
            "/private/var/folders",
            "/private/var/tmp",
            "/tmp",
        ] {
            assert!(
                !dir.starts_with(fixed),
                "precondition: {} must lie outside the write-only temp grant {fixed}",
                dir.display()
            );
        }
        dir
    }

    fn sandbox() -> Sandbox {
        Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded))
    }

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn write_only_mode_reads_everywhere_and_writes_only_where_granted() {
        let root = scratch("write-only");
        let allowed = root.join("allowed");
        let outside = outside_the_write_only_grant("write-only-out");
        fs::create_dir_all(&allowed).unwrap();
        fs::write(outside.join("readable.txt"), "READABLE-OUTSIDE").unwrap();
        let policy = SandboxPolicy::new()
            .mode(ConfinementMode::WriteOnly)
            .allow_write(&allowed)
            .network(NetworkPolicy::Allow);
        let script = format!(
            "cat {o}/readable.txt > {a}/copy.txt; echo no > {o}/out.txt",
            a = allowed.display(),
            o = outside.display()
        );
        let child = sandbox()
            .spawn(
                &policy,
                SandboxCommand::new("/bin/sh")
                    .arg("-c")
                    .arg(script)
                    .cwd(&allowed),
            )
            .unwrap();
        let out = child.wait_with_output(T).unwrap();
        assert_eq!(
            fs::read_to_string(allowed.join("copy.txt")).unwrap(),
            "READABLE-OUTSIDE",
            "reads outside the grants stay open in write-only mode"
        );
        assert!(
            !outside.join("out.txt").exists(),
            "writes outside are denied"
        );
        assert!(!out.status.unwrap().success());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn deny_read_hides_a_file_inside_a_granted_root_and_leaves_its_neighbour() {
        let root = scratch("deny-read");
        fs::write(root.join("public.txt"), "PUBLIC").unwrap();
        fs::create_dir_all(root.join("secrets")).unwrap();
        fs::write(root.join("secrets").join("key.txt"), "TOP-SECRET").unwrap();
        for mode in [ConfinementMode::Strict, ConfinementMode::WriteOnly] {
            let policy = SandboxPolicy::new()
                .mode(mode)
                .allow_write(&root)
                .network(NetworkPolicy::Allow)
                .deny_read(root.join("secrets"));
            let script = format!(
                "cat {r}/public.txt > {r}/public.copy; cat {r}/secrets/key.txt > {r}/leak.txt",
                r = root.display()
            );
            let _ = fs::remove_file(root.join("public.copy"));
            let _ = fs::remove_file(root.join("leak.txt"));
            let child = sandbox()
                .spawn(
                    &policy,
                    SandboxCommand::new("/bin/sh")
                        .arg("-c")
                        .arg(script)
                        .cwd(&root),
                )
                .unwrap();
            let _ = child.wait_with_output(T).unwrap();
            assert_eq!(
                fs::read_to_string(root.join("public.copy")).unwrap(),
                "PUBLIC",
                "{mode:?}: the neighbour stays readable"
            );
            let leaked = fs::read_to_string(root.join("leak.txt")).unwrap_or_default();
            assert!(
                !leaked.contains("TOP-SECRET"),
                "{mode:?}: the secret leaked: {leaked:?}"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn exec_any_runs_a_script_from_an_unlisted_directory_and_the_default_does_not() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("exec-any");
        let tools = root.join("tools");
        fs::create_dir_all(&tools).unwrap();
        let script = tools.join("helper.sh");
        fs::write(&script, "#!/bin/sh\necho helper-ran\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        // `None`: the launch itself failed because the exec was denied.
        let run = |exec_any: bool| -> Option<String> {
            // Readable but not an exec path: only `exec_any` lets it run.
            let policy = SandboxPolicy::new()
                .allow_read(&tools)
                .allow_write(&root)
                .exec_any(exec_any)
                .network(NetworkPolicy::Allow);
            let child = sandbox()
                .spawn(&policy, SandboxCommand::new(&script).cwd(&root))
                .ok()?;
            let out = child.wait_with_output(T).unwrap();
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let denied = run(false);
        assert!(
            denied.is_none_or(|stdout| !stdout.contains("helper-ran")),
            "without exec_any an unlisted directory must not execute"
        );
        let allowed = run(true).expect("exec_any lets the launch succeed");
        assert!(
            allowed.contains("helper-ran"),
            "exec_any must run it: {allowed:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn from_profile_applies_a_caller_built_profile() {
        use rightkit_sandbox::seatbelt::{write_only_profile, Prepared};
        use std::os::unix::process::CommandExt;
        let root = scratch("from-profile");
        let outside = outside_the_write_only_grant("from-profile-out");
        let profile = write_only_profile(std::slice::from_ref(&root), NetworkPolicy::Allow);
        let prepared = Prepared::from_profile(profile).expect("profile");
        let mut cmd = sh(&format!(
            "echo in > {r}/in.txt; echo out > {o}/out.txt",
            r = root.display(),
            o = outside.display()
        ));
        // SAFETY: `apply` only calls sandbox_init on a profile prepared in the parent.
        unsafe {
            cmd.pre_exec(move || prepared.apply());
        }
        let _ = cmd.status().expect("spawn");
        assert_eq!(fs::read_to_string(root.join("in.txt")).unwrap(), "in\n");
        assert!(!outside.join("out.txt").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }
}
