//! P6 inventory/platform port (packet P6-inventory-platform).
//!
//! This module ports the pure, self-contained pieces of
//! `src/lib/artifacts/**` and `src/lib/naming/{registry,migrations}.mjs`
//! from the legacy JS tree to Rust. It is intentionally narrow: the wider
//! `src/lib/inventory/**`, `src/lib/platform/**`, `src/lib/distribution/**`,
//! `src/lib/skills/**`, `src/lib/naming/check.mjs`,
//! `src/lib/tasklist-validator/**`, and `src/lib/dispatch-validator/**`
//! trees are NOT covered here (see the P6 packet report) and remain to be
//! ported in follow-up work.

pub mod artifacts;

pub use artifacts::{digest_bytes, safe_artifact_path, ArtifactPathError};
