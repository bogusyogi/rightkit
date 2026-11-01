//! Membrane: the one program the host app runs for context, memory and Blueprint queries.
//!
//! `cli` defines the command line (C4), `dispatch` runs it, `modes` holds what each command
//! does (resident first, then in process), and `serve` is the resident the host app supervises.
//! `activation` keeps the activation lock, and `native_init`
//! the repository enrollment behind `init`. Real work is delegated to `membrane_runtime`.

pub mod activation;
pub mod cli;
pub mod dispatch;
pub mod modes;
pub mod native_init;
pub mod serve;

/// Process-wide exit-code contract (README "CLI contract"). Scripts distinguish the outcomes
/// below without parsing stderr; a hook ignores all of this and always exits 0.
pub const EXIT_OK: i32 = 0;
/// An internal failure: a bug or an unreadable/corrupt store.
pub const EXIT_INTERNAL_ERROR: i32 = 1;
/// A usage error: bad arguments, empty input. Same code clap uses for a parse failure.
pub const EXIT_USER_ERROR: i32 = 2;
/// The answer is not available: no memory store, or Membrane could not produce the context or
/// Blueprint answer asked for. The reason is one line on stderr.
pub const EXIT_NOT_AVAILABLE: i32 = 3;
/// `status --require-resident` found no resident answering.
pub const EXIT_RESIDENT_UNREACHABLE: i32 = 4;

/// Top-level `"schema"` of each `--json` document: `membrane.<command>.v<version>`. A change
/// that removes or retypes a field bumps the version; adding a field does not.
pub const SCHEMA_CONTEXT_PULL: &str = "membrane.context.pull.v1";
pub const SCHEMA_BLUEPRINT: &str = "membrane.blueprint.v1";
/// `membrane blueprint search <text>` carries this id; other Blueprint verbs keep
/// `SCHEMA_BLUEPRINT`.
pub const SCHEMA_BLUEPRINT_SEARCH: &str = "membrane.blueprint.search.v1";
pub const SCHEMA_WHERE: &str = "membrane.where.v1";
pub const SCHEMA_MEMORY_LIST: &str = "membrane.memory.list.v1";
pub const SCHEMA_MEMORY_SEARCH: &str = "membrane.memory.search.v1";
pub const SCHEMA_MEMORY_DELETE: &str = "membrane.memory.delete.v1";
pub const SCHEMA_MEMORY_PUSH: &str = "membrane.memory.push.v1";
pub const SCHEMA_MEMORY_GET: &str = "membrane.memory.get.v1";
pub const SCHEMA_MEMORY_SCOPES: &str = "membrane.memory.scopes.v1";
pub const SCHEMA_MEMORY_INFO: &str = "membrane.memory.info.v1";
pub const SCHEMA_RECEIPTS_MARK: &str = "membrane.receipts.mark.v1";
pub const SCHEMA_STATUS: &str = "membrane.status.v1";
/// `membrane adapt <verb>`: one schema per verb.
pub const SCHEMA_ADAPT_OBSERVE: &str = "membrane.adapt.observe.v1";
pub const SCHEMA_ADAPT_RECALL: &str = "membrane.adapt.recall.v1";
pub const SCHEMA_ADAPT_APPLY: &str = "membrane.adapt.apply.v1";
pub const SCHEMA_ADAPT_TASTE_REVIEW: &str = "membrane.adapt.taste_review.v1";
pub const SCHEMA_ADAPT_STATUS: &str = "membrane.adapt.status.v1";
pub const SCHEMA_ADAPT_ADJUDICATE_TASTE: &str = "membrane.adapt.adjudicate_taste.v1";
pub const SCHEMA_ADAPT_MINE_SESSION: &str = "membrane.adapt.mine_session.v1";
/// `membrane transcript append`.
pub const SCHEMA_TRANSCRIPT_APPEND: &str = "membrane.transcript.append.v1";
pub const SCHEMA_TRANSCRIPT_TIP: &str = "membrane.transcript.tip.v1";

pub use rightkit_context_protocol as protocol;
pub use rightkit_context_runtime as runtime;
pub use serve::run_serve;
