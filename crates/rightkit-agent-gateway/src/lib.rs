//! `rightkit-agent-gateway`: the core every local agent gateway runs on, with no product verbs.
//!
//! A gateway lets a caller that is not the user (a paired CLI or MCP agent, a model service
//! behind a tunnel) call an app's tools. Whatever the tools are, the gate around them is the
//! same, and this crate holds it once:
//!
//! * [`origin`]: the door a call came in by ([`Origin`]), set by the process that received it,
//!   never by the request.
//! * [`registry`] and [`pairing`] (feature `sql`): several named agents, each with a pairing key
//!   stored only as a SHA-256 hash, a permission [`gate::Level`] and a scope list (the accounts,
//!   folders or projects it may touch), revocable at once.
//! * [`gate`]: the pipeline's checks in one fixed order (caller known, app unlocked, method
//!   known, level high enough, scope allowed, door allows changes) over a static method table,
//!   and [`gate::conclude`], which writes exactly one audit row per call.
//! * [`audit`]: the row ([`audit::Entry`]) and the sink trait ([`audit::AuditSink`]); a SQL
//!   sink over the host's `agent_audit` table (feature `sql`). An app with a log file
//!   implements the trait over it.
//! * [`Refusal`] with stable codes (the approvals core's type) and [`gate::Denied`], the gate's
//!   own refusals.
//! * [`content`]: text a third party wrote (a mail sender, another AI) goes out wrapped or
//!   stamped as untrusted, capped.
//! * [`redact`]: masks secrets in text before it leaves this computer, and the leak check a
//!   gateway runs as its release gate.
//!
//! Pending approvals are the `rightkit-approvals` core's. The tools are the adapters':
//! `rightkit-mail-gateway` (mail verbs over this core) and `rightkit-remote-gateway` (files,
//! terminals, screen and the rest for one owner).

#[cfg(feature = "sql")]
pub mod access;
pub mod audit;
pub mod content;
pub mod gate;
pub mod origin;
#[cfg(feature = "sql")]
pub mod pairing;
pub mod redact;
#[cfg(feature = "sql")]
pub mod registry;

pub use gate::{Denied, Level, Method};
pub use origin::Origin;
/// The approvals core's refusal: one type, with a stable `code`, for every gateway refusal.
pub use rightkit_approvals::{Refusal, Result};

/// Milliseconds since the Unix epoch, 0 if the clock reads before it.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
