//! `rightkit-agent-gateway`: the local agent gateway of a mail-style app.
//!
//! An agent (a local CLI or MCP client) pairs with the app, gets a read-only profile and can be
//! widened to drafts and to "send with approval". Every request goes through one gate
//! ([`gate::handle_full`]): key, lock, profile, account, rate limits, then a read handler, a
//! draft or forward creator, a label mutation or an attachment export, and one audit row.
//!
//! Nothing an agent can call sends, forwards or deletes mail: `send`, `forward`, `delete` and
//! `attach` only create a pending approval ([`approval_store`]) that the user acts on in the app
//! window ([`approval::approve`]). Text written by an email sender is wrapped as untrusted
//! ([`content`]).
//!
//! The crate owns the `agents`, `agent_accounts`, `agent_audit` and approval tables' SQL, not
//! their schema or migrations: the host creates them and gives the gateway its connections
//! through [`access::ConnAccess`]. Everything else it needs from the host (the mutation guard,
//! the local mail mirror, settings, the file scanner, the product name) is [`host::GatewayHost`],
//! installed once with [`host::set_host`]. Gmail calls are the host's too, behind the
//! [`draft::DraftCreator`], [`approval::DraftMailer`] and [`approval_trash::ThreadTrasher`].

pub mod access;
pub mod approval;
pub mod approval_attach;
pub mod approval_kind;
pub mod approval_request;
pub mod approval_store;
pub mod approval_trash;
pub mod attach_open;
pub mod attach_path;
pub mod attachment;
pub mod audit;
pub mod content;
pub mod draft;
pub mod draft_files;
mod draft_open;
pub mod draft_view;
pub mod export;
pub mod forward;
pub mod gate;
pub mod gate_effects;
pub mod gate_forward;
pub mod host;
pub mod import_file;
pub mod mime;
pub mod mutate;
pub mod mutate_local;
pub mod outcome;
pub mod pairing;
pub mod read_handlers;
pub mod redact;
pub mod registry;
pub mod send_request;

pub use outcome::{AgentError, Profile, Request};
