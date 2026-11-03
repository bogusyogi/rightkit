//! Command line of the one `membrane` binary (C4).
//!
//! `host context|memory|blueprint ...` pass their arguments straight through to these
//! commands, and agent hooks call `membrane hook`. Nothing here keeps a process running except
//! `serve`, which the host app starts and owns.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "membrane",
    bin_name = "membrane",
    about = "Membrane: context, memory and Blueprint queries for the host app.",
    version,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Context for an agent.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Durable memory: `push`, `search`, `delete`, plus the maintenance commands.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Search or erase the indexed ChatGPT and Codex history: `search <text>`, `status`, `erase`.
    History(HistoryArgs),
    /// Adapt: observe executions, recall Taste, apply reviewed manifests, and review Taste.
    Adapt {
        #[command(subcommand)]
        command: AdaptCommand,
    },
    /// Append committed session events to a session's transcript, behind a cursor.
    Transcript {
        #[command(subcommand)]
        command: TranscriptCommand,
    },
    /// Query the Blueprint graph of the current repository.
    Blueprint(BlueprintArgs),
    /// Report what Membrane is doing.
    Status(StatusArgs),
    /// Print the path of the running `membrane` binary, with its version (`membrane.where.v1`).
    Where(WhereArgs),
    /// Answer one agent hook event: payload on stdin, reply on stdout. Always exits 0.
    Hook(HookArgs),
    /// Delivery receipts for hook context: `confirm` (from the host app's Stop hook) and `list`.
    Receipts {
        #[command(subcommand)]
        command: ReceiptsCommand,
    },
    /// Run the resident for the host app. Exits when the host app closes this process's stdin pipe.
    Serve,
    /// Enroll a repository in the registry that Blueprint and context pulls authorize against.
    Init(InitArgs),
}

#[derive(Debug, Subcommand)]
pub enum ContextCommand {
    /// Print the context Membrane would give an agent starting this task.
    Pull(PullArgs),
}

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Repository to pull context for (default: the current directory).
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// The repository, by its root path. Resolved exactly as `--cwd` is. Both flags may be given
    /// (CodeRight sends both) when they name the same path; different paths are exit 2.
    #[arg(long, alias = "repo-root")]
    pub repo: Option<PathBuf>,
    /// The task or prompt the context is for.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Context budget in tokens.
    #[arg(long)]
    pub budget: Option<usize>,
    /// Print one JSON document (`membrane.context.pull.v1`) instead of the bare context text.
    #[arg(long)]
    pub json: bool,
    /// Record the returned context as a receipt for this harness, under `--request-id`.
    #[arg(long, value_enum, requires = "request_id")]
    pub wire: Option<PullWireArg>,
    /// The caller's id for this context. `receipts mark <id> --delivered` names it later.
    #[arg(long)]
    pub request_id: Option<String>,
}

/// The harnesses that may record a receipt through `context pull --wire`. Their transcripts are
/// their own; Membrane only keeps the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PullWireArg {
    Coderight,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Store text as a durable memory, optionally named, scoped, tiered and keyworded.
    Push(PushArgs),
    /// Search memories by text, within tiers and a scope chain or a repository's scope.
    Search(SearchArgs),
    /// Delete one memory by id.
    Delete(DeleteArgs),
    /// A durable-memory maintenance command (`list`, `get`, `recall`, `put`, `export`, ...),
    /// run in process against the store: `membrane memory <command> [args...]`. `list --json`,
    /// `get <id> --json`, `scopes` and `info` answer as versioned documents.
    #[command(external_subcommand)]
    Durable(Vec<String>),
}

#[derive(Debug, Args)]
pub struct PushArgs {
    /// The text to remember.
    #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
    pub text: Option<String>,
    /// Read the text from standard input.
    #[arg(long)]
    pub stdin: bool,
    /// Name the memory: its id becomes `<scope>/<name>`, so pushing the same name in the same
    /// scope again replaces it. Without a name the id is generated.
    #[arg(long)]
    pub name: Option<String>,
    /// The scope to store it under: a repository path, a scope slug, or `global` (the default).
    #[arg(long)]
    pub scope: Option<String>,
    /// The tier to store it in (default `semantic`).
    #[arg(long, value_enum)]
    pub tier: Option<TierArg>,
    /// A retrieval keyword. Repeat the flag for several. Without any, keywords are derived from
    /// the text.
    #[arg(long = "keyword")]
    pub keywords: Vec<String>,
    /// Print one JSON document (`membrane.memory.push.v1`) with the new id.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum AdaptCommand {
    /// Record one observation: an `AdaptObservationRequestV1` JSON document (`membrane.adapt.observe.v1`).
    Observe(AdaptObserveArgs),
    /// Recall the active Taste records for a query (`membrane.adapt.recall.v1`).
    Recall(AdaptRecallArgs),
    /// Admit a reviewed manifest, or sealed Insight issues (`membrane.adapt.apply.v1`).
    Apply(AdaptApplyArgs),
    /// Build a pending Taste manifest from native mine output (`membrane.adapt.taste_review.v1`).
    TasteReview(AdaptTasteReviewArgs),
    /// Live Adapt progress for a scope (`membrane.adapt.status.v1`).
    Status(AdaptStatusArgs),
    /// Bind semantic decisions to a pending Taste manifest (`membrane.adapt.adjudicate_taste.v1`).
    AdjudicateTaste(AdaptAdjudicateTasteArgs),
    /// Mine committed session transcripts from the store, report only (`membrane.adapt.mine_session.v1`).
    MineSession(AdaptMineSessionArgs),
}

#[derive(Debug, Args)]
pub struct AdaptStatusArgs {
    /// The scope whose Adapt progress is read.
    #[arg(long, default_value = "global")]
    pub scope: String,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.status.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptAdjudicateTasteArgs {
    /// The pending manifest (`PreferenceManifestV1` JSON) from `adapt taste-review`.
    #[arg(long)]
    pub manifest: PathBuf,
    /// The semantic decisions (`SemanticAdjudicationV1` JSON).
    #[arg(long)]
    pub decisions: PathBuf,
    /// When the decisions were validated; must equal the decisions document's `validated_at`.
    #[arg(long)]
    pub validated_at: String,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.adjudicate_taste.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptMineSessionArgs {
    /// The sessions whose committed events are mined. Repeat for several.
    #[arg(long = "session", required = true)]
    pub session_ids: Vec<String>,
    /// The scope the findings are for.
    #[arg(long, default_value = "workspace")]
    pub scope: String,
    /// How many times a pattern must recur to be reported.
    #[arg(long, default_value_t = 2)]
    pub min_recurrence: u32,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.mine_session.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptObserveArgs {
    /// Read the observation document from standard input.
    #[arg(long, conflicts_with = "input")]
    pub stdin: bool,
    /// Read the observation document from this file.
    #[arg(long, conflicts_with = "stdin")]
    pub input: Option<PathBuf>,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.observe.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptRecallArgs {
    /// The text the Taste records must apply to.
    pub query: String,
    /// Most records to return.
    #[arg(short = 'k', default_value_t = 6)]
    pub k: usize,
    /// The scope to recall from.
    #[arg(long, default_value = "global")]
    pub scope: String,
    /// A scope dimension as KEY=VALUE. Repeat the flag for several.
    #[arg(long = "dimension", value_name = "KEY=VALUE")]
    pub dimensions: Vec<String>,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.recall.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptApplyArgs {
    /// A reviewed preference manifest (`PreferenceManifestV1` JSON) to admit.
    #[arg(long, conflicts_with = "insights")]
    pub manifest: Option<PathBuf>,
    /// Sealed Insight issues (a JSON array) to request admission for. Needs `--installation-id`.
    #[arg(long, conflicts_with = "manifest")]
    pub insights: Option<PathBuf>,
    /// The installation the Insights are admitted under (with `--insights`).
    #[arg(long)]
    pub installation_id: Option<String>,
    /// Validate and plan without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.apply.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AdaptTasteReviewArgs {
    /// Native mine output with Taste source bindings (`{"taste_review": {...}}` JSON).
    #[arg(long)]
    pub input: PathBuf,
    /// The installation the manifest is bound to.
    #[arg(long)]
    pub installation_id: String,
    /// The canonical pool digest to bind; default is the current pool.
    #[arg(long)]
    pub canonical_pool_sha256: Option<String>,
    /// When the manifest is created, as the caller records it.
    #[arg(long)]
    pub created_at: String,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.adapt.taste_review.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum TranscriptCommand {
    /// Append committed events at `--cursor`, the session's current end (`membrane.transcript.append.v1`).
    Append(TranscriptAppendArgs),
    /// The committed end cursor of a session's transcript (`membrane.transcript.tip.v1`).
    Tip(TranscriptTipArgs),
}

#[derive(Debug, Args)]
pub struct TranscriptTipArgs {
    /// The session whose transcript end is read.
    #[arg(long)]
    pub session: String,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.transcript.tip.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct TranscriptAppendArgs {
    /// The session whose transcript is appended to.
    #[arg(long)]
    pub session: String,
    /// The session's current end: the `expected_stream_seq` the batch extends.
    #[arg(long)]
    pub cursor: u64,
    /// The scope the rows belong to.
    #[arg(long, default_value = "global")]
    pub scope: String,
    /// Read the events (a JSON array of `SessionTranscriptEventV1` rows) from standard input.
    #[arg(long, conflicts_with = "text")]
    pub stdin: bool,
    /// The events as one JSON array of `SessionTranscriptEventV1` rows.
    #[arg(long, conflicts_with = "stdin")]
    pub text: Option<String>,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.transcript.append.v1`).
    #[arg(long)]
    pub json: bool,
}

/// A memory tier, by the name `memory search --tier` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TierArg {
    Working,
    Episodic,
    Semantic,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The text to search for.
    pub query: String,
    /// Only memories in this tier. Repeat the flag for several tiers.
    #[arg(long = "tier", value_enum)]
    pub tiers: Vec<TierArg>,
    /// Most results to return, 1 to 100.
    #[arg(long, default_value_t = 6)]
    pub limit: usize,
    /// Only the memories a repository can see: its own scope, the scopes above it, and `global`.
    /// Without `--cwd`, `--repo` or `--scope`, every scope is searched.
    #[arg(long, conflicts_with_all = ["repo", "scopes"])]
    pub cwd: Option<PathBuf>,
    /// The repository, by its root path. Resolved exactly as `--cwd` is.
    #[arg(long, alias = "repo-root", conflicts_with_all = ["cwd", "scopes"])]
    pub repo: Option<PathBuf>,
    /// A scope chain: search exactly these scopes (paths or slugs), most specific first. Repeat
    /// the flag for several. No ancestors or `global` are added.
    #[arg(long = "scope", conflicts_with_all = ["cwd", "repo"])]
    pub scopes: Vec<String>,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.memory.search.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// The memory id, as `memory search` or `memory list` printed it.
    pub id: String,
    /// The Cortex store (default: the store under the state root).
    #[arg(long)]
    pub db: Option<PathBuf>,
    /// Print one JSON document (`membrane.memory.delete.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// `search <text> [--scope S] [--limit N] --json`, `status --json`, or
    /// `erase --conversation ID | --source ID`.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub words: Vec<String>,
}

#[derive(Debug, Args)]
pub struct BlueprintArgs {
    /// A Blueprint verb with its arguments (`status`, `search <text>`, `impact <node>`, `build`,
    /// `refresh`, ...), or free text, which is searched. `build` and `refresh` publish a new graph
    /// generation for the repository.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub query: Vec<String>,
}

#[derive(Debug, Args)]
pub struct WhereArgs {
    /// Print one JSON object (`membrane.where.v1`) instead of the bare path.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print one JSON object (`membrane.status.v1`).
    #[arg(long)]
    pub json: bool,
    /// Exit 4 unless the resident answered; the status is still printed.
    #[arg(long)]
    pub require_resident: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WireArg {
    Claude,
    Codex,
}

#[derive(Debug, Args)]
pub struct HookArgs {
    /// The harness whose hook schema the reply follows.
    #[arg(long, value_enum)]
    pub wire: WireArg,
    /// The hook event, e.g. `SessionStart` or `UserPromptSubmit`.
    #[arg(long)]
    pub event: String,
}

#[derive(Debug, Subcommand)]
pub enum ReceiptsCommand {
    /// Mark emitted context `delivered` when its marker is in the host transcript. Prints
    /// nothing and always exits 0.
    Confirm(ConfirmArgs),
    /// List the context receipts.
    List(ReceiptsListArgs),
    /// Mark the receipt `<id>` delivered, for a caller that injected the context itself.
    Mark(MarkArgs),
}

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// The `--request-id` the context was pulled with.
    pub id: String,
    /// Required: the only change `mark` makes. An `emitted` receipt becomes `delivered`; any
    /// other state is left as it is.
    #[arg(long, required = true)]
    pub delivered: bool,
    /// Print one JSON document (`membrane.receipts.mark.v1`).
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ConfirmArgs {
    /// The harness whose transcript this is.
    #[arg(long, value_enum)]
    pub wire: WireArg,
    /// The session whose receipts are confirmed.
    #[arg(long)]
    pub session: String,
    /// The host's record of the session: the Claude transcript or the Codex rollout (JSONL).
    #[arg(long)]
    pub transcript: PathBuf,
}

#[derive(Debug, Args)]
pub struct ReceiptsListArgs {
    /// Only this session's receipts.
    #[arg(long)]
    pub session: Option<String>,
    /// Print one JSON array.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// The repository root to enroll.
    pub root: PathBuf,
    #[arg(long)]
    pub repository: String,
    #[arg(long)]
    pub scope: String,
    /// Move an existing exact registry binding from this old root to `root`.
    #[arg(long)]
    pub rebind_from: Option<PathBuf>,
    #[arg(long)]
    pub virtual_id: Option<String>,
    #[arg(long)]
    pub tenant_id: Option<String>,
    #[arg(long)]
    pub parent: Vec<String>,
    #[arg(long)]
    pub native: Option<String>,
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Show what would change without writing the registry.
    #[arg(long)]
    pub dry_run: bool,
}

/// Parse a command line. Argument zero is the program name.
pub fn parse<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    Cli::try_parse_from(args)
}
