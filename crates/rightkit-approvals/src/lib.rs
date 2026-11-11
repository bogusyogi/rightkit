//! One approvals core for every app.
//!
//! An agent, a tool or a background job asks; the user decides in the app; the act runs once.
//! Every app keeps the same record and the same rules here and differs only in where records
//! are stored and how the decider is proven:
//!
//! * **Model** ([`Request`], [`Record`]): a request has an id, a kind, a scope, an [`Origin`]
//!   (door, filer, client), a [`Binding`], a summary the decider is shown, display-only detail,
//!   a creation time and a deadline. A record adds its [`State`] (pending, approved, denied,
//!   done, failed, expired), the [`Grant`] an approval gives (one act, idle session, until a
//!   time, until revoked), a reason code and a result.
//! * **Binding** (the reference rule): an approval is always bound to a digest of the exact
//!   arguments, the destination, the expiry and the revision ([`Binding`], [`bind_of`]). At use
//!   the act about to run must present the same binding ([`Binding::check`]).
//! * **Decision** ([`rules::decide`]): only a pending, unexpired request is decided; a decision
//!   carries the display digest of what the decider was shown ([`Request::display_digest`]) and,
//!   where the decision crosses a process boundary, a [`Proof`]. The move is one
//!   compare-and-set: two deciders never both win.
//! * **Use** ([`rules::take`], [`rules::session`]): an approval is spent by one act, for the
//!   kind, scope, door, filer and destination it was given for; a session grant lives until it
//!   idles out, its time ends or it is revoked.
//! * **Stores** ([`Store`]): [`MemoryStore`] (nothing on disk approves; a restart forgets),
//!   [`FileStore`] (feature `file`: one file per request, marker files per move), or an app's
//!   database implementing the trait.
//!
//! No app names, wire tags or product words live here: the display digest takes the app's
//! domain tag, kinds and scopes are the app's words.

mod digest;
mod error;
#[cfg(feature = "file")]
mod file;
mod memory;
mod model;
mod proof;
pub mod rules;
mod store;

pub use digest::{bind_of, new_id, sha256_hex};
pub use error::{Error, Refusal, Result};
#[cfg(feature = "file")]
pub use file::FileStore;
pub use memory::{now_ms, MemoryStore, Retention};
pub use model::{Binding, Change, Grant, Origin, Record, Request, State};
pub use proof::Proof;
pub use store::Store;
