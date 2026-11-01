//! Cycle-free HTTP workflow API contracts.
#![forbid(unsafe_code)]

pub mod error;
pub mod legacy;
pub mod openapi;
pub mod route_types;

pub use error::WorkflowApiError;
pub use legacy::{LegacyTriggerDelivery, LegacyWorkflowAdmission};
pub use openapi::openapi_v1_schema;
pub use route_types::*;
