//! Orthic-only peer admission for the shared speech service.
//!
//! Decision (Adrian, relayed by the Dell lead chat, 2026-10-31): the service accepts "Only orthic apps for now".
//! `rightkit-daemon`'s `Listener` proves only that the peer is the current user; every accepted connection is then
//! checked here with `rightkit_daemon::verify_peer` (rightkit#1319) before one request byte is read
//! (`transport::listen`). The platform checks (macOS `SecCodeCheckValidity` from the peer's audit token, Windows
//! `WinVerifyTrust` and the leaf signer's common name, every other platform refused) are that crate's; this module
//! only names the identities.
//!
//! Identities (never guessed; read from the repo):
//! - macOS Developer ID team `6KLGD3LLKF` (`docs/RIGHT_RELEASE_BUILD_UPLOAD.md:51`, `docs/SIGNING.md:19`,
//!   `/Volumes/D/claude/docs/MAC_DEVELOPER_ID_SIGNING.md:11`; every Mach-O in the release receipt carries it,
//!   `docs/results/2026-11-01-overnight/levers-data/mac-build/sign-receipt.txt`).
//! - Windows Authenticode signer `CN=Damned Ventures LLC`, Azure Artifact Signing (`docs/WINDOWS-SIGNING.md:5`; the
//!   release `install` kind signs the exe and its sidecars, `right-release.config.mjs:138,146`).
//!
//! Every build enforces; there is no environment switch.

use rightkit_daemon::{verify_peer, PeerIdentity, PeerPolicy, SigningIdentity};

/// Orthic Labs Apple Developer team identifier (`kSecCodeInfoTeamIdentifier` / leaf `subject.OU`).
const ORTHIC_MAC_TEAM_ID: &str = "6KLGD3LLKF";

/// Common name of the Orthic Authenticode signing certificate (Azure Artifact Signing).
const ORTHIC_WINDOWS_SIGNER_CN: &str = "Damned Ventures LLC";

/// Only Orthic-signed peers.
fn policy() -> PeerPolicy {
    PeerPolicy::signed_by(SigningIdentity::MacTeamId(ORTHIC_MAC_TEAM_ID.into())).or_signed_by(
        SigningIdentity::WindowsCommonName(ORTHIC_WINDOWS_SIGNER_CN.into()),
    )
}

/// `true` admits the connection; `false` means the caller must close it without reading. Fails closed: anything
/// short of a proven Orthic signature is refused. A refusal is logged with the peer pid and a content-free reason.
pub(super) fn admit(peer: &PeerIdentity) -> bool {
    match verify_peer(peer, &policy()) {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(event = "service_peer_untrusted", pid = ?peer.pid, %error);
            false
        }
    }
}
