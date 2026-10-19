//! Stable-prefix cache keys for provider prompt caches (ported from CodeRight
//! `coderight-cache`). Only the stable tier (identity + UTC day) enters the key;
//! volatile content (per-turn memory, timestamps) must not. Key strings are
//! byte-compatible with the donor so existing provider slots keep hitting.

/// Standard FNV-1a 64 (offset basis 14695981039346656037, prime 1099511628211) over the UTF-8
/// bytes. Interoperable with other FNV-1a 64 implementations; not for security.
pub fn fnv1a_64(s: &str) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// NON-STANDARD FNV-1a 64 variant: its offset basis is 1469598103934665603 (the standard basis with
/// its last digit missing), so it matches no other FNV-1a implementation. Kept unchanged because
/// existing cache-slot names and persisted keys were derived from it. New code uses [`fnv1a_64`].
#[deprecated(
    since = "0.2.3",
    note = "non-standard offset basis; use fnv1a_64 for new keys, keep this only for keys already persisted with it"
)]
pub fn fnv1a(s: &str) -> u64 {
    legacy_fnv1a(s)
}

/// The cache-slot hash behind every key this module builds; byte-compatible with the donor.
fn legacy_fnv1a(s: &str) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// Extract `YYYY-MM-DD` from an ISO 8601 timestamp.
pub fn parse_iso_day(iso: &str) -> &str {
    iso.split('T').next().unwrap_or("")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// First observation of this prefix.
    Fresh,
    /// Prefix unchanged since last observation.
    Hit,
    /// Prefix changed: the provider cache was invalidated.
    Break,
}

/// `cache:prompt:<kind>:<fnv1a>` over `identity\n__day__:<day>`.
fn slot_key(kind: &str, identity: &str, day: &str) -> String {
    format!(
        "cache:prompt:{kind}:{:016x}",
        legacy_fnv1a(&format!("{identity}\n__day__:{day}"))
    )
}

/// Single-session stable tier.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StableCacheKey {
    pub identity: String,
    pub day: String,
}

impl StableCacheKey {
    pub fn new(identity: impl Into<String>, day: impl Into<String>) -> Self {
        Self {
            identity: identity.into(),
            day: day.into(),
        }
    }

    pub fn compute(&self) -> String {
        slot_key("stable", &self.identity, &self.day)
    }
}

/// Stable tier shared by every worker under one conductor run; distinct prefix so slots never collide with a session's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConductorStableKey {
    pub identity: String,
    pub day: String,
}

pub const CONDUCTOR_CACHE_KEY_PREFIX: &str = "cache:prompt:conductor:";

impl ConductorStableKey {
    pub fn new(identity: impl Into<String>, day: impl Into<String>) -> Self {
        Self {
            identity: identity.into(),
            day: day.into(),
        }
    }

    pub fn compute(&self) -> String {
        slot_key("conductor", &self.identity, &self.day)
    }
}

/// Detects prompt-cache breaks across observations and attributes them.
#[derive(Debug, Clone, Default)]
pub struct CacheTracker {
    last_hash: Option<u64>,
    last_stable: Option<String>,
    last_tools: Option<String>,
}

impl CacheTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn step(&mut self, hash: u64) -> CacheStatus {
        let status = match self.last_hash {
            None => CacheStatus::Fresh,
            Some(prev) if prev == hash => CacheStatus::Hit,
            Some(_) => CacheStatus::Break,
        };
        self.last_hash = Some(hash);
        status
    }

    /// Observe a system prompt + stable prefix.
    pub fn observe_prefix(&mut self, system_prompt: &str, stable_prefix: &str) -> CacheStatus {
        self.step(legacy_fnv1a(&format!("{system_prompt}\n{stable_prefix}")))
    }

    /// Observe a stable-tier key; volatile content is deliberately not an argument.
    pub fn observe_stable(&mut self, stable: &StableCacheKey) -> CacheStatus {
        self.step(legacy_fnv1a(&stable.compute()))
    }

    /// Observe a stable tier plus a tool-schema digest; a `Break` is attributed to
    /// `tool-defs-changed`, `system-prefix-changed` or `unattributed-prefix-drift`.
    pub fn observe_stable_with_tools(
        &mut self,
        stable: &StableCacheKey,
        tool_digest: Option<&str>,
    ) -> (CacheStatus, Option<String>) {
        let component = stable.compute();
        let combined = match tool_digest {
            Some(d) => format!("{component}\n__tools__:{d}"),
            None => component.clone(),
        };
        let status = self.step(legacy_fnv1a(&combined));
        let reason = (status == CacheStatus::Break).then(|| {
            if self.last_tools.as_deref() != tool_digest {
                "tool-defs-changed"
            } else if self.last_stable.as_deref() != Some(component.as_str()) {
                "system-prefix-changed"
            } else {
                "unattributed-prefix-drift"
            }
            .to_string()
        });
        self.last_stable = Some(component);
        self.last_tools = tool_digest.map(str::to_string);
        (status, reason)
    }
}

/// `cache:prompt:<fnv1a(system_prompt)>` for system-prompt-only keys.
pub fn system_prompt_only_cache_key(system_prompt: &str) -> String {
    format!("cache:prompt:{:016x}", legacy_fnv1a(system_prompt))
}

#[cfg(test)]
mod fnv_tests {
    use super::*;

    // Reference vectors from the FNV test suite (FNV-1a, 64-bit).
    #[test]
    fn fnv1a_64_matches_the_reference_vectors() {
        assert_eq!(fnv1a_64(""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64("a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64("foobar"), 0x85944171f73967e8);
    }

    #[test]
    #[allow(deprecated)]
    fn the_legacy_variant_is_unchanged_and_differs_from_the_standard() {
        // Pinned so existing persisted keys keep their values.
        let legacy = fnv1a("");
        assert_eq!(legacy, 1469598103934665603);
        assert_ne!(legacy, fnv1a_64(""));
    }
}
