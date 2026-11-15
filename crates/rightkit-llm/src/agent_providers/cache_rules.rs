//! Prompt-cache semantics per route, as data. The request builders read their marker rules from
//! here (`anthropic_client.rs::build_message_body`, `client.rs::build_chat_request`), so the table
//! and the wire behaviour cannot drift apart.
//!
//! Sizes and lifetimes are the vendors' published figures; a model not named below gets the row's
//! default. `min_cacheable_tokens` is advisory (a prefix shorter than it is simply not cached by the
//! vendor, which is not an error), so no builder gates on it.

/// How a route is told what to cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMarking {
    /// Explicit `cache_control` breakpoints on blocks (Anthropic Messages).
    ExplicitBreakpoints,
    /// `cache_control` on the system message and the last tool of a chat-completions request;
    /// honoured by gateways that pass it through (OpenRouter and similar).
    GatewayCacheControl,
    /// The vendor caches the longest stable prefix by itself; the request carries no marker.
    AutomaticPrefix,
}

/// Cache semantics of one route family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheRules {
    pub route: &'static str,
    pub marking: CacheMarking,
    /// Most markers one request may carry (0 when the route takes none).
    pub max_breakpoints: usize,
    /// Cache lifetime of the default marker, in seconds.
    pub default_ttl_secs: u32,
    /// The `ttl` value of the extended-lifetime marker, when the route has one.
    pub extended_ttl: Option<&'static str>,
}

/// Native Anthropic Messages route.
pub const ANTHROPIC_NATIVE: CacheRules = CacheRules {
    route: "anthropic-native",
    marking: CacheMarking::ExplicitBreakpoints,
    max_breakpoints: 4,
    default_ttl_secs: 300,
    extended_ttl: Some("1h"),
};

/// First-party OpenAI chat route (strict body validation: an unknown field is an error).
pub const OPENAI_OFFICIAL: CacheRules = CacheRules {
    route: "openai-official",
    marking: CacheMarking::AutomaticPrefix,
    max_breakpoints: 0,
    default_ttl_secs: 300,
    extended_ttl: None,
};

/// Any other OpenAI-compatible chat route.
pub const OPENAI_COMPAT_GATEWAY: CacheRules = CacheRules {
    route: "openai-compatible-gateway",
    marking: CacheMarking::GatewayCacheControl,
    max_breakpoints: 2,
    default_ttl_secs: 300,
    extended_ttl: None,
};

/// Every row, for listing and for the per-row tests.
pub const CACHE_RULES: &[CacheRules] = &[ANTHROPIC_NATIVE, OPENAI_OFFICIAL, OPENAI_COMPAT_GATEWAY];

/// The rules for the OpenAI-compatible chat route with this base-URL class.
pub fn chat_route_rules(is_official_openai: bool) -> &'static CacheRules {
    if is_official_openai {
        &OPENAI_OFFICIAL
    } else {
        &OPENAI_COMPAT_GATEWAY
    }
}

/// Smallest prefix, in tokens, the vendor will cache for `model` on `rules`' route; `None` when the
/// route sets no floor of its own.
pub fn min_cacheable_tokens(rules: &CacheRules, model: &str) -> Option<u32> {
    let model = model.to_ascii_lowercase();
    match rules.marking {
        CacheMarking::ExplicitBreakpoints => {
            if model.contains("haiku-3") || model.contains("3-haiku") || model.contains("3-5-haiku")
            {
                Some(2048)
            } else if model.contains("opus-4-5")
                || model.contains("haiku-4-5")
                || model.contains("haiku-5")
            {
                Some(4096)
            } else {
                Some(1024)
            }
        }
        CacheMarking::AutomaticPrefix => Some(1024),
        CacheMarking::GatewayCacheControl => None,
    }
}

impl CacheRules {
    /// Whether requests on this route carry cache markers at all.
    pub fn takes_markers(&self) -> bool {
        self.max_breakpoints > 0
    }
}
