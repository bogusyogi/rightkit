//! Direct inference authority primitives.

mod attestation;
mod catalog;
mod direct;
mod resolver;
mod scheduler;
mod types;

pub use attestation::{selection_fingerprint, SelectionFingerprintInput};
pub use catalog::{catalog_entry_hash, discover_direct_models, stable_catalog_models};
pub use direct::{
    normalize_base_url, resolved_direct_addresses, validate_direct_connection,
    validate_direct_request_target,
};
pub use resolver::{
    bind_inference_route, ensure_inference_selection_qualified, resume_binding_matches,
};
pub use scheduler::InferenceScheduler;
pub use types::*;
