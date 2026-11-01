//! Bounded, shareable runtime resources.
//!
//! This crate is deliberately independent from engine, protocol, and tools.
//! Integration adapters own conversion to product protocol types; this core owns
//! leases, admission, output retention, and durable spill mechanics.

mod governor;
mod output;
mod service;

pub use governor::{
    AdmissionError, PressureLevel, ResourceGovernor, ResourceKind, ResourceLimits, ResourcePermit,
    ResourceProfile,
};
pub use output::{
    spool_path, BoundedOutput, OutputSpool, SpoolRef, HEAD_BYTES, RETAINED_BYTES, TAIL_BYTES,
};
pub use service::{Lease, ResourceSnapshot, ServiceKey, ServiceScope, SharedServiceRegistry};

#[cfg(test)]
mod tests;
