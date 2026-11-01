use rightkit_dictation_core::engine::SecretString;
use std::sync::OnceLock;

pub use rightkit_dictation_models::model_paths;
pub use rightkit_dictation_telemetry::{app_paths, crash_dump, telemetry};
pub use rightkit_transcript_library::delivery;

pub mod engine_process;
pub mod engine_supervisor;
pub mod host;
pub mod sidecar_log;

#[derive(Clone, Default)]
pub struct SupervisorConfig {
    pub asr_backend: String,
    pub dictation_language: String,
    pub ai_polish_enabled: bool,
    pub groq_key: Option<SecretString>,
    pub nvidia_key: Option<SecretString>,
    pub openrouter_key: Option<SecretString>,
    pub groq_state: String,
    pub nvidia_state: String,
    pub openrouter_state: String,
    pub product_provider_state: String,
    pub product_provider_ready: bool,
    pub any_key_read_error: bool,
}

/// Manual `Debug`: the BYOK key fields must never be printable.
impl std::fmt::Debug for SupervisorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mask = |key: &Option<SecretString>| {
            if key.is_some() {
                "Some(<redacted>)"
            } else {
                "None"
            }
        };
        f.debug_struct("SupervisorConfig")
            .field("asr_backend", &self.asr_backend)
            .field("dictation_language", &self.dictation_language)
            .field("ai_polish_enabled", &self.ai_polish_enabled)
            .field("groq_key", &format_args!("{}", mask(&self.groq_key)))
            .field("nvidia_key", &format_args!("{}", mask(&self.nvidia_key)))
            .field(
                "openrouter_key",
                &format_args!("{}", mask(&self.openrouter_key)),
            )
            .field("groq_state", &self.groq_state)
            .field("nvidia_state", &self.nvidia_state)
            .field("openrouter_state", &self.openrouter_state)
            .field("product_provider_state", &self.product_provider_state)
            .field("product_provider_ready", &self.product_provider_ready)
            .field("any_key_read_error", &self.any_key_read_error)
            .finish()
    }
}

type SupervisorConfigProvider = fn() -> SupervisorConfig;
static SUPERVISOR_CONFIG_PROVIDER: OnceLock<SupervisorConfigProvider> = OnceLock::new();

pub fn install_supervisor_config_provider(
    provider: SupervisorConfigProvider,
) -> Result<(), SupervisorConfigProvider> {
    SUPERVISOR_CONFIG_PROVIDER.set(provider)
}

pub(crate) fn supervisor_config() -> SupervisorConfig {
    SUPERVISOR_CONFIG_PROVIDER
        .get()
        .map(|provider| provider())
        .unwrap_or_default()
}
