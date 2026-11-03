//! Session-scoped secrets/environment values (ENG-106).
//!
//! Distinct from `crate::agent_config::migrations` and the rest of this crate's on-disk
//! `CodeRightConfig`: a [`SessionSecrets`] value is **never serialized**,
//! never written to the config TOML, never durable across process restarts,
//! and scoped to exactly one running session. It exists to answer one
//! question for the engine: "what key/value pairs get injected into this
//! run's environment" — without ever putting a secret value anywhere that
//! could end up in the transcript, the journal, or an exported session
//! package.
//!
//! # How secrets are kept out of the transcript/journal/exports
//!
//! - [`SessionSecrets`] deliberately does **not** derive `Serialize` or
//!   `Debug` printing of values — its custom [`std::fmt::Debug`] impl prints
//!   only key names and value lengths, so an accidental `{:?}` in a log
//!   statement cannot leak a value.
//! - [`SessionSecrets::inject_into`] is the *only* way values leave this
//!   type, and it writes into a plain `HashMap<String, String>` meant for a
//!   process-spawn environment block — a boundary that is not the model
//!   context, not `ToolActivity`/transcript rendering, and not the journal
//!   writer. Callers on the daemon side must build the child process
//!   environment from that map directly; nothing here formats it into a
//!   string that could be echoed back through a tool-call argument or result
//!   (the same boundary `MCPSecretStore`/`mcp_auth` document on the client
//!   side).
//! - There is no `to_toml`/`to_json`/journal-append function anywhere in this
//!   module. A session package (see `history::session_package`, ENG-101) or
//!   an audit log entry that wants to record *that* a secret was configured
//!   should record [`SessionSecrets::key_names`] — names only, never values —
//!   which is exactly why that accessor exists and the values accessor does
//!   not have an equivalent "list everything" form.
//! - Values live only in process memory for the lifetime of the owning
//!   session and are dropped (not zeroed — this crate has no dependency that
//!   would justify pulling in a zeroizing allocator, and the values are
//!   ordinary API keys/env values rather than long-lived cryptographic key
//!   material) when the session ends.

use std::collections::HashMap;

/// A single session-scoped secret/env value. Kept as a named type (rather
/// than a bare `HashMap<String, String>`) so the "value" side has a place to
/// carry provenance (e.g. "typed in the Secrets editor" vs. "inherited from
/// `RIGHTKIT_*`") in future without changing the public shape callers hold.
#[derive(Clone, PartialEq, Eq)]
struct SecretValue(String);

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<redacted, {} bytes>", self.0.len())
    }
}

/// A bounded, in-memory-only collection of session-scoped secret/env values.
///
/// This type never derives or implements `Serialize`/`Deserialize` — that is
/// load-bearing, not an oversight. Session secrets must never round-trip
/// through the on-disk config format, a journal entry, or a session package
/// export.
#[derive(Clone, Default)]
pub struct SessionSecrets {
    values: HashMap<String, SecretValue>,
}

/// Errors returned when setting a session secret.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionSecretError {
    /// The key was empty, or not a valid environment-variable-style
    /// identifier (letters/digits/underscore, not starting with a digit).
    #[error("'{0}' is not a valid session secret key")]
    InvalidKey(String),
}

impl SessionSecrets {
    /// Create an empty set of session secrets.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or overwrite) a session secret. Validates the key shape so a
    /// malformed key can never silently fail to inject at run time — the
    /// caller-facing editor and the injection path use the same validation.
    pub fn set(&mut self, key: &str, value: impl Into<String>) -> Result<(), SessionSecretError> {
        if !is_valid_key(key) {
            return Err(SessionSecretError::InvalidKey(key.to_string()));
        }
        self.values
            .insert(key.to_string(), SecretValue(value.into()));
        Ok(())
    }

    /// Remove a session secret. No-op if the key was not set.
    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }

    /// Whether a value is currently set for `key`. Used by editors that want
    /// to show a "saved" indicator without reading the value back out.
    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// Number of secrets currently held.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no secrets are held.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Key names only, sorted for stable iteration. Safe to log, put in a
    /// receipt, or show in an audit trail — this is the accessor that exists
    /// specifically so callers never need to reach for the values to report
    /// "what was configured."
    pub fn key_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.values.keys().cloned().collect();
        names.sort();
        names
    }

    /// The one sanctioned exit point for actual values: write every held
    /// secret into `env`, a plain map destined for a child-process
    /// environment block. Existing keys in `env` are overwritten — session
    /// secrets take precedence over any inherited ambient environment, which
    /// matches "session-scoped override" semantics.
    ///
    /// Callers must build the process environment from `env` directly (e.g.
    /// `Command::envs`) and must never pass `env`'s contents through any
    /// tool-call argument, model-visible context, transcript entry, or
    /// journal write.
    pub fn inject_into(&self, env: &mut HashMap<String, String>) {
        for (key, value) in &self.values {
            env.insert(key.clone(), value.0.clone());
        }
    }
}

impl std::fmt::Debug for SessionSecrets {
    /// Deliberately prints only key names, never values, so an accidental
    /// `{:?}` (in a log line, a panic message, or a test failure output)
    /// cannot leak a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSecrets")
            .field("key_names", &self.key_names())
            .finish()
    }
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
