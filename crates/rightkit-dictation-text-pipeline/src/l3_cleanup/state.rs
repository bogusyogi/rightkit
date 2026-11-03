//! Shared state for the cloud cleanup lanes: provider/circuit types, the
//! public `CleanupOutcome`/`PolishContext`/`CleanupHealth` contract, the
//! process-wide counters, and the provider-agent cache.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;

pub(crate) static CIRCUIT: OnceLock<Mutex<Circuit>> = OnceLock::new();
// Aggregate semantic-provider attempt count exposed through runtime health.
pub(crate) static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SUCCESSES: AtomicU64 = AtomicU64::new(0);
pub(crate) static FAILURES: AtomicU64 = AtomicU64::new(0);
pub(crate) static SKIPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static LOCAL_FALLBACKS: AtomicU64 = AtomicU64::new(0);
pub(crate) static CIRCUIT_OPENS: AtomicU64 = AtomicU64::new(0);
pub(crate) static AGENTS: OnceLock<Mutex<Vec<CachedAgent>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Groq,
    // Not routed by the product ladder (Groq only); kept for the circuit and health surface.
    #[allow(dead_code)]
    Cerebras,
    #[allow(dead_code)]
    Nvidia,
    #[allow(dead_code)]
    OpenRouter,
}

impl Provider {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Provider::Groq => "groq",
            Provider::Cerebras => "cerebras",
            Provider::Nvidia => "nvidia",
            Provider::OpenRouter => "openrouter",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderSpec {
    pub(crate) provider: Provider,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) model: String,
}

#[derive(Debug, Default)]
pub(crate) struct Circuit {
    groq: ProviderCircuit,
    cerebras: ProviderCircuit,
    nvidia: ProviderCircuit,
    openrouter: ProviderCircuit,
}

#[derive(Debug, Default)]
struct ProviderCircuit {
    consecutive_failures: u32,
    opened_until: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CleanupOutcome {
    Cleaned(String),
    Skipped {
        reason: &'static str,
        circuit_open: bool,
    },
    Failed {
        error_class: &'static str,
        circuit_open: bool,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PolishContext {
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    /// Existing text in the focused field (macOS AX capture) — grounds the
    /// polish in what the user is writing. Sanitized before any payload.
    pub field_text: Option<String>,
    /// The user's selection inside the focused field, if any.
    pub selected_text: Option<String>,
    /// True when the platform accessibility probe successfully reached a
    /// non-password focused field. Text may still be absent for an empty field.
    /// This is evaluation provenance only and is never inserted into prompts.
    pub field_context_available: bool,
    /// User-saved vocabulary terms, injected into LLM prompts so the model
    /// preserves their exact spelling/casing. Sanitized (secret-shaped terms
    /// dropped) before any payload is built — see `sanitize_context`.
    pub vocabulary: Vec<String>,
    /// Optional user-authored writing convention. Style-only context; never
    /// inferred from environment data and never recorded in telemetry.
    pub writing_region: Option<String>,
    /// Sound-alike aliases paired with their terms. Drawn from the same
    /// mirror as `vocabulary` but carries explicit context-gated correction
    /// ("whisper flow" -> "Wispr Flow"). Empty by default; serialization is
    /// opt-in so old callers still receive a working context.
    #[serde(default)]
    pub sound_alikes: Vec<(String, Vec<String>)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupHealth {
    pub circuit_open: bool,
    pub consecutive_failures: u32,
    pub groq_circuit_open: bool,
    pub nvidia_circuit_open: bool,
    pub openrouter_circuit_open: bool,
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub skips: u64,
    pub local_fallbacks: u64,
    pub circuit_opens: u64,
}

#[derive(Clone)]
pub(crate) struct CachedAgent {
    pub(crate) timeout_ms: u64,
    /// Base-URL override host allowed to resolve to local addresses.
    pub(crate) private_host: Option<String>,
    pub(crate) agent: Arc<rightkit_llm::http::Client>,
}

impl Circuit {
    fn provider_mut(&mut self, provider: Provider) -> &mut ProviderCircuit {
        match provider {
            Provider::Groq => &mut self.groq,
            Provider::Cerebras => &mut self.cerebras,
            Provider::Nvidia => &mut self.nvidia,
            Provider::OpenRouter => &mut self.openrouter,
        }
    }

    pub(crate) fn is_open(&mut self, provider: Provider, now: Instant) -> bool {
        self.provider_mut(provider).is_open(now)
    }

    pub(crate) fn record_success(&mut self, provider: Provider) {
        self.provider_mut(provider).record_success();
    }

    pub(crate) fn record_failure(
        &mut self,
        provider: Provider,
        threshold: u32,
        cooldown: Duration,
        now: Instant,
    ) -> bool {
        self.provider_mut(provider)
            .record_failure(threshold, cooldown, now)
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.groq
            .consecutive_failures
            .saturating_add(self.cerebras.consecutive_failures)
            .saturating_add(self.nvidia.consecutive_failures)
            .saturating_add(self.openrouter.consecutive_failures)
    }
}

impl ProviderCircuit {
    fn is_open(&mut self, now: Instant) -> bool {
        if self.opened_until.is_some_and(|until| now < until) {
            return true;
        }
        if self.opened_until.is_some() {
            self.opened_until = None;
            self.consecutive_failures = 0;
        }
        false
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.opened_until = None;
    }

    fn record_failure(&mut self, threshold: u32, cooldown: Duration, now: Instant) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= threshold {
            self.opened_until = Some(now + cooldown);
            return true;
        }
        false
    }
}
