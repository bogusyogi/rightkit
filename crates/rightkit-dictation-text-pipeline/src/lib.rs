//! The app text pipeline: deterministic polish, vocabulary, screen context,
//! bounded remote cleanup, and canonical polish evaluation.

#![allow(
    clippy::result_large_err,
    clippy::large_enum_variant,
    clippy::too_many_arguments,
    clippy::doc_lazy_continuation,
    clippy::manual_clamp
)]

pub mod canonical_polish_harness;
pub mod guardrail_replay;
pub mod l1c1_serialize;
pub mod l3_cleanup;
pub mod provider_keys;
pub mod screen_vocab;
pub mod settings;
pub mod text_polish;
pub mod vocabulary;

// Shared size bound used by focus capture and cleanup sanitization. Keeping it
// here prevents UI/focus implementation details from leaking into this crate.
pub mod focus {
    pub const FIELD_CONTEXT_MAX_CHARS: usize = 1_500;
}

#[cfg(test)]
mod test_support {
    static TEST_ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

    pub(crate) fn test_env_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_ENV_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
