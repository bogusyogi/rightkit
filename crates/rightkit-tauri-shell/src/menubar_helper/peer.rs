//! macOS action-channel peer policy. Exact Developer ID identities are verified through
//! rightkit-daemon using connection audit tokens & process-instance checks. Unsigned,
//! development-certificate, other-team & other-identifier peers are refused in every build.

use std::ffi::CStr;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// Why a peer was not admitted. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerCheck {
    /// The kernel gave no audit token for the connection.
    NoToken,
    /// The process behind the token could not be found or inspected.
    Unverifiable,
    /// The process exists and does not satisfy the requirement.
    Unsatisfied,
}

impl PeerCheck {
    pub fn code(self) -> &'static str {
        match self {
            PeerCheck::NoToken => "no_audit_token",
            PeerCheck::Unverifiable => "unverifiable",
            PeerCheck::Unsatisfied => "unverified",
        }
    }
}

use crate::menubar_helper::protocol;
use rightkit_daemon::{verify_peer, HostIdentityReason, PeerIdentity, PeerPolicy, SigningIdentity};

/// The signing identities one sender class may connect with.
pub struct RolePolicy {
    policy: PeerPolicy,
}

impl RolePolicy {
    /// One `MacDeveloperId { team_id, identifier }` per identifier. `None` when the team or an
    /// identifier is malformed (the daemon would refuse every peer at check time; a host
    /// learns at build time instead).
    pub fn new(team_id: &str, identifiers: &[String]) -> Option<Self> {
        if !protocol::valid_team_id(team_id)
            || identifiers.is_empty()
            || !identifiers
                .iter()
                .all(|identifier| protocol::valid_signing_identifier(identifier))
        {
            return None;
        }
        let mut identities = identifiers
            .iter()
            .map(|identifier| SigningIdentity::MacDeveloperId {
                team_id: team_id.to_string(),
                identifier: identifier.clone(),
            });
        let first = identities.next()?;
        let policy = identities.fold(PeerPolicy::signed_by(first), |policy, identity| {
            policy.or_signed_by(identity)
        });
        Some(Self { policy })
    }

    /// Does the process that connected as `peer` carry one of this class's identities?
    pub fn admits(&self, peer: &PeerIdentity) -> Result<(), PeerCheck> {
        if peer.audit_token.is_none() {
            return Err(PeerCheck::NoToken);
        }
        match verify_peer(peer, &self.policy) {
            Ok(_) => Ok(()),
            // The signature check ran and said no (or could not reach an answer; the daemon
            // folds both into `Refused`): not admitted either way.
            Err(error) => Err(match error.reason {
                HostIdentityReason::Refused(_) => PeerCheck::Unsatisfied,
                _ => PeerCheck::Unverifiable,
            }),
        }
    }
}

/// The per-user temporary directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, the directory under
/// `/var/folders` the system creates `0700` for this user), independent of the `TMPDIR`
/// environment, so a process launched by LaunchServices and one launched from a shell agree. It
/// must be owned by this user and closed to group and others, or the call fails closed.
pub fn user_temp_dir() -> Result<PathBuf, String> {
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: `buffer` is writable for its whole length; confstr NUL-terminates within it.
    let written = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if written == 0 || written > buffer.len() {
        return Err("per-user temp dir unavailable".to_string());
    }
    // SAFETY: confstr wrote a NUL-terminated string within `buffer`.
    let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_str()
        .map_err(|_| "per-user temp dir is not UTF-8".to_string())?;
    let path = PathBuf::from(text);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("per-user temp dir unreadable: {error}"))?;
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err("per-user temp dir is not private to this user".to_string());
    }
    Ok(path)
}
