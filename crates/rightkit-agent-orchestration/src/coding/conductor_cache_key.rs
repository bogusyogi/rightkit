//! Cross-worker stable-prefix cache key for the conductor (ROADMAP P1).
//!
//! Thin re-export shim over `host_cache` so sibling workers spawned
//! under one conductor run share the same stable-prefix cache key -- the
//! structural guarantee that lets them cache-read against each other's
//! prefix instead of independently cache-breaking. The conductor computes
//! the key ONCE per run and threads it onto every
//! `WorkerTask`(`crate::coding::conductor::WorkerTask`) before dispatch.
//!
//! The historical implementation lived in this file as a hand-written
//! MIRROR of `rightkit_agent_engine_core::cache::{fnv1a, ConductorStableKey, parse_iso_day}`,
//! pinned by parity tests (`fnv1a_matches_engine_vector`,
//! `format_matches_engine_namespace`, `day_extractor_matches_engine_parse_iso_day`).
//! Those mirrors + parity tests are gone now that `host_cache` is the
//! single source of truth shared between `rightkit_agent_engine_core` and
//! `crate`. If you change the format here, change it
//! in `host_cache` -- both crates already import from there.
//!
//! # Handoff brief exclusion
//!
//! The handoff brief (`crate::coding::handoff_brief::HandoffBrief`) is the
//! per-worker CONTEXT tier. It is deliberately excluded from the stable
//! key -- only the conductor identity (a stable string like
//! `host_conductor-v1`) and the UTC day enter the hash. Varying
//! the brief while keeping identity + day constant produces the SAME
//! key (sibling workers still cache-read against each other).

pub use rightkit_cache::prefix::{
    parse_iso_day as conductor_day_from_iso, ConductorStableKey, StableCacheKey,
    CONDUCTOR_CACHE_KEY_PREFIX,
};

/// Canonical conductor cache key string for `identity` + `day`.
pub fn conductor_stable_cache_key(identity: &str, day: &str) -> String {
    ConductorStableKey::new(identity, day).compute()
}
