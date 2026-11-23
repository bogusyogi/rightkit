//! The renderer-facing layer over [`KeyVault`]: the flat JSON status and update shapes, the whole
//! "save" flow (gate, apply, refresh, report) and the readiness label of the one eligible provider.
//!
//! Every name is derived from the provider table the host passes to [`KeyVault::new`].
//! A host whose renderer already uses `<id>_saved`, `<id>_key` and `clear_<id>`
//! keeps its wire shape without a typed struct per provider. Which
//! provider is eligible, the gate that guards a write and what happens after a change are the
//! host's inputs.
//!
//! No key, and no fragment of one, appears in any value this module returns or in `Debug` output
//! ([`KeysUpdate`] has none).

use rightkit_credentials::Store;
use serde_json::{Map, Value};

use crate::{
    classify_key_read, KeyChange, KeyVault, Provider, SetOutcome, SidecarKey, SidecarKeyState,
};

/// A requested change of every slot, read from the renderer's flat JSON object: `<id>_key` (a
/// string, or null for "leave alone") and `clear_<id>` (a bool, default false) for each provider
/// of the table. Unknown fields are ignored.
pub struct KeysUpdate {
    /// Table order: provider, key to store, clear.
    changes: Vec<(Provider, Option<String>, bool)>,
}

impl KeysUpdate {
    /// Read `value` against `providers`. A field of the wrong type is refused by name only.
    pub fn from_json(providers: &[Provider], value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "key update must be an object".to_string())?;
        let mut changes = Vec::with_capacity(providers.len());
        for provider in providers {
            let key_field = format!("{}_key", provider.id);
            let key = match object.get(&key_field) {
                None | Some(Value::Null) => None,
                Some(Value::String(text)) => Some(text.clone()),
                Some(_) => return Err(format!("{key_field} must be a string")),
            };
            let clear_field = format!("clear_{}", provider.id);
            let clear = match object.get(&clear_field) {
                None => false,
                Some(Value::Bool(flag)) => *flag,
                Some(_) => return Err(format!("{clear_field} must be a boolean")),
            };
            changes.push((*provider, key, clear));
        }
        Ok(Self { changes })
    }

    /// True when the update would store a key. Clearing is never a write that needs entitlement:
    /// an owner can always delete their own keys.
    pub fn writes_keys(&self) -> bool {
        self.changes
            .iter()
            .any(|(_, key, clear)| !*clear && key.is_some())
    }

    fn into_changes(self) -> Vec<KeyChange> {
        self.changes
            .into_iter()
            .map(|(provider, key, clear)| KeyChange {
                provider,
                clear,
                key,
            })
            .collect()
    }
}

impl<O> KeyVault<O>
where
    O: Fn() -> Result<Store, String>,
{
    /// The renderer's status object: `{ "<id>_saved": bool }` for every provider. A slot that
    /// cannot be read counts as not saved here; the engine hand-off ([`Self::load_for_sidecar`])
    /// keeps the read failure distinct.
    pub fn status(&self) -> Map<String, Value> {
        self.providers()
            .iter()
            .zip(self.read_all(false))
            .map(|(provider, result)| {
                (
                    format!("{}_saved", provider.id),
                    Value::Bool(result.is_ok_and(|key| !key.trim().is_empty())),
                )
            })
            .collect()
    }

    /// Read every provider once for an engine hand-off, in table order. Callers must never turn a
    /// read failure into an empty key: doing so makes an OS keychain or ACL regression
    /// indistinguishable from a customer deliberately having no provider configured. The slot
    /// store retries once (after 100 ms) so a restart racing a transient read does not freeze the
    /// provider set for the process lifetime.
    pub fn load_for_sidecar(&self) -> Vec<SidecarKey> {
        self.read_all(true)
            .into_iter()
            .map(classify_key_read)
            .collect()
    }

    /// Apply every requested slot change. A failure on one slot never stops the others: all are
    /// attempted and each failure is reported.
    pub fn apply_update(&self, update: KeysUpdate) -> SetOutcome {
        self.apply(update.into_changes())
    }

    /// The whole save flow for a renderer update object: read it, run `gate` first when it would
    /// store a key (a refusal is returned as is and nothing is written; clearing is never gated),
    /// apply every change, call `on_mutated` after any change (also a partial one) so the host can
    /// refresh the credentials its engine holds, then answer the new [`Self::status`] or, when a
    /// slot failed, every failure joined with `"; "`.
    pub fn set_from_json(
        &self,
        update: &Value,
        gate: impl FnOnce() -> Result<(), String>,
        on_mutated: impl FnOnce(),
    ) -> Result<Map<String, Value>, String> {
        let update = KeysUpdate::from_json(self.providers(), update)?;
        if update.writes_keys() {
            gate()?;
        }
        let outcome = self.apply_update(update);
        if outcome.mutated {
            on_mutated();
        }
        if outcome.failures.is_empty() {
            Ok(self.status())
        } else {
            Err(outcome.failures.join("; "))
        }
    }
}

/// Whether the one provider the product routes to can serve a request. Only that provider counts:
/// a key saved for a dormant provider must not make cloud features look ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    state: SidecarKeyState,
    label: String,
}

impl Readiness {
    /// The readiness for `eligible` (a provider `id`) given the state of its slot read.
    pub fn of(eligible: &str, state: SidecarKeyState) -> Self {
        let label = match state {
            SidecarKeyState::Present => "ready".to_string(),
            SidecarKeyState::Absent => format!("{eligible}_key_absent"),
            SidecarKeyState::ReadError => format!("{eligible}_key_read_error"),
        };
        Self { state, label }
    }

    /// `ready`, `<id>_key_absent` or `<id>_key_read_error`; safe for diagnostics.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn is_ready(&self) -> bool {
        self.state == SidecarKeyState::Present
    }
}

/// True when any of the slot reads failed (as opposed to being absent).
pub fn any_read_error(states: impl IntoIterator<Item = SidecarKeyState>) -> bool {
    states
        .into_iter()
        .any(|state| state == SidecarKeyState::ReadError)
}
