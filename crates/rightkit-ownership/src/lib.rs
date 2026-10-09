//! RightKit's ownership scanner, verdict and admission: the single Rust implementation.
//!
//! This crate replaces the Node scanner in `packages/release` (`ownership-scan-worker.mjs`
//! and the modules it imports). The RustRight broker links it as a library.
//!
//! - [`worker`]: [`Scanner`], the in-process scan worker (inputs probe and protected
//!   scan), and [`NativeScanRunner`], which adapts it to [`ScanRunner`].
//! - [`scan`], [`deps`], [`lockfiles`], [`source`], [`inventory`], [`committed`]: the
//!   repository inventory, direct and transitive dependency findings, source findings and
//!   exception evaluation. Git is read through the `git` executable.
//! - [`evidence`]: sealed scan evidence over exact committed source.
//! - [`approval`], [`keylog`], [`keyauth`], [`trust`]: canonical identity (WHATWG),
//!   trust-registry validation, signed approval records, the key log, the shared key
//!   authority and the protected trust loader. Signatures use `p256` and `rsa`.
//! - [`verdict`]: report validation, `ownershipVerdict` and the six-binding cache key.
//! - [`admission`]: [`OwnershipAdmission`] with report and enforce modes, the verdict
//!   cache, single-flight scans and `assertAdmitted`. The cache key carries the UTC day.
//! - [`limits`]: global bound and separate per-repository probe and scan permits.
//! - [`runner`]: the bounded subprocess runner for an external scan worker.
//!
//! The broker trait that consumes an admission lives in the broker, not here.

pub mod admission;
pub mod approval;
pub mod clock;
pub mod committed;
pub mod deps;
pub mod error;
pub mod evidence;
pub mod gitio;
pub mod inventory;
pub mod json;
pub mod keyauth;
pub mod keylog;
pub mod limits;
pub mod lockfiles;
pub mod paths;
pub mod runner;
pub mod scan;
pub mod source;
pub mod trust;
pub mod types;
pub mod verdict;
pub mod worker;

/// The ownership capability catalog the scanner enforces, embedded at build time. A
/// byte-identical copy of `packages/release/rightkit-ownership.json` (a test fails on
/// drift), so the crate works from crates.io without the source tree.
const CATALOG: &str = include_str!("../catalog/ownership-catalog.json");

/// The embedded scan catalog as JSON text.
pub fn catalog() -> &'static str {
    CATALOG
}

/// The embedded scan catalog, parsed once.
///
/// # Panics
/// Never in practice: a test asserts the embedded text parses.
pub fn catalog_value() -> &'static serde_json::Value {
    static PARSED: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(CATALOG).expect("embedded ownership catalog parses"))
}

#[cfg(test)]
mod tests;

pub use admission::{
    classify_ownership_report, AdmissionConfig, Classification, Mode, OwnershipAdmission, Refusal,
    Target, Verdict, Violation, DEFAULT_TIMEOUT,
};
pub use approval::{
    canonical_repository_identity, validate_ownership_floor, verify_repository_registration,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::ScanError;
pub use limits::{Kind, WorkerLimits};
pub use runner::{
    default_scanner_root, resolve_lexical, FnScanRunner, ProcessScanRunner, ScanRequest, ScanRunner,
};
pub use scan::{ScanContext, SCANNER_VERSION};
pub use verdict::{codes, is_blocking_finding, ownership_cache_key, ownership_verdict, Decision};
pub use worker::{host_string, scanner_policy_digest, NativeScanRunner, Scanner};

/// Typed failure with a `RK_OWNERSHIP_*` code, as Node's `Object.assign(new Error, {code})`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipError {
    pub code: &'static str,
    pub message: String,
}
