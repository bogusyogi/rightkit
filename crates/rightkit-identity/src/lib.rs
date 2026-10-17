//! `rightkit-identity` - installation identity, device pairing, bearer tokens and build identity.
//!
//! Every time-dependent call takes the clock as an argument; nothing here spawns work.

pub mod bearer;
pub mod build_info;
pub mod installation;
pub mod pairing;
mod util;

pub use bearer::{AuthError, BearerRegistry, RateLimiter};
pub use build_info::BuildIdentity;
pub use installation::{
    host_display_name, machine_binding_hash, ClaimOutcome, InstallationIdentity, InstallationStore,
    Startup, StartupClaim,
};
pub use pairing::{
    confirmation_code, transcript_mac, DeviceGrant, Observation, PairingSession, PairingState,
    PairingTranscript, ReplayStore, ResumeTicket, Role,
};

/// Errors from this crate.
#[derive(Debug)]
pub enum Error {
    /// The operating system random source failed.
    Random,
    /// A filesystem operation failed (message from the underlying error).
    Io(String),
    /// A persisted record did not parse.
    Corrupt(String),
    /// A state-machine transition or a verification was refused.
    Refused(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Random => write!(f, "operating system random source failed"),
            Error::Io(m) => write!(f, "io: {m}"),
            Error::Corrupt(m) => write!(f, "corrupt record: {m}"),
            Error::Refused(m) => write!(f, "refused: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
