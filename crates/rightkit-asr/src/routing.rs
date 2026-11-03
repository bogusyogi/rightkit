//! Engine selection by language. Policy-free mechanics: the caller states the
//! language and which engines it has models for; this says which to use.
//!
//! Rule (HeardRight's shipping behaviour, generalised):
//! - `Auto`: Parakeet (multilingual v3, auto-detects across its 25 European
//!   languages) when available, else Whisper (its own detection).
//! - A specific language: Parakeet has no language input, so locking a
//!   language needs Whisper. If only Parakeet is available and the language is
//!   one of Parakeet v3's, Parakeet is returned with `language_locked = false`
//!   so the caller knows the request was honoured only by auto-detection.
//!   If only Whisper is available it is used. Unsupported combinations are errors.
use crate::error::{Error, Result};
use crate::whisper::Language;

/// Languages covered by Parakeet-TDT v3.
pub const PARAKEET_V3_LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "ru", "sk", "sl", "es", "sv", "uk",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Parakeet,
    Whisper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    pub parakeet: bool,
    pub whisper: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingPolicy {
    /// Use Whisper whenever a specific language is requested and Whisper is available.
    pub lock_language_with_whisper: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self { lock_language_with_whisper: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineSelection {
    pub engine: EngineKind,
    /// True when the engine will actually decode in the requested language.
    pub language_locked: bool,
}

pub fn parakeet_supports(code: &str) -> bool {
    PARAKEET_V3_LANGUAGES.contains(&code)
}

pub fn select_engine(
    language: &Language,
    available: Availability,
    policy: RoutingPolicy,
) -> Result<EngineSelection> {
    let pick = |engine, language_locked| Ok(EngineSelection { engine, language_locked });
    match language {
        Language::Auto => match (available.parakeet, available.whisper) {
            (true, _) => pick(EngineKind::Parakeet, false),
            (false, true) => pick(EngineKind::Whisper, false),
            _ => Err(Error::Config("no ASR engine available".into())),
        },
        Language::Code(code) => {
            if available.whisper && (policy.lock_language_with_whisper || !available.parakeet || !parakeet_supports(code)) {
                pick(EngineKind::Whisper, true)
            } else if available.parakeet && parakeet_supports(code) {
                pick(EngineKind::Parakeet, false)
            } else {
                Err(Error::Config(format!("no available ASR engine supports language `{code}`")))
            }
        }
    }
}
