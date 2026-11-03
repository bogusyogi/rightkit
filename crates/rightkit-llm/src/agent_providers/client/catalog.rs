//! `ra-018` decomposition: known-provider catalog and lookup helpers — moved
//! verbatim out of `client.rs`. Catalog values, model-id translation, and
//! billing-route classification are unchanged; only the module boundary is
//! new. See `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.

/// Known providers — mirrors `ProviderCatalog` from the .NET codebase.
pub struct ProviderInfo {
    pub id: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub display_name: &'static str,
    pub api_key_env: &'static str,
    pub setup_hint: &'static str,
    /// the host app operational request water marks for inline-image history.
    /// These are deliberately conservative application budgets, not claims
    /// about an undocumented provider maximum.
    pub image_request_high_water_bytes: usize,
    pub image_request_low_water_bytes: usize,
}

/// A provider's API-key env-var name. Only builds that read keys from the environment
/// (`host-env`, tests) carry it; the mobile build takes keys from the app and stores none.
#[cfg(feature = "agent_providers-host-env")]
macro_rules! key_env {
    ($name:literal) => {
        $name
    };
}
#[cfg(not(feature = "agent_providers-host-env"))]
macro_rules! key_env {
    ($name:literal) => {
        ""
    };
}

pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "openai",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-5.6",
        display_name: "OpenAI / ChatGPT",
        api_key_env: key_env!("OPENAI_API_KEY"),
        setup_hint: "Sign in through OpenAI's reusable ChatGPT browser auth, or use an API key",
        image_request_high_water_bytes: 24 * 1024 * 1024,
        image_request_low_water_bytes: 12 * 1024 * 1024,
    },
    ProviderInfo {
        id: "commandcode",
        base_url: "https://api.commandcode.ai/provider/v1",
        default_model: "gpt-5.6-sol",
        display_name: "Command Code",
        api_key_env: key_env!("COMMAND_CODE_API_KEY"),
        setup_hint: "Sign in through Command Code's reusable browser auth",
        image_request_high_water_bytes: 24 * 1024 * 1024,
        image_request_low_water_bytes: 12 * 1024 * 1024,
    },
    ProviderInfo {
        id: "opencode-go",
        base_url: "https://opencode.ai/zen/go/v1",
        default_model: "gpt-5.6-luna",
        display_name: "OpenCode Go",
        api_key_env: key_env!("OPENCODE_API_KEY"),
        setup_hint: "Paste the API key from your OpenCode Go account",
        image_request_high_water_bytes: 24 * 1024 * 1024,
        image_request_low_water_bytes: 12 * 1024 * 1024,
    },
    ProviderInfo {
        id: "opencode",
        base_url: "https://opencode.ai/zen/v1",
        default_model: "mimo-v2.5-free",
        display_name: "OpenCode Zen",
        api_key_env: key_env!("OPENCODE_API_KEY"),
        setup_hint: "Paste the API key from your OpenCode Zen account",
        image_request_high_water_bytes: 24 * 1024 * 1024,
        image_request_low_water_bytes: 12 * 1024 * 1024,
    },
    ProviderInfo {
        id: "minimax",
        base_url: "https://api.minimax.io/v1",
        default_model: "MiniMax-M3",
        display_name: "MiniMax",
        api_key_env: key_env!("MINIMAX_API_KEY"),
        setup_hint: "Use an API or Token Plan key from platform.minimax.io",
        image_request_high_water_bytes: 16 * 1024 * 1024,
        image_request_low_water_bytes: 8 * 1024 * 1024,
    },
    ProviderInfo {
        id: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "moonshotai/kimi-k2.6",
        display_name: "OpenRouter",
        api_key_env: key_env!("OPENROUTER_API_KEY"),
        setup_hint: "Get your key at openrouter.ai/keys",
        image_request_high_water_bytes: 24 * 1024 * 1024,
        image_request_low_water_bytes: 12 * 1024 * 1024,
    },
    ProviderInfo {
        id: "custom",
        base_url: "",
        default_model: "",
        display_name: "Custom (OpenAI-compatible)",
        api_key_env: "",
        setup_hint: "Set base URL and model in Settings > Models",
        image_request_high_water_bytes: 8 * 1024 * 1024,
        image_request_low_water_bytes: 4 * 1024 * 1024,
    },
];

/// Translate the host app's stable catalog slug into a provider-native model id.
/// OpenRouter accepts the catalog slug; MiniMax's own compatible endpoint does not.
pub fn provider_model_id(provider_id: &str, model_id: &str) -> String {
    if provider_id.eq_ignore_ascii_case("minimax")
        && (model_id.eq_ignore_ascii_case("minimax/minimax-m3")
            || model_id.eq_ignore_ascii_case("minimax-m3"))
    {
        "MiniMax-M3".to_string()
    } else {
        model_id.to_string()
    }
}

pub fn find_provider(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.id.eq_ignore_ascii_case(id))
}

/// Resolve exact catalog routes without treating arbitrary compatible hosts as
/// a known provider.
pub fn find_provider_by_base_url(base_url: &str) -> Option<&'static ProviderInfo> {
    let normalized = base_url.trim().trim_end_matches('/');
    PROVIDERS.iter().find(|provider| {
        !provider.base_url.is_empty()
            && normalized.eq_ignore_ascii_case(provider.base_url.trim_end_matches('/'))
    })
}

/// Direct MiniMax traffic uses the user's Token Plan rather than metered
/// per-token billing. OpenRouter traffic remains metered even for MiniMax models.
pub fn is_subscription_billed_route(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/');
    find_provider("minimax").is_some_and(|provider| {
        normalized.eq_ignore_ascii_case(provider.base_url.trim_end_matches('/'))
    })
}

/// Direct MiniMax text traffic can use either an ordinary API key or a
/// stored Token Plan. The daemon checks this exact catalog route before a
/// request; arbitrary compatible URLs never inherit token-plan behavior.
pub fn token_plan_provider_for_base_url(base_url: &str) -> Option<&'static str> {
    let normalized = base_url.trim().trim_end_matches('/');
    find_provider("minimax").and_then(|provider| {
        normalized
            .eq_ignore_ascii_case(provider.base_url.trim_end_matches('/'))
            .then_some(provider.id)
    })
}
