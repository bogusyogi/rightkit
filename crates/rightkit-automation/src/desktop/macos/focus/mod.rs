//! Focused-field accessibility (AX) reading, insertion-confirmation proof,
//! and the `FocusPin` exact-editor capture used across a recording's
//! lifetime, split by concern:
//!   - `context`: focused-field/AX context reading (`FocusedField`).
//!   - `proof`: `FocusedFieldProof` capture and insertion confirmation.
//!   - `native_write`: native (non-clipboard) selection-write capability.
//!   - `polish`: polish-only field context probe.
//!   - `pin`: `FocusPin` capture/validation lifecycle.
//!   - `harvest`: window-text harvest for screen-context vocabulary.
//!
//! Proof capture and confirmation both resolve against a retained
//! `FocusPin`, so `proof` and `pin` stay cross-referencing siblings rather
//! than a strict layering. Every path resolves as
//! `crate::desktop::macos::X` via the `pub use` re-exports below.

mod context;
mod harvest;
mod native_write;
mod pin;
mod polish;
mod proof;

pub use context::*;
pub use harvest::*;
pub use native_write::*;
pub use pin::*;
pub use polish::*;
pub use proof::*;
