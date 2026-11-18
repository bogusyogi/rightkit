//! Named credential slots over one [`Store`]: per-slot read outcome, an interaction choice per
//! read, and save and clear by slot name.
//!
//! An app that keeps a few optional secrets (provider API keys, for example) names each one as a
//! [`Slot`] (a logical name for diagnostics plus the store account) and reads them through a
//! [`SlotStore`]. Every read reports one [`SlotOutcome`] per slot, and the three ways a read can
//! come back empty are kept apart:
//!
//! * [`SlotOutcome::Missing`]: the store answered that there is no item (or the item is blank).
//! * [`SlotOutcome::Refused`]: the store would not answer: a guard denies it, the store is
//!   non-interactive and the OS wants a prompt, or the user or OS policy denied access.
//! * [`SlotOutcome::Inaccessible`]: the store failed (unavailable, timed out, backend error, a value
//!   that is not text).
//!
//! A caller must never turn a refused or inaccessible slot into a missing one: a keychain or ACL
//! regression would then look the same as a user who deliberately has no key. The same rule as
//! [`crate::Error::is_unavailable_or_blocked`], per slot.
//!
//! Interaction is chosen by the host: a store built interactive may raise the OS prompt on a read
//! (an explicit user action, or an app update that re-signed the binary); [`SlotStore::read_with`]
//! forces either mode for one read. Nothing in this module logs or formats a secret value; errors
//! come from [`crate::Error`]. Callers must keep credential values out of error text.
//!
//! The slot set, the logical names, the accounts, the namespace and every message text are the
//! host's. This module names no provider and no product.
//!
//! ```no_run
//! use rightkit_credentials::slots::{Slot, SlotStore};
//! use rightkit_credentials::{Config, Interaction, Namespace, Store};
//!
//! let ns = Namespace::new("com.example.app.keys")?;
//! let store = Store::open(Config::new(ns).interaction(Interaction::Interactive));
//! let slots = SlotStore::new(store, vec![Slot::new("alpha", "alpha"), Slot::new("beta", "beta")])?;
//! let snapshot = slots.read_all_retrying(std::time::Duration::from_millis(100));
//! if snapshot.any_failure() { /* tell the engine "unknown", never "empty" */ }
//! # Ok::<(), rightkit_credentials::Error>(())
//! ```

use crate::{Error, Interaction, Secret, Store};
use std::time::Duration;

/// One named secret: a logical `name` (diagnostics, hand-off) and the store `account` it lives at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub account: String,
}

impl Slot {
    pub fn new(name: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            account: account.into(),
        }
    }
}

/// What one slot read found. See the module documentation for the three empty cases.
#[derive(Debug, Clone)]
pub enum SlotOutcome {
    /// A non-blank text value.
    Present(Secret),
    /// The store reports no item, or the item is empty after trimming.
    Missing,
    /// Access was refused: a guard, a needed prompt in a non-interactive store, or a denial.
    Refused(Error),
    /// The store could not be read: unavailable, timed out, backend error, or not valid UTF-8.
    Inaccessible(Error),
}

impl SlotOutcome {
    /// Classify the raw result of a store read.
    pub fn classify(read: Result<Option<Secret>, Error>) -> Self {
        match read {
            Ok(Some(secret)) => match secret.expose_str() {
                Ok(text) if text.trim().is_empty() => SlotOutcome::Missing,
                Ok(_) => SlotOutcome::Present(secret),
                Err(error) => SlotOutcome::Inaccessible(error),
            },
            Ok(None) => SlotOutcome::Missing,
            Err(error @ (Error::Guarded { .. } | Error::InteractionRequired | Error::Denied)) => {
                SlotOutcome::Refused(error)
            }
            Err(error) => SlotOutcome::Inaccessible(error),
        }
    }

    pub fn is_present(&self) -> bool {
        matches!(self, SlotOutcome::Present(_))
    }

    /// True for `Refused` and `Inaccessible`: the slot could not be read, which is not the same
    /// as the slot being empty.
    pub fn is_failure(&self) -> bool {
        matches!(self, SlotOutcome::Refused(_) | SlotOutcome::Inaccessible(_))
    }

    /// The failure, if the read failed.
    pub fn error(&self) -> Option<&Error> {
        match self {
            SlotOutcome::Refused(error) | SlotOutcome::Inaccessible(error) => Some(error),
            _ => None,
        }
    }

    /// The text value of a present slot, exactly as stored (not trimmed).
    pub fn text(&self) -> Option<&str> {
        match self {
            SlotOutcome::Present(secret) => secret.expose_str().ok(),
            _ => None,
        }
    }

    /// Safe for diagnostics: `present`, `missing`, `refused` or `inaccessible`.
    pub fn label(&self) -> &'static str {
        match self {
            SlotOutcome::Present(_) => "present",
            SlotOutcome::Missing => "missing",
            SlotOutcome::Refused(_) => "refused",
            SlotOutcome::Inaccessible(_) => "inaccessible",
        }
    }
}

/// One read of every slot, in slot order.
#[derive(Debug, Clone)]
pub struct SlotSnapshot {
    entries: Vec<(String, SlotOutcome)>,
}

impl SlotSnapshot {
    pub fn get(&self, name: &str) -> Option<&SlotOutcome> {
        self.entries
            .iter()
            .find(|(entry, _)| entry == name)
            .map(|(_, outcome)| outcome)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &SlotOutcome)> {
        self.entries
            .iter()
            .map(|(name, outcome)| (name.as_str(), outcome))
    }

    /// True when any slot was refused or inaccessible.
    pub fn any_failure(&self) -> bool {
        self.entries.iter().any(|(_, outcome)| outcome.is_failure())
    }

    /// Take one slot's outcome out of the snapshot (so the secret can move, not be copied).
    pub fn take(&mut self, name: &str) -> Option<SlotOutcome> {
        let index = self.entries.iter().position(|(entry, _)| entry == name)?;
        Some(self.entries.remove(index).1)
    }
}

/// Result of deleting one explicitly declared slot. No secret is read by this API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotClearOutcome {
    Removed,
    Missing,
    Refused(Error),
    Inaccessible(Error),
}

impl SlotClearOutcome {
    fn classify(result: Result<bool, Error>) -> Self {
        match result {
            Ok(true) => Self::Removed,
            Ok(false) => Self::Missing,
            Err(error @ (Error::Guarded { .. } | Error::InteractionRequired | Error::Denied)) => {
                Self::Refused(error)
            }
            Err(error) => Self::Inaccessible(error),
        }
    }

    pub fn error(&self) -> Option<&Error> {
        match self {
            Self::Refused(error) | Self::Inaccessible(error) => Some(error),
            _ => None,
        }
    }

    pub fn is_failure(&self) -> bool {
        self.error().is_some()
    }
}

/// Every requested deletion's result, in request order. Partial failure stays visible;
/// callers must report failed slots even when other slots were removed.
#[must_use = "inspect deletion outcomes; a partial failure is not success"]
#[derive(Debug, Clone)]
pub struct SlotClearReport {
    entries: Vec<(String, SlotClearOutcome)>,
}

impl SlotClearReport {
    pub fn get(&self, name: &str) -> Option<&SlotClearOutcome> {
        self.entries
            .iter()
            .find(|(entry, _)| entry == name)
            .map(|(_, outcome)| outcome)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &SlotClearOutcome)> {
        self.entries
            .iter()
            .map(|(name, outcome)| (name.as_str(), outcome))
    }

    pub fn any_failure(&self) -> bool {
        self.entries.iter().any(|(_, outcome)| outcome.is_failure())
    }
}

/// A [`Store`] plus the named slots an app keeps in it.
#[derive(Clone)]
pub struct SlotStore {
    store: Store,
    slots: Vec<Slot>,
}

impl SlotStore {
    /// Slot names must be unique; a duplicate is `Error::Invalid`.
    pub fn new(store: Store, slots: Vec<Slot>) -> Result<Self, Error> {
        for (index, slot) in slots.iter().enumerate() {
            crate::namespace::check_account(&slot.account)?;
            if slot.name.is_empty() || slot.name.chars().any(char::is_control) {
                return Err(Error::Invalid {
                    what: "slot",
                    reason: "name must be non-empty without control characters".into(),
                });
            }
            if slots[..index].iter().any(|earlier| earlier.name == slot.name) {
                return Err(Error::Invalid {
                    what: "slot",
                    reason: format!("duplicate slot name {}", slot.name),
                });
            }
        }
        Ok(Self { store, slots })
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    fn account(&self, name: &str) -> Result<&str, Error> {
        self.slots
            .iter()
            .find(|slot| slot.name == name)
            .map(|slot| slot.account.as_str())
            .ok_or_else(|| Error::Invalid {
                what: "slot",
                reason: format!("unknown slot name {name}"),
            })
    }

    /// Read one slot with the store's own interaction mode.
    pub fn read(&self, name: &str) -> Result<SlotOutcome, Error> {
        Ok(SlotOutcome::classify(self.store.get(self.account(name)?)))
    }

    /// Read one slot forcing the interaction mode for this read only.
    pub fn read_with(&self, name: &str, interaction: Interaction) -> Result<SlotOutcome, Error> {
        let account = self.account(name)?;
        Ok(SlotOutcome::classify(
            self.store_for(interaction).get(account),
        ))
    }

    /// Read every slot once with the store's own interaction mode.
    pub fn read_all(&self) -> SlotSnapshot {
        self.snapshot(&self.store)
    }

    /// Read every slot once forcing the interaction mode.
    pub fn read_all_with(&self, interaction: Interaction) -> SlotSnapshot {
        self.snapshot(&self.store_for(interaction))
    }

    /// Read every slot; when any read failed, wait `delay` and read every slot again, returning
    /// the second read. A transient keychain or Credential Manager failure (a sidecar restart
    /// racing the store) is retried once; a persistent one is reported as it is.
    pub fn read_all_retrying(&self, delay: Duration) -> SlotSnapshot {
        let first = self.read_all();
        if !first.any_failure() {
            return first;
        }
        drop(first);
        std::thread::sleep(delay);
        self.read_all()
    }

    /// Save `value` into the slot (create or replace), with the store's own interaction mode.
    /// The caller validates and trims the value first; its rules are the host's.
    pub fn write(&self, name: &str, value: &str) -> Result<(), Error> {
        self.store.set_str(self.account(name)?, value)
    }

    /// Remove the slot's item. Clearing an absent item succeeds.
    pub fn clear(&self, name: &str) -> Result<(), Error> {
        self.store.delete(self.account(name)?).map(|_| ())
    }

    /// Delete only requested, declared slots. Validate every name before any OS call,
    /// reject duplicate requests, then attempt every deletion even after an OS failure.
    /// `Ok(report)` means the request was valid; inspect `report.any_failure()`.
    /// No entitlement check is imposed: apps must allow users to clear their own secrets
    /// after downgrade. OS authorization, interaction policy & guards still apply.
    pub fn clear_many(&self, names: &[&str]) -> Result<SlotClearReport, Error> {
        for (index, name) in names.iter().enumerate() {
            self.account(name)?;
            if names[..index].contains(name) {
                return Err(Error::Invalid {
                    what: "slot",
                    reason: format!("duplicate clear request {name}"),
                });
            }
        }
        Ok(SlotClearReport {
            entries: names
                .iter()
                .map(|name| {
                    // All names were validated before mutations started.
                    let outcome = match self.account(name) {
                        Ok(account) => SlotClearOutcome::classify(self.store.delete(account)),
                        Err(error) => SlotClearOutcome::Inaccessible(error),
                    };
                    ((*name).to_owned(), outcome)
                })
                .collect(),
        })
    }

    /// Delete every declared slot, continuing after failures. Never enumerates OS items
    /// or expands beyond this app-provided slot list. Inspect every reported failure.
    pub fn clear_all(&self) -> SlotClearReport {
        SlotClearReport {
            entries: self
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.name.clone(),
                        SlotClearOutcome::classify(self.store.delete(&slot.account)),
                    )
                })
                .collect(),
        }
    }

    fn store_for(&self, interaction: Interaction) -> Store {
        match interaction {
            Interaction::Interactive => self.store.interactive(),
            Interaction::NonInteractive => self.store.non_interactive(),
        }
    }

    fn snapshot(&self, store: &Store) -> SlotSnapshot {
        SlotSnapshot {
            entries: self
                .slots
                .iter()
                .map(|slot| {
                    (
                        slot.name.clone(),
                        SlotOutcome::classify(store.get(&slot.account)),
                    )
                })
                .collect(),
        }
    }
}
