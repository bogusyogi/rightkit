//! Qualification tooling for the repository code audit engine.
//!
//! Scores an installed audit CLI against fixture bundles (`bench`), bridges
//! constructed semantic capsules (`capsule`), validates assurance
//! qualification records (`assurance`) and observes security-control cases
//! (`security_controls`). Hosts wire each `run` entry into their own CLI.
#![allow(clippy::too_many_arguments)]

pub mod assurance;
pub mod bench;
pub mod capsule;
pub mod security_controls;
