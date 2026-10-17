//! The one request contract, independent of transport.
//!
//! A request is a method id, a [`Caller`], JSON params, and a JSON result or a
//! [`ServiceError`](crate::ServiceError). The local transport (framed JSON lines over a
//! current-user-only socket or pipe) is an adapter onto it; a later transport maps onto the same
//! types. This module holds no I/O.
//!
//! * [`Caller`] says who is asking: a local app (OS-verified peer) or a device (id and scopes).
//! * [`MethodTable`] is the single source of method policy ([`MethodPolicy`]); the local
//!   [`Allowlist`](crate::Allowlist) is derived from it ([`MethodTable::allowlist`]) and
//!   [`MethodTable::authorize`] answers for any caller.
//! * [`Handler`], [`CallContext`] and [`Emit`] are the serving side.
//! * [`status_for_code`] maps an error code to an HTTP-style status for transports that need one.

mod caller;
mod handler;
mod method;
mod status;

pub use caller::{role_of, role_scope, Caller, Scope, ROLE_SCOPE_PREFIX};
pub use handler::{CallContext, Emit, Handler};
pub use method::{validate_method_id, MethodPolicy, MethodTable, TableError, Transport};
pub use status::{status_for_code, status_for_error};

pub(crate) use handler::new_context;
pub(crate) use method::authorize_allowlist;
