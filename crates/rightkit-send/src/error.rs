//! Typed errors. `Display` gives a short English diagnostic for logs. A host that
//! shows text to people matches on the variant (or on the serialized `code`) and
//! words it in its own language.

use serde::Serialize;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Error {
    /// The stored certificate and key do not form a usable identity.
    Identity { detail: String },
    /// The listen port is held by another program.
    PortInUse { port: u16 },
    /// The listen port could not be bound for another reason.
    Listen { port: u16, detail: String },
    /// The discovery socket could not be opened; devices that find this one
    /// first still work.
    DiscoveryUnavailable { detail: String },
    /// The device list could not be written to the trust store path.
    TrustStore { detail: String },
    /// A device key was not a 64-character SHA-256 in hex.
    InvalidFingerprint,
    /// A device record was asked for that has no record.
    NotListed,
    /// The default state was set to Allow; only single devices may be allowed.
    AllowNotDefault,
    /// Forgetting a record is refused while that device is on the network.
    DeviceNearby,
    /// The send target is not on the network.
    DeviceNotNearby,
    /// The send target is set to Deny.
    DeviceBlocked,
    /// The device announced no usable address.
    NoUsableAddress,
    /// The device's identity is not a valid fingerprint.
    InvalidIdentity { peer: String },
    /// The device answered over plain HTTP. Sends are refused without TLS.
    PeerPlaintext { peer: String },
    /// The certificate the device presented is not the one its fingerprint names.
    /// Distinct from an unreachable device: the address answered with another key.
    PeerIdentityMismatch { peer: String },
    /// The device could not be connected to.
    PeerUnreachable { peer: String, detail: String },
    /// The connection to the device failed after it was made.
    PeerLost { peer: String, detail: String },
    /// The device answered with a status the protocol does not define here.
    PeerAnswer { peer: String, status: u16 },
    /// The device's answer could not be parsed.
    PeerReply { peer: String },
    /// The device asks for a PIN, which is not supported.
    PeerNeedsPin { peer: String },
    /// The device is busy with another transfer.
    PeerBusy { peer: String },
    /// The device refused the request as too large for an unproven sender.
    PeerTooLarge { peer: String },
    /// The device is rate limiting requests.
    PeerRateLimited { peer: String },
    /// The device refused one file.
    FileRefused { peer: String, file: String },
    /// The device reported that one file arrived damaged.
    FileDamaged { peer: String, file: String },
    /// Sending one file stopped.
    FileStopped { file: String, detail: String },
    /// More entries than the send limit.
    TooManyFiles { max: usize },
    /// Nothing to send after expanding the selection.
    NothingToSend,
    /// A chosen path is neither a file nor a folder.
    NotFileOrFolder { path: String },
    /// A received file's checksum did not match the one the sender named.
    Damaged { file: String },
    /// An internal failure that should not happen (serializing our own message).
    Internal { detail: String },
    /// A chosen path could not be read.
    Unreadable { path: String, detail: String },
    /// The sender stopped responding while a transfer was open.
    SenderStopped,
    /// A request from an unproven sender named too much (receive side).
    Unverified { max_files: usize, max_bytes: u64 },
    /// A received file could not be saved.
    SaveFailed { file: String, detail: String },
    /// A received file was cut off mid-transfer.
    Interrupted { file: String, detail: String },
    /// A received file could not be marked as coming from outside the computer.
    /// An unproven sender's file is then not kept.
    Mark { file: String, detail: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Identity { detail } => write!(f, "identity unusable: {detail}"),
            Error::PortInUse { port } => write!(f, "port {port} is already in use"),
            Error::Listen { port, detail } => write!(f, "cannot listen on port {port}: {detail}"),
            Error::DiscoveryUnavailable { detail } => write!(f, "discovery unavailable: {detail}"),
            Error::TrustStore { detail } => write!(f, "cannot save the device list: {detail}"),
            Error::InvalidFingerprint => write!(f, "fingerprint is not 64 hex characters"),
            Error::NotListed => write!(f, "device is not in the list"),
            Error::AllowNotDefault => write!(f, "the default state cannot be allow"),
            Error::DeviceNearby => write!(f, "device is on the network now"),
            Error::DeviceNotNearby => write!(f, "device is not on the network"),
            Error::DeviceBlocked => write!(f, "device is set to deny"),
            Error::NoUsableAddress => write!(f, "device has no usable address"),
            Error::InvalidIdentity { peer } => write!(f, "{peer} presented no valid identity"),
            Error::PeerPlaintext { peer } => write!(f, "{peer} has TLS turned off"),
            Error::PeerIdentityMismatch { peer } => {
                write!(f, "{peer}: certificate does not match its fingerprint")
            }
            Error::PeerUnreachable { peer, detail } => write!(f, "cannot reach {peer}: {detail}"),
            Error::PeerLost { peer, detail } => write!(f, "lost the connection to {peer}: {detail}"),
            Error::PeerAnswer { peer, status } => write!(f, "{peer} answered {status}"),
            Error::PeerReply { peer } => write!(f, "{peer} sent an answer that cannot be read"),
            Error::PeerNeedsPin { peer } => write!(f, "{peer} requires a PIN"),
            Error::PeerBusy { peer } => write!(f, "{peer} is busy with another transfer"),
            Error::PeerTooLarge { peer } => {
                write!(f, "{peer} refused: the request is over the unproven limit")
            }
            Error::PeerRateLimited { peer } => write!(f, "{peer} is rate limiting requests"),
            Error::FileRefused { peer, file } => write!(f, "{peer} refused {file}"),
            Error::FileDamaged { peer, file } => write!(f, "{peer} reports {file} arrived damaged"),
            Error::FileStopped { file, detail } => write!(f, "sending {file} stopped: {detail}"),
            Error::TooManyFiles { max } => write!(f, "more than {max} files"),
            Error::NothingToSend => write!(f, "nothing to send"),
            Error::NotFileOrFolder { path } => write!(f, "{path} is not a file or folder"),
            Error::Unreadable { path, detail } => write!(f, "cannot read {path}: {detail}"),
            Error::SenderStopped => write!(f, "the sender stopped responding"),
            Error::Unverified { max_files, max_bytes } => write!(
                f,
                "a sender that cannot prove its identity may send at most {max_files} files and {max_bytes} bytes at once"
            ),
            Error::SaveFailed { file, detail } => write!(f, "cannot save {file}: {detail}"),
            Error::Interrupted { file, detail } => write!(f, "transfer of {file} interrupted: {detail}"),
            Error::Mark { file, detail } => write!(f, "cannot mark {file} as downloaded: {detail}"),
            Error::Damaged { file } => write!(f, "{file} arrived damaged"),
            Error::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

/// A failed connection to a peer. A certificate that is not the one the
/// fingerprint names is `PeerIdentityMismatch`; anything else is `otherwise`.
pub(crate) fn peer_io(
    peer: &str,
    error: &std::io::Error,
    otherwise: impl FnOnce(String, String) -> Error,
) -> Error {
    if crate::net::is_identity_mismatch(error) {
        Error::PeerIdentityMismatch {
            peer: peer.to_string(),
        }
    } else {
        otherwise(peer.to_string(), error.to_string())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_stable_codes() {
        let error = Error::PeerIdentityMismatch {
            peer: "Phone".into(),
        };
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "peer_identity_mismatch");
        assert_eq!(value["peer"], "Phone");
        let unit = serde_json::to_value(&Error::InvalidFingerprint).unwrap();
        assert_eq!(unit["code"], "invalid_fingerprint");
        assert_eq!(
            Error::PortInUse { port: 53317 }.to_string(),
            "port 53317 is already in use"
        );
    }

    #[test]
    fn a_mismatch_is_distinct_from_other_connection_failures() {
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let classified = peer_io("Phone", &refused, |peer, detail| Error::PeerUnreachable {
            peer,
            detail,
        });
        assert!(matches!(classified, Error::PeerUnreachable { .. }));
    }
}
