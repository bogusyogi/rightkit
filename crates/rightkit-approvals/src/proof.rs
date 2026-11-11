//! Who decided. The core never trusts a caller's word that the decider approved; an app whose
//! decision does not happen inside its own window presents a [`Proof`].
//!
//! Proofs the apps use, each implemented where its key material lives: a token minted with a
//! per-launch secret the app shell holds; a signature by an enrolled approver key; the owner's
//! PIN checked by the engine. An app whose decision is a call from its own window, in its own
//! process, decides without one.

use crate::error::Result;
use crate::model::Record;

/// Evidence that the decider approved exactly `record`.
pub trait Proof {
    /// `Ok` when the evidence covers this record (its id, its kind and its binding, or its
    /// display digest). A proof that can be presented once consumes itself here.
    fn verify(&self, record: &Record) -> Result<()>;
}
