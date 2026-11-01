// the host app sandbox request shape: a thin adapter over `rightkit-sandbox`.
//
// The OS backends are RightKit's, except one piece:
// - **Linux** - seccomp-BPF (strict allowlist, network-only filter, architecture check) is
//   `rightkit_sandbox::confine_command`. Landlock stays local (`landlock.rs`,
//   RIGHTKIT-SERIOUS: RightKit's always adds `/proc`, `/etc`, `/sys`, `/dev` as readable roots,
//   which exposes `/proc/<pid>/environ` of same-user processes to the confined child (report T-2).
// - **macOS** - Seatbelt (`rightkit_sandbox::seatbelt`): the write-only tiers wrap the
//   command with `sandbox-exec` (`ConfinementMode::WriteOnly`, see
//   `shell::macos_sandbox_wrap_for_tier`); vendor drivers run deny-default in process.
// - **Windows** - AppContainer plus a Job Object; see `windows_appcontainer.rs`.
//
// What stays here is the host app's own model: [`SandboxConfig`] (the tier-shaped request the
// launch paths build) and its mapping onto the backends. A configured Linux restriction
// fails the launch when Landlock or seccomp is unavailable; selected isolation never
// silently becomes an unsandboxed run.

pub use rightkit_agent_policy::SandboxTier;

/// Configuration for process sandboxing.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Filesystem paths the sandboxed process may read (and execute) from.
    /// If both path lists are empty, no Landlock path restriction is applied.
    pub allowed_read_paths: Vec<std::path::PathBuf>,
    /// Filesystem paths the sandboxed process may write to.
    pub allowed_write_paths: Vec<std::path::PathBuf>,
    /// Whether to apply the strict seccomp allowlist (Linux). The allowlist omits
    /// the network syscalls, so it also denies egress.
    pub seccomp_enabled: bool,
    /// Whether to deny outbound network. Without `seccomp_enabled` this is the
    /// network-only seccomp filter on Linux.
    pub network_denied: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allowed_read_paths: Vec::new(),
            allowed_write_paths: Vec::new(),
            seccomp_enabled: false, // off by default; enabled by the `strict` tier
            network_denied: false,
        }
    }
}

impl SandboxConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allow reading from the given path (and all files under it if it's a directory).
    pub fn allow_read(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.allowed_read_paths.push(path.into());
        self
    }

    /// Allow writing to the given path.
    pub fn allow_write(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.allowed_write_paths.push(path.into());
        self
    }

    /// Enable seccomp syscall filtering.
    pub fn with_seccomp(mut self) -> Self {
        self.seccomp_enabled = true;
        self
    }

    /// Deny outbound network (see [`SandboxConfig::network_denied`]).
    pub fn deny_network(mut self) -> Self {
        self.network_denied = true;
        self
    }

    /// The RightKit policy for the seccomp half of this request (Linux): network denied with
    /// the strict allowlist (`seccomp_enabled`) or the network-only filter (`network_denied`).
    /// It lists no path and skips Landlock, so only the seccomp program is applied; the
    /// filesystem half is `sandbox::landlock` (see the RIGHTKIT-SERIOUS note there). `None` when
    /// the request does not deny the network.
    pub fn seccomp_policy(&self) -> Option<rightkit_sandbox::SandboxPolicy> {
        if !(self.seccomp_enabled || self.network_denied) {
            return None;
        }
        Some(
            rightkit_sandbox::SandboxPolicy::new()
                .network(rightkit_sandbox::NetworkPolicy::Deny)
                .seccomp(if self.seccomp_enabled {
                    rightkit_sandbox::SeccompMode::Allowlist
                } else {
                    rightkit_sandbox::SeccompMode::NetworkOnly
                })
                .landlock(rightkit_sandbox::LandlockMode::SkipWhenNoPaths),
        )
    }

    /// Confine `command` (Linux) to this request: Landlock for the listed paths (local, see
    /// `sandbox::landlock`), then RightKit's seccomp program when the network is denied. Each
    /// is prepared in the parent and applied by a syscall-only `pre_exec` hook, in that order.
    /// Call it after any earlier `pre_exec` closure that needs the pre-confinement state.
    /// Fails closed: an error means the command must not run. A read path is read plus
    /// execute, a write path everything; a relative path is refused rather than resolved
    /// against the child's working directory.
    #[cfg(target_os = "linux")]
    pub fn confine(&self, command: &mut std::process::Command) -> std::io::Result<()> {
        use std::os::unix::process::CommandExt as _;
        if let Some(path) = self
            .allowed_read_paths
            .iter()
            .chain(&self.allowed_write_paths)
            .find(|path| !path.is_absolute())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("sandbox path must be absolute: {}", path.display()),
            ));
        }
        let landlock = super::landlock::PreparedLandlock::new(
            &self.allowed_read_paths,
            &self.allowed_write_paths,
        )?;
        // SAFETY: `apply` performs only prctl and landlock_restrict_self on state prepared
        // above; nothing allocates in the child.
        unsafe {
            command.pre_exec(move || landlock.apply());
        }
        if let Some(policy) = self.seccomp_policy() {
            rightkit_sandbox::confine_command(command, &policy)?;
        }
        Ok(())
    }
}

/// Confine a vendor-driver `command` to its `executable`, `cwd` and managed `home`.
/// Egress stays allowed: vendor CLIs talk to their own provider.
///
/// - Linux: Landlock reads `executable`, `cwd` and the system roots; `home` is writable, and
///   `cwd` too when `cwd_writable`.
/// - macOS: deny-default Seatbelt, execute `executable`, read `cwd`, write `home` plus
///   temp. `exec_any` is deliberately NOT set: execution stays limited to the system
///   directories plus `executable` (the behaviour before this port); enabling it is a product
///   decision, see the wave-3 notes.
///
/// Registers a `pre_exec` hook after whatever the caller registered first (for example
/// `setpgid`). Fails closed.
#[cfg(unix)]
// RIGHTKIT-SERIOUS: RightKit's Strict Seatbelt profile allows ioctl and writes on /dev/ttys* and metadata reads everywhere, wider than the old driver profile (report T-4)
pub fn confine_driver_command(
    command: &mut std::process::Command,
    executable: &std::path::Path,
    cwd: &std::path::Path,
    cwd_writable: bool,
    home: &std::path::Path,
) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        // Seatbelt grants the working directory read-only on every driver launch.
        let _ = cwd_writable;
        let mut policy = rightkit_sandbox::SandboxPolicy::new()
            .allow_exec(executable)
            .allow_read(cwd)
            .allow_write(home)
            .network(rightkit_sandbox::NetworkPolicy::Allow);
        // Compilers, test runners and CLIs need scratch space.
        for temp in ["/private/tmp", "/private/var/tmp", "/private/var/folders"] {
            policy = policy.allow_write(temp);
        }
        rightkit_sandbox::confine_command(command, &policy)
    }
    #[cfg(target_os = "linux")]
    {
        let mut config = SandboxConfig::new()
            .allow_read(executable)
            .allow_read(cwd)
            .allow_read(home)
            .allow_write(home)
            .allow_read("/usr")
            .allow_read("/bin")
            .allow_read("/lib")
            .allow_read("/lib64")
            .allow_read("/etc/ssl");
        if cwd_writable {
            config = config.allow_write(cwd);
        }
        config.confine(command)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (command, executable, cwd, cwd_writable, home);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "driver sandbox is implemented for Linux and macOS only; refusing an unsandboxed launch",
        ))
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use rightkit_sandbox::{LandlockMode, NetworkPolicy, SeccompMode};

    #[test]
    fn default_config_is_permissive() {
        let config = SandboxConfig::default();
        assert!(config.allowed_read_paths.is_empty());
        assert!(!config.seccomp_enabled);
        // No network denial: no seccomp program at all.
        assert!(config.seccomp_policy().is_none());
    }

    #[test]
    fn builder_adds_paths() {
        let config = SandboxConfig::new()
            .allow_read("/tmp")
            .allow_write("/tmp/work");
        assert_eq!(config.allowed_read_paths.len(), 1);
        assert_eq!(config.allowed_write_paths.len(), 1);
    }

    #[test]
    fn deny_network_builder_sets_flag() {
        assert!(SandboxConfig::new().deny_network().network_denied);
        assert!(!SandboxConfig::new().network_denied);
    }

    #[test]
    fn strict_config_denies_network_with_the_allowlist() {
        let policy = SandboxConfig::new()
            .with_seccomp()
            .seccomp_policy()
            .unwrap();
        assert_eq!(policy.network, NetworkPolicy::Deny);
        assert_eq!(policy.seccomp, SeccompMode::Allowlist);
        // Seccomp only: the filesystem half is the local Landlock backend.
        assert_eq!(policy.landlock, LandlockMode::SkipWhenNoPaths);
        assert!(policy.read_paths.is_empty() && policy.write_paths.is_empty());
        assert!(policy.exec_paths.is_empty());
    }

    #[test]
    fn network_only_config_uses_the_network_only_filter() {
        let policy = SandboxConfig::new()
            .deny_network()
            .seccomp_policy()
            .unwrap();
        assert_eq!(policy.network, NetworkPolicy::Deny);
        assert_eq!(policy.seccomp, SeccompMode::NetworkOnly);
    }

    #[test]
    fn tier_parse_maps_known_and_unknown_values() {
        assert_eq!(SandboxTier::parse("none"), SandboxTier::Disabled);
        assert_eq!(SandboxTier::parse(" OFF "), SandboxTier::Disabled);
        assert_eq!(SandboxTier::parse("strict"), SandboxTier::Strict);
        assert_eq!(SandboxTier::parse("Strict"), SandboxTier::Strict);
        assert_eq!(SandboxTier::parse("landlock-seccomp"), SandboxTier::Strict);
        assert_eq!(SandboxTier::parse("landlock"), SandboxTier::WorkspaceWrite);
        // Empty / unknown input fails closed to the shared strict default.
        assert_eq!(SandboxTier::parse(""), SandboxTier::Strict);
        assert_eq!(
            SandboxTier::parse("workspace-write"),
            SandboxTier::WorkspaceWrite
        );
        assert_eq!(SandboxTier::parse("garbage"), SandboxTier::Strict);
        assert_eq!(SandboxTier::default(), SandboxTier::Strict);
    }

    #[cfg(target_os = "linux")]
    fn run_confined(script: &str, cfg: SandboxConfig) -> std::process::ExitStatus {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cfg.confine(&mut cmd).expect("confine command");
        cmd.status().expect("spawn")
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_landlock_denies_outside_writes_symlink_escape_and_child_escape() {
        if !rightkit_sandbox::is_landlock_available() {
            return;
        }
        let base = std::env::temp_dir().join(format!("cr-ll-{}", std::process::id()));
        let ws = base.join("ws");
        let outside = base.join("outside");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ws.join("link")).unwrap();
        let cfg = || SandboxConfig::new().allow_read("/").allow_write(&ws);
        assert!(
            run_confined(&format!("echo x > {}/a", ws.display()), cfg()).success(),
            "workspace write must work"
        );
        assert!(
            !run_confined(&format!("echo x > {}/b", outside.display()), cfg()).success(),
            "write outside workspace must be denied"
        );
        assert!(
            !run_confined(&format!("echo x > {}/link/c", ws.display()), cfg()).success(),
            "symlink escape must be denied"
        );
        assert!(
            !run_confined(
                &format!("/bin/sh -c 'echo x > {}/d'", outside.display()),
                cfg()
            )
            .success(),
            "child process must inherit confinement"
        );
        for name in ["b", "c", "d"] {
            assert!(!outside.join(name).exists(), "{name} escaped the sandbox");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_network_denied_blocks_inet_without_strict_tier() {
        let has_python = std::process::Command::new("python3")
            .arg("-V")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !has_python {
            return;
        }
        let probe = "python3 -c 'import socket,sys\ntry:\n socket.socket(socket.AF_INET)\nexcept OSError:\n sys.exit(0)\nsys.exit(1)'";
        assert!(
            run_confined(probe, SandboxConfig::new().deny_network()).success(),
            "AF_INET socket must be denied"
        );
        let unix_ok = "python3 -c 'import socket; socket.socket(socket.AF_UNIX)'";
        assert!(
            run_confined(unix_ok, SandboxConfig::new().deny_network()).success(),
            "AF_UNIX must stay allowed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn strict_seccomp_rejects_a_local_tcp_connect_with_eperm() {
        if !rightkit_sandbox::is_landlock_available() {
            return;
        }
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind local seccomp egress probe");
        let address = listener.local_addr().expect("probe address");
        std::net::TcpStream::connect(address)
            .expect("unconfined local seccomp egress probe must connect");
        // Confine a child (never this test process: seccomp is irreversible).
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--ignored",
            "--exact",
            "sandbox::implementation::config_tests::strict_seccomp_worker",
        ])
        .env("RIGHTKIT_SECCOMP_WORKER", "1")
        .env("RIGHTKIT_SECCOMP_EGRESS_PROBE", address.to_string());
        SandboxConfig::new()
            .allow_read("/")
            .with_seccomp()
            .deny_network()
            .confine(&mut cmd)
            .expect("confine worker");
        let output = cmd.output().expect("spawn isolated seccomp worker");
        assert!(
            output.status.success(),
            "seccomp worker failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn strict_seccomp_worker() {
        assert_eq!(
            std::env::var("RIGHTKIT_SECCOMP_WORKER").as_deref(),
            Ok("1")
        );
        let address = std::env::var("RIGHTKIT_SECCOMP_EGRESS_PROBE")
            .expect("parent must provide local seccomp egress probe");
        let error = std::net::TcpStream::connect(address)
            .expect_err("seccomp-confined child must not connect to local TCP listener");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EPERM),
            "seccomp must reject the socket syscall with EPERM: {error}"
        );
    }
}
