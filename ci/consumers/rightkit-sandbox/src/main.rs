// rightkit-sandbox 0.6.0 registry consumer, outside the workspace.
//
// Pins `rightkit-sandbox =0.6.0` exactly with default features. No path, git,
// workspace, patch, or test-seam dependencies. Runs on Windows and macOS
// runners; other hosts exercise the typed `Unsupported` path and exit nonzero.
//
// Proves, with public APIs only:
// - reserve-before-spawn: `Strict` refuses `ChildProcess` with typed
//   `ResourceDenied` (`resource_admission_denied`); `Practical` reserves,
//   releases on drop, and reserves again.
// - pure planning/classification: `glob_matches` anchored semantics,
//   `path_is_within` containment, `plan_deny_read_paths` refusing a
//   relative path, `classify_denial` filesystem vs unrecognized.
// - one confined child: `Sandbox::spawn` with `allow_read(ro)` +
//   `allow_write(rw)` runs the platform shell (`cmd.exe` on Windows,
//   `/bin/sh` elsewhere) with cwd inside `rw`, writes `allowed.txt`
//   there, exits successfully without a denial, and leaves `ro`
//   without the file.
//
// Not covered: the staged 0.5.1 Windows pipe-pair `BestEffort` ancestor
// leases and verbatim-prefix deny globs (need real-user stores; the
// package gate rows remain the proof), `spawn_piped` / `spawn_in_profile`
// / `spawn_with_pipe_pair`, PTY sessions (macOS-only with a policy),
// `confine_command`, loopback/AppContainer profiles, and network policy.
// State: one temp dir tree only, removed on drop. No network, no CFT,
// no assets, no real user data. Run only by the CI consumer lane.
use rightkit_sandbox::{
    classify_denial, glob_matches, path_is_protected_secret, path_is_within, plan_deny_read_paths,
    DenialCategory, DenialSource, ResourceAuthority, ResourceKind, ResourceProfile, Sandbox,
    SandboxCommand, SandboxError, SandboxPolicy,
};
use std::path::PathBuf;
use std::time::Duration;

type Failure = Box<dyn std::error::Error>;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rk-sandbox-{tag}-{}-{now}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn canonical(&self) -> PathBuf {
        std::fs::canonicalize(&self.0).unwrap_or_else(|_| self.0.clone())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Failure> {
    // Reserve-before-spawn: Strict denies before any process exists.
    let strict = ResourceAuthority::new(ResourceProfile::Strict);
    let denied = strict
        .reserve(ResourceKind::ChildProcess)
        .expect_err("Strict must deny a child-process reservation");
    assert_eq!(denied.code(), "resource_admission_denied");

    // Practical reserves, releases on drop, and reserves again.
    let practical = ResourceAuthority::new(ResourceProfile::Practical);
    {
        let _permit = practical
            .reserve(ResourceKind::ChildProcess)
            .expect("Practical must admit one child");
    }
    practical
        .reserve(ResourceKind::ChildProcess)
        .expect("drop must release the reservation");

    // Strict Sandbox::spawn refuses before spawning (typed, no process).
    let strict_sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Strict));
    let probe_policy = SandboxPolicy::new();
    #[cfg(windows)]
    let probe = SandboxCommand::new("cmd.exe").arg("/c").arg("echo probe");
    #[cfg(not(windows))]
    let probe = SandboxCommand::new("/bin/sh").arg("-c").arg("echo probe");
    match strict_sandbox.spawn(&probe_policy, probe) {
        Err(SandboxError::ResourceDenied(_)) => {}
        Ok(_) => panic!("Strict spawn must be ResourceDenied, but it spawned"),
        Err(other) => panic!("Strict spawn must be ResourceDenied, got {other}"),
    }

    // Pure planning helpers: anchored glob, containment, relative refusal.
    assert!(glob_matches("/tmp/rk-*.log", "/tmp/rk-a.log"));
    assert!(!glob_matches("/tmp/rk-*.log", "/tmp/rk-a.txt"));
    assert!(!glob_matches("/tmp/rk-*.log", "/tmp/sub/rk-a.log"));
    let tmp = TempDir::new("pure");
    let root = tmp.canonical();
    assert!(path_is_within(&root.join("a.txt"), &root));
    assert!(!path_is_within(&root, &root.join("a.txt")));
    assert!(plan_deny_read_paths(&[PathBuf::from("relative/deny")]).is_err());
    // A protected-secret probe must not crash the planner path query.
    let _ = path_is_protected_secret(&root.join("a.txt"));

    // Pure classification: filesystem vs unrecognized, never a guess.
    let fs = classify_denial("deny file-read: /tmp/blocked.txt", DenialSource::Unknown);
    assert_eq!(fs.category, DenialCategory::Filesystem);
    let unknown = classify_denial("hello ok", DenialSource::Unknown);
    assert_eq!(unknown.category, DenialCategory::Unrecognized);

    // One confined child: shell writes inside the writable grant only.
    let base = TempDir::new("spawn");
    let base_root = base.canonical();
    let ro = base_root.join("ro");
    let rw = base_root.join("rw");
    std::fs::create_dir_all(&ro)?;
    std::fs::create_dir_all(&rw)?;
    let ro = std::fs::canonicalize(&ro).unwrap_or(ro);
    let rw = std::fs::canonicalize(&rw).unwrap_or(rw);
    let policy = SandboxPolicy::new()
        .allow_read(ro.clone())
        .allow_write(rw.clone());
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Practical));
    #[cfg(windows)]
    let command = SandboxCommand::new("cmd.exe")
        .arg("/c")
        .arg("echo sandboxed-ok>allowed.txt")
        .cwd(rw.clone());
    #[cfg(not(windows))]
    let command = SandboxCommand::new("/bin/sh")
        .arg("-c")
        .arg("echo sandboxed-ok > allowed.txt")
        .cwd(rw.clone());
    let output = sandbox
        .spawn(&policy, command)?
        .wait_with_output(Duration::from_secs(30))?;
    let status = output.status.expect("child must report an exit status");
    assert!(status.success(), "confined echo must succeed");
    assert!(!output.timed_out, "confined echo must not time out");
    assert!(output.denial.is_none(), "clean run must carry no denial");
    let written = std::fs::read_to_string(rw.join("allowed.txt"))?;
    assert!(
        written.contains("sandboxed-ok"),
        "writable grant must receive the child file"
    );
    assert!(
        !ro.join("allowed.txt").exists(),
        "read-only grant must not gain the file"
    );

    println!("rightkit-sandbox 0.6.0 registry consumer passed");
    Ok(())
}
