//! Re-exports of rightkit-cache::prefix for legacy crate::cache::* paths.
//!
//! The cache primitives (FNV-1a, StableCacheKey, ConductorStableKey,
//! parse_iso_day) are owned by the published `rightkit-cache` crate. This shim
//! preserves the historical crate::cache::* import paths inside the engine crate.

pub use rightkit_cache::prefix::{
    parse_iso_day, system_prompt_only_cache_key, CacheStatus, CacheTracker, ConductorStableKey,
    StableCacheKey, CONDUCTOR_CACHE_KEY_PREFIX,
};

/// The non-standard FNV-1a 64 variant behind every cache-slot name, routing key and signature the
/// engine already persisted or sent to providers. Its values must not change, so the one
/// deprecated call lives here and the engine's callers use this wrapper.
#[allow(deprecated)] // legacy basis kept on purpose: existing provider slots and persisted keys hit it
pub fn fnv1a(s: &str) -> u64 {
    rightkit_cache::prefix::fnv1a(s)
}

/// Production-side helper: the UTC day component of rightkit_agent_engine_protocol::now_iso.
/// Stays in the engine crate because it depends on rightkit_agent_engine_protocol.
pub(crate) fn today_day() -> String {
    parse_iso_day(&rightkit_agent_engine_protocol::now_iso()).to_string()
}
