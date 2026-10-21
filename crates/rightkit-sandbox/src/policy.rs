use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use crate::SandboxError;

/// Whether the child may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// Egress allowed.
    Allow,
    /// No outbound network (local unix sockets stay on macOS; seccomp omits sockets on Linux).
    #[default]
    Deny,
}

/// How much of the filesystem a confined child is walled off from.
///
/// Implemented by the macOS Seatbelt backend. Linux and Windows refuse
/// [`ConfinementMode::WriteOnly`] (fail closed) rather than quietly run with a different
/// boundary than the one requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfinementMode {
    /// Deny-default: reads, writes and execution are limited to the paths the policy lists
    /// (plus the always-readable system locations).
    #[default]
    Strict,
    /// Reads and execution stay unrestricted so ordinary toolchains run; writes are denied
    /// everywhere except `write_paths`, the per-user temporary locations and a few `/dev`
    /// nodes. Network follows [`SandboxPolicy::network()`]. macOS only.
    WriteOnly,
}

/// Linux seccomp filter shape, used when the network is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeccompMode {
    /// Default-deny allowlist of the syscalls a compiler or test runner needs. Network
    /// syscalls are absent, which is what denies egress.
    #[default]
    Allowlist,
    /// Default-allow filter that denies only network creation: `socket` and `socketpair`
    /// for every domain except `AF_UNIX`, and `io_uring_*` (which could reach the network
    /// without a socket syscall). For tiers that confine the filesystem and network but must
    /// not break arbitrary syscalls.
    NetworkOnly,
}

/// Whether the Linux backend applies Landlock when the policy lists no paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LandlockMode {
    /// Always apply Landlock. With no paths listed the child sees only the system locations.
    #[default]
    Always,
    /// Apply no filesystem confinement when `read_paths`, `write_paths` and `exec_paths` are
    /// all empty (a network-only tier). Any listed path turns Landlock on, and then it must
    /// be available or the launch fails. This is the one documented way to run on Linux
    /// without a Landlock ruleset, so it is opt-in.
    SkipWhenNoPaths,
}

/// Windows loopback exemption for a network-enabled AppContainer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopbackPolicy {
    /// Windows blocks loopback for every AppContainer; leave it blocked.
    #[default]
    Blocked,
    /// Add the container to the machine's loopback-exempt list for as long as a child holds
    /// the lease (process-wide reference count, removed with the last holder). Only applies
    /// with [`NetworkPolicy::Allow`]. Writing the list needs an elevated process; without
    /// elevation the container simply stays loopback-blocked (never wider). Ignored off
    /// Windows.
    Exempt,
}

/// One `deny_read` entry: an exact path (and everything below it) or a glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReadEntry {
    /// A file or directory. A directory covers everything below it.
    Path(PathBuf),
    /// An absolute glob: `*` matches within one path segment, `**` across segments, `?` one
    /// non-separator character; everything else is literal.
    Glob(String),
}

impl DenyReadEntry {
    pub(crate) fn validate(&self) -> Result<(), SandboxError> {
        match self {
            Self::Path(p) => crate::deny_read::validate_deny_path(p),
            Self::Glob(g) => crate::deny_read::validate_glob(g),
        }
    }
}

/// Environment handed to the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvPolicy {
    /// `false` (default) clears the environment, then re-adds `keep` and `set`.
    pub inherit: bool,
    /// Parent variables copied through when scrubbing.
    pub keep: Vec<String>,
    /// Explicit variables set in the child.
    pub set: BTreeMap<String, String>,
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self {
            inherit: false,
            keep: ["PATH", "LANG", "LC_ALL", "TERM"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            set: BTreeMap::new(),
        }
    }
}

/// What the confined child may touch. Paths must be absolute. Reads of core
/// system locations (`/usr`, `/bin`, `/System`, ...) are always allowed so
/// interpreters can start; everything else is denied unless listed.
///
/// Build it with [`SandboxPolicy::new`] and the builder methods, or with struct-update syntax
/// (`SandboxPolicy { read_paths, ..Default::default() }`), which keeps compiling when options
/// are added. A literal that names every field is not source-compatible with a release that
/// adds one; no Rust struct can be both.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    pub read_paths: Vec<PathBuf>,
    /// Read + write + create/remove beneath these paths.
    pub write_paths: Vec<PathBuf>,
    /// Extra locations the child may execute from (system dirs are implicit).
    pub exec_paths: Vec<PathBuf>,
    pub network: NetworkPolicy,
    pub env: EnvPolicy,
    /// Windows named AppContainer capabilities (for example `registryRead`), derived with
    /// `DeriveCapabilitySidsFromName`. Ignored on other platforms. Never grants network:
    /// `internetClient` is controlled only by `network`.
    pub named_capabilities: Vec<String>,
    /// Filesystem boundary shape. Default [`ConfinementMode::Strict`].
    pub mode: ConfinementMode,
    /// Allow executing any program (macOS `(allow process*)`, Linux Landlock `EXECUTE` on `/`).
    /// For vendor CLIs that exec helpers from arbitrary install roots (nvm, pyenv, Homebrew).
    /// It grants execution only: reading the image still needs `read_paths`/`exec_paths`. No
    /// effect on Windows, where execution follows the ACLs the policy grants.
    pub exec_any: bool,
    /// Paths and globs the child can never read. See [`SandboxPolicy::deny_read()`].
    pub deny_read: Vec<DenyReadEntry>,
    /// Linux seccomp filter shape when the network is denied.
    pub seccomp: SeccompMode,
    /// Linux: apply Landlock even when no path is listed (default) or only when one is.
    pub landlock: LandlockMode,
    /// Windows loopback exemption for network-enabled containers.
    pub loopback: LoopbackPolicy,
}

impl SandboxPolicy {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn allow_read(mut self, p: impl Into<PathBuf>) -> Self {
        self.read_paths.push(p.into());
        self
    }
    pub fn allow_write(mut self, p: impl Into<PathBuf>) -> Self {
        self.write_paths.push(p.into());
        self
    }
    pub fn allow_exec(mut self, p: impl Into<PathBuf>) -> Self {
        self.exec_paths.push(p.into());
        self
    }
    pub fn network(mut self, n: NetworkPolicy) -> Self {
        self.network = n;
        self
    }
    pub fn env(mut self, e: EnvPolicy) -> Self {
        self.env = e;
        self
    }
    /// Adds a named Windows AppContainer capability. Fixture/QA use only:
    /// the name must be one of the internal allowlist
    /// (`lpacChromeInstallFiles`, `registryRead`); any other name is rejected
    /// at validation so grants cannot bypass the network/path policy.
    pub fn named_capability(mut self, name: impl Into<String>) -> Self {
        self.named_capabilities.push(name.into());
        self
    }

    /// Sets the filesystem boundary shape.
    pub fn mode(mut self, mode: ConfinementMode) -> Self {
        self.mode = mode;
        self
    }
    /// Allows executing any program. See [`SandboxPolicy::exec_any()`].
    pub fn exec_any(mut self, allow: bool) -> Self {
        self.exec_any = allow;
        self
    }
    /// Denies reading (and writing) `path` and everything below it, even inside a granted
    /// root. macOS: a `(deny file-read* file-write*)` rule after every allow. Windows: **not
    /// supported yet** (issue #887; a lowbox token ignores DENY ACEs): any `deny_read` entry
    /// makes the spawn fail with `SandboxError::Unsupported`, never a silent leak. **Linux: not
    /// enforced.** Landlock only adds allowances and cannot carve a hole out of a granted
    /// root, so the entry is omitted there; check [`crate::deny_read_enforced`] and grant
    /// narrower roots instead when it is `false`. Use [`crate::secret_paths_within`] to
    /// find the protected locations beneath a root.
    pub fn deny_read(mut self, path: impl Into<PathBuf>) -> Self {
        self.deny_read.push(DenyReadEntry::Path(path.into()));
        self
    }
    /// As [`SandboxPolicy::deny_read()`] for an absolute glob. macOS matches it by regex at
    /// access time. Windows: not supported yet (issue #887), the spawn is refused with
    /// `SandboxError::Unsupported` before the glob is expanded. Linux omits it.
    pub fn deny_read_glob(mut self, pattern: impl Into<String>) -> Self {
        self.deny_read.push(DenyReadEntry::Glob(pattern.into()));
        self
    }
    /// Sets the Linux seccomp filter shape.
    pub fn seccomp(mut self, mode: SeccompMode) -> Self {
        self.seccomp = mode;
        self
    }
    /// Sets whether Linux applies Landlock when no path is listed.
    pub fn landlock(mut self, mode: LandlockMode) -> Self {
        self.landlock = mode;
        self
    }
    /// Sets the Windows loopback exemption.
    pub fn loopback(mut self, loopback: LoopbackPolicy) -> Self {
        self.loopback = loopback;
        self
    }

    pub(crate) fn validate(&self) -> Result<(), SandboxError> {
        validate_named_capabilities(&self.named_capabilities)?;
        for entry in &self.deny_read {
            entry.validate()?;
        }
        for p in self
            .read_paths
            .iter()
            .chain(&self.write_paths)
            .chain(&self.exec_paths)
        {
            let s = p.to_string_lossy();
            if !p.is_absolute() || s.contains('\0') {
                return Err(SandboxError::InvalidPolicy(format!(
                    "path must be absolute and NUL-free: {s}"
                )));
            }
        }
        Ok(())
    }
}

/// Named capabilities a policy may request. Everything else (internetClient,
/// documentsLibrary, ...) must go through the grant model, not this list.
pub(crate) const ALLOWED_NAMED_CAPABILITIES: &[&str] = &["lpacChromeInstallFiles", "registryRead"];

pub(crate) fn validate_named_capabilities(names: &[String]) -> Result<(), SandboxError> {
    for n in names {
        if n.contains('\0') || !ALLOWED_NAMED_CAPABILITIES.contains(&n.as_str()) {
            return Err(SandboxError::InvalidPolicy(format!(
                "named capability not allowed: {}",
                n.escape_debug()
            )));
        }
    }
    Ok(())
}

/// Program to run. No shell interpretation: `program` + `args` go to exec.
#[derive(Debug, Clone)]
pub struct SandboxCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl SandboxCommand {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: vec![],
            cwd: None,
            env: vec![],
        }
    }
    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }
    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }
    pub fn cwd(mut self, p: impl Into<PathBuf>) -> Self {
        self.cwd = Some(p.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
}

#[cfg(test)]
mod named_capability_tests {
    use super::*;

    fn check(name: &str) -> Result<(), SandboxError> {
        SandboxPolicy::new().named_capability(name).validate()
    }

    #[test]
    fn rejects_non_allowlisted_names() {
        for n in [
            "internetClient",
            "internetClientServer",
            "privateNetworkClientServer",
            "documentsLibrary",
            "removableStorage",
            "registryRead\0x",
            "",
        ] {
            assert!(
                matches!(check(n), Err(SandboxError::InvalidPolicy(_))),
                "{n:?}"
            );
        }
    }

    #[test]
    fn accepts_allowlisted_names() {
        assert!(check("lpacChromeInstallFiles").is_ok());
        assert!(check("registryRead").is_ok());
    }
}

#[cfg(test)]
mod knob_tests {
    use super::*;

    #[test]
    fn struct_update_literal_keeps_compiling_with_every_knob_defaulted() {
        let policy = SandboxPolicy {
            read_paths: vec![PathBuf::from("/r")],
            ..Default::default()
        };
        assert_eq!(policy.read_paths.len(), 1);
        assert_eq!(policy.mode, ConfinementMode::Strict);
        assert!(policy.deny_read.is_empty());
    }

    #[test]
    fn every_new_knob_defaults_to_the_behaviour_before_it_existed() {
        let policy = SandboxPolicy::new();
        assert_eq!(policy.mode, ConfinementMode::Strict);
        assert!(!policy.exec_any);
        assert!(policy.deny_read.is_empty());
        assert_eq!(policy.seccomp, SeccompMode::Allowlist);
        assert_eq!(policy.landlock, LandlockMode::Always);
        assert_eq!(policy.loopback, LoopbackPolicy::Blocked);
    }

    #[test]
    fn builders_set_their_knob_and_nothing_else() {
        let policy = SandboxPolicy::new()
            .mode(ConfinementMode::WriteOnly)
            .exec_any(true)
            .seccomp(SeccompMode::NetworkOnly)
            .landlock(LandlockMode::SkipWhenNoPaths)
            .loopback(LoopbackPolicy::Exempt)
            .deny_read("/x/secret")
            .deny_read_glob("/x/*.pem");
        assert_eq!(policy.mode, ConfinementMode::WriteOnly);
        assert!(policy.exec_any);
        assert_eq!(policy.seccomp, SeccompMode::NetworkOnly);
        assert_eq!(policy.landlock, LandlockMode::SkipWhenNoPaths);
        assert_eq!(policy.loopback, LoopbackPolicy::Exempt);
        assert_eq!(
            policy.deny_read,
            vec![
                DenyReadEntry::Path(PathBuf::from("/x/secret")),
                DenyReadEntry::Glob("/x/*.pem".into()),
            ]
        );
        // Granting nothing else: the path lists and the network choice are untouched.
        assert!(policy.read_paths.is_empty() && policy.write_paths.is_empty());
        assert!(policy.exec_paths.is_empty());
        assert_eq!(policy.network, NetworkPolicy::Deny);
    }
}
