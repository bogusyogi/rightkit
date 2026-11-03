//! Attach to the resident daemon, or fall back to running in process (rightkit#404).
//!
//! A shell that can work with or without a resident engine (the hub and the notch beside Pulse's
//! engine) asks [`attach_or_fallback`]: when a live host of the service is discovered and answers,
//! the result is [`Attached::Daemon`] with a connected [`Client`]; otherwise the caller's fallback
//! runs in this process and its value is [`Attached::Fallback`]. Nothing is spawned here: starting
//! a resident engine is [`DaemonLifecycle`](crate::DaemonLifecycle)'s job, under the spawn claim.
//!
//! A discovered host is only used when the connection succeeds, and with a
//! [`HostExpectation`] only when the OS says the host is that program. A record that exists but
//! cannot be read is "unknown", and is reported as [`Attached::Unknown`] without running the
//! fallback, so a transient read failure never starts a second engine beside the resident one.

use crate::client::{Client, ClientConfig};
use crate::discovery::{try_adopt_or_spawn, AdoptOrSpawn};
use crate::host_identity::HostExpectation;

/// What [`attach_or_fallback`] did.
#[allow(clippy::large_enum_variant)]
pub enum Attached<T> {
    /// A resident host answered; this client is connected to it.
    Daemon(Client),
    /// No usable resident host: the fallback ran in this process.
    Fallback(T),
    /// Whether a host exists could not be established (`service.json` exists and could not be
    /// read). The fallback did not run; look again.
    Unknown(std::io::Error),
}

/// Connect to the resident daemon described by `config`, or run `fallback`. `expect` pins the host
/// to a program (see [`Client::connect_expecting`]); without it any same-user host is accepted.
pub fn attach_or_fallback<T>(
    config: &ClientConfig,
    expect: Option<&HostExpectation>,
    fallback: impl FnOnce() -> T,
) -> Attached<T> {
    let suite = match config.resolve_suite() {
        Ok(suite) => suite,
        Err(_) => return Attached::Fallback(fallback()),
    };
    match try_adopt_or_spawn(&suite, config.service()) {
        Err(e) => Attached::Unknown(e),
        Ok(AdoptOrSpawn::Spawn) => Attached::Fallback(fallback()),
        Ok(AdoptOrSpawn::Adopt(_)) => {
            let connected = match expect {
                Some(expect) => Client::connect_expecting(config, expect).ok(),
                None => Client::connect_config(config).ok(),
            };
            match connected {
                Some(client) => Attached::Daemon(client),
                None => Attached::Fallback(fallback()),
            }
        }
    }
}
