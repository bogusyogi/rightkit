//! Re-exports of rightkit-cache::prefix for legacy crate::cache::* paths.
//!
//! The cache primitives (FNV-1a, StableCacheKey, ConductorStableKey,
//! parse_iso_day) are owned by the published `rightkit-cache` crate. This shim
//! preserves the historical crate::cache::* import paths inside the engine crate.

pub use rightkit_cache::prefix::{
    fnv1a_64, parse_iso_day, system_prompt_only_cache_key, CacheStatus, CacheTracker,
    ConductorStableKey, StableCacheKey, CONDUCTOR_CACHE_KEY_PREFIX,
};

/// The non-standard FNV-1a 64 variant behind every cache-slot name, routing key and signature the
/// engine already persisted or sent to providers: the standard FNV-1a 64 loop with an offset basis
/// of 1469598103934665603 (the standard basis with its last digit missing), so it matches no other
/// FNV-1a implementation. Its values must not change. `rightkit_cache::prefix::fnv1a` is the same
/// function but deprecated and has no undeprecated twin, so the loop is spelled out here rather than
/// calling it under a lint waiver.
pub fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 1469598103934665603;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

/// Production-side helper: the UTC day component of rightkit_agent_engine_protocol::now_iso.
/// Stays in the engine crate because it depends on rightkit_agent_engine_protocol.
pub(crate) fn today_day() -> String {
    parse_iso_day(&rightkit_agent_engine_protocol::now_iso()).to_string()
}
