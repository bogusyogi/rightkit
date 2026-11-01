//! The Pro gate port. This crate decides *where* a paid capability starts; the engine binary
//! decides *whether* the licence allows it (the host entitlement module, which verifies
//! the signed licence with rightkit-license). Nothing here reads a licence, a flag or an
//! environment variable, so a caller cannot hand the engine a verdict.
//!
//! Gated capability: [`CLOUD_PROVIDERS`], any provider that needs an API key. Local providers
//! never call the gate.

use std::sync::OnceLock;

use crate::{GenError, GenResult};

/// Feature label for hosted (keyed) generation providers.
pub const CLOUD_PROVIDERS: &str = "cloud providers";

/// Installed once by the engine at startup.
pub trait Gate: Send + Sync {
    /// `Ok` when the licence includes `feature`; a typed refusal otherwise.
    fn require(&self, feature: &str) -> GenResult<()>;
}

static GATE: OnceLock<Box<dyn Gate>> = OnceLock::new();

/// Install the process gate. A second call is ignored.
pub fn install(gate: Box<dyn Gate>) {
    let _ = GATE.set(gate);
}

/// Refuse unless the installed gate allows `feature`. With no gate installed this refuses,
/// except in this crate's own unit tests, which exercise providers through mocks.
pub fn require(feature: &str) -> GenResult<()> {
    match GATE.get() {
        Some(gate) => gate.require(feature),
        None if cfg!(test) => Ok(()),
        None => Err(GenError::new(
            "license_unverified",
            format!("{feature}: no licence check is installed in this process"),
        )),
    }
}
