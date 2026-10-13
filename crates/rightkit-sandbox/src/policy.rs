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
    pub fn named_capability(mut self, name: impl Into<String>) -> Self {
        self.named_capabilities.push(name.into());
        self
    }

    pub(crate) fn validate(&self) -> Result<(), SandboxError> {
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
