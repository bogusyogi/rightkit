//! Which ASR backend the engine runs for a stored choice and an entitlement, and the small
//! normalisations over the dictation settings (language, polish mode).
//!
//! No model id, default or sentence is fixed here: the host supplies the default backend, the
//! retired and legacy aliases, the supported list, the entitlement-gated prefix, and a sink for the
//! one diagnostic line.

/// Host event sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// The host's backend vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct BackendPolicy<'a> {
    /// The backend used when nothing valid is stored.
    pub default_backend: &'a str,
    /// A retired selection that keeps RUNNING until `replacement_installed` reports the default's
    /// model on disk; flipping earlier would boot the engine at a missing model.
    pub retired_until_replacement: &'a str,
    /// Aliases that predate the sidecar split; they normalise to the default immediately.
    pub legacy_aliases: &'a [&'a str],
    /// Backends kept as stored.
    pub supported: &'a [&'a str],
    /// Backends whose name starts with this need the entitlement (case-insensitive, trimmed).
    pub entitled_prefix: &'a str,
    /// Event name logged when the stored value is unsupported.
    pub unsupported_event: &'a str,
}

impl BackendPolicy<'_> {
    /// The stored backend as a registry alias (`stored` is the raw setting).
    pub fn resolve(
        &self,
        stored: Option<&str>,
        replacement_installed: impl FnOnce() -> bool,
        sink: EventSink,
    ) -> String {
        let value = stored
            .map(str::to_string)
            .unwrap_or_else(|| self.default_backend.to_string());
        let default_backend = self.default_backend;
        if value == self.retired_until_replacement {
            if replacement_installed() {
                default_backend.to_string()
            } else {
                value
            }
        } else if self.legacy_aliases.contains(&value.as_str()) {
            default_backend.to_string()
        } else if self.supported.contains(&value.as_str()) {
            value
        } else {
            sink(
                self.unsupported_event,
                &format!("backend={value}; falling back to {default_backend}"),
            );
            default_backend.to_string()
        }
    }

    /// The backend the engine may be told to run for this entitlement. Without it the default is
    /// used however the setting reads; the stored setting is left alone, so the gated backend
    /// returns with the entitlement (`raw` is [`Self::resolve`]'s result or the raw setting).
    pub fn effective(&self, raw: &str, is_entitled: bool) -> String {
        if !is_entitled
            && raw
                .trim()
                .to_ascii_lowercase()
                .starts_with(self.entitled_prefix)
        {
            return self.default_backend.to_string();
        }
        raw.to_string()
    }
}

/// The dictation language: trimmed and lowercased; `auto_code` when unset or empty.
pub fn dictation_language(stored: Option<&str>, auto_code: &str) -> String {
    stored
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| auto_code.to_string())
}

/// `stored` when it is one of `allowed`, else `default_value` (the polish mode).
pub fn choice_or_default(stored: Option<&str>, allowed: &[&str], default_value: &str) -> String {
    match stored {
        Some(value) if allowed.contains(&value) => value.to_string(),
        _ => default_value.to_string(),
    }
}
