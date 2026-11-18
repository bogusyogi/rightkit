//! Discovery and held-file authentication for an installed Windows CLI.

use std::{fmt, io, path::Path};

use rightkit_daemon::{
    discover_installed_cli_candidates, InstalledCliCandidateState, InstalledCliProbeFailure,
    SignatureError, WindowsPublisherPolicy,
};
use rightkit_fs::{state::PinnedExecutable, FsError};

use crate::windows_identity::{
    verify_held_windows_cli_identity_with_dependency_policy, WindowsCliRole,
};

/// Exact installed CLI policy from the caller's trusted, shipped app catalogue.
/// Populate every field from that catalogue, independently of request inputs,
/// environment, registry values and the candidate executable.
pub struct WindowsInstalledCliPolicy<'a> {
    /// Exact Windows uninstall display name.
    pub display_name: &'a str,
    /// Expected helper path relative to the installation directory.
    pub relative_executable: &'a Path,
    /// Approved Authenticode publisher and certificate chain.
    pub publisher: WindowsPublisherPolicy<'a>,
    /// Exact product identifier in the signed CLI identity record.
    pub product_id: &'a str,
    /// Exact role in the signed CLI identity record.
    pub role: WindowsCliRole,
    /// Exact audited embedded manifest bytes, excluding private DLL redirection.
    pub approved_manifest: &'a [u8],
}

/// A verified helper with its executable file and ancestors pinned until drop.
/// This lease authenticates the held file. A launcher must separately check external
/// redirection and bind its suspended process to this file before resuming.
#[must_use = "dropping the verification lease releases the executable and its ancestors"]
pub struct VerifiedWindowsInstalledCli {
    _pinned: PinnedExecutable,
}

/// A refusal while finding or authenticating an installed Windows CLI.
#[derive(Debug)]
#[non_exhaustive]
pub enum WindowsInstalledCliError {
    /// Discovery rejected the policy or could not run.
    Discovery(io::Error),
    /// A discovery probe could not finish.
    ProbeFailed(InstalledCliProbeFailure),
    /// Discovery observed more than one matching installation entry.
    Ambiguous,
    /// A matching installation did not have a usable executable observation.
    Incomplete,
    /// The executable or its ancestors could not be pinned safely.
    Pin(FsError),
    /// The held file failed publisher, identity or signed PE policy verification.
    Identity(SignatureError),
}

impl fmt::Display for WindowsInstalledCliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(error) => write!(f, "installed CLI discovery failed: {error}"),
            Self::ProbeFailed(failure) => {
                write!(f, "installed CLI probe failed: {}", failure.error)
            }
            Self::Ambiguous => f.write_str("installed CLI discovery found multiple entries"),
            Self::Incomplete => f.write_str("installed CLI entry is incomplete"),
            Self::Pin(error) => write!(f, "could not pin installed CLI: {error}"),
            Self::Identity(error) => write!(f, "installed CLI verification failed: {error}"),
        }
    }
}

impl std::error::Error for WindowsInstalledCliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Discovery(error) => Some(error),
            Self::ProbeFailed(failure) => Some(&failure.error),
            Self::Pin(error) => Some(error),
            Self::Identity(error) => Some(error),
            Self::Ambiguous | Self::Incomplete => None,
        }
    }
}

/// Authenticate the sole complete discovered installation against trusted policy.
/// `Ok(None)` means the complete discovery probe found zero matching entries.
/// An error is a refusal and must not trigger fallback to another route.
pub fn verify_installed_windows_cli(
    policy: &WindowsInstalledCliPolicy<'_>,
) -> Result<Option<VerifiedWindowsInstalledCli>, WindowsInstalledCliError> {
    use WindowsInstalledCliError as Error;

    let discovery =
        discover_installed_cli_candidates(policy.display_name, policy.relative_executable)
            .map_err(Error::Discovery)?;
    if let Some(failure) = discovery.failures.into_iter().next() {
        return Err(Error::ProbeFailed(failure));
    }
    let mut candidates = discovery.candidates.into_iter();
    let Some(candidate) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        return Err(Error::Ambiguous);
    }
    match candidate.state {
        InstalledCliCandidateState::Present => {}
        InstalledCliCandidateState::ProbeFailed(failure) => {
            return Err(Error::ProbeFailed(failure))
        }
        _ => return Err(Error::Incomplete),
    }

    let pinned = PinnedExecutable::open(&candidate.executable).map_err(Error::Pin)?;
    verify_held_windows_cli_identity_with_dependency_policy(
        pinned.file(),
        &policy.publisher,
        policy.product_id,
        policy.role,
        policy.approved_manifest,
    )
    .map_err(Error::Identity)?;
    Ok(Some(VerifiedWindowsInstalledCli { _pinned: pinned }))
}
