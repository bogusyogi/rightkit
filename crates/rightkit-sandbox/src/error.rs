use crate::resources::ResourceDenied;

/// Typed failure from [`crate::Sandbox`]. Confinement never silently degrades:
/// an unsupported platform/feature is `Unsupported`, not an unsandboxed run.
#[derive(Debug)]
pub enum SandboxError {
    /// Reserve-before-spawn refused; no process was created.
    ResourceDenied(ResourceDenied),
    /// Policy or command is malformed (relative path, NUL byte, ...).
    InvalidPolicy(String),
    /// Requested confinement is not available here (fail closed).
    Unsupported(String),
    /// OS-level spawn/setup failure.
    Io(std::io::Error),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResourceDenied(d) => write!(f, "{d}"),
            Self::InvalidPolicy(m) => write!(f, "invalid sandbox policy: {m}"),
            Self::Unsupported(m) => write!(f, "sandbox unsupported: {m}"),
            Self::Io(e) => write!(f, "sandbox spawn failed: {e}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<std::io::Error> for SandboxError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<ResourceDenied> for SandboxError {
    fn from(e: ResourceDenied) -> Self {
        Self::ResourceDenied(e)
    }
}
