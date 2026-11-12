//! `host_tools` — every capability the agent can invoke.
#![allow(
    clippy::await_holding_lock,
    clippy::derivable_impls,
    clippy::identity_op,
    clippy::inherent_to_string,
    clippy::manual_range_contains,
    clippy::map_identity,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_range_loop,
    clippy::nonminimal_bool,
    clippy::ptr_arg,
    clippy::to_string_in_format_args,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_to_owned,
    clippy::bool_assert_comparison,
    clippy::result_large_err,
    clippy::type_complexity
)]
// Without `host` the host-only tools are not registered, so the readers and helpers only they call
// are compiled but unused (the mobile runtime builds this way).
#![cfg_attr(not(feature = "host"), allow(dead_code, unused_imports))]
//!
//! All tools implement [`tool_trait::Tool`] and are registered in [`registry::ToolRegistry`].

pub mod apply_patch;
pub mod artifact_crud;
pub mod ask_user;
pub mod async_inbox;
pub mod attach_verdict;
#[cfg(feature = "host")]
pub mod background_shell;
pub mod brief_tool;
#[cfg(feature = "host")]
pub mod browser;
#[cfg(feature = "host")]
pub mod browser_consent;
pub mod browser_fetch;
#[cfg(feature = "host")]
pub mod check_runner;
pub mod cloud_endpoint;
pub mod cloud_workspace;
pub mod cloudflare;
#[cfg(feature = "code-mode")]
pub mod code_mode;
pub mod computer_use;
pub mod config_tool;
pub mod context;
pub mod context7;
pub mod deferred_resolution;
pub mod dev_tunnel_proxy;
mod durable_write;
pub mod edit_file;
pub mod edit_helpers;
pub mod effect_receipt_store;
pub mod exec;
pub mod exit_plan_mode;
pub mod file_service;
pub mod file_version_guard;
pub mod finalize_artifact;
pub mod flyio;
pub mod git_clone;
pub mod git_diff;
pub mod git_fetch;
pub mod git_history_ops;
pub mod git_lfs;
pub mod git_lfs_batch_client;
pub mod git_merge;
pub mod git_read;
pub mod git_write;
pub mod github;
pub mod handoff;
pub(crate) mod http_client;
pub mod image_generate;
pub mod list_files;
pub mod lock_recover;
#[cfg(feature = "host")]
pub mod lsp_tool;
#[cfg(feature = "host")]
pub mod managed_pty;
pub mod mcp_tools;
#[cfg(feature = "host")]
pub mod media_tools;
pub mod multi_edit;
#[cfg(feature = "host")]
pub mod no_window;
pub mod notebook_tool;
pub mod observed_version;
pub mod output_compaction;
#[cfg(feature = "host")]
pub mod plugin_tool;
#[cfg(feature = "host")]
pub mod process_launch;
#[cfg(feature = "host")]
pub mod process_resource_authority;
pub mod read_file;
pub mod recursive_fetch_tool;
pub mod registry;
#[cfg(feature = "host")]
pub mod repl_tool;
pub mod repository_root;
pub mod research_report;
#[cfg(feature = "host")]
pub mod rich_parser_worker;
#[cfg(feature = "host")]
pub mod sandbox;
#[cfg(feature = "host")]
pub mod schedule_cron_tool;
pub mod search;
pub mod send_message;
pub mod sequential_thinking;
#[cfg(feature = "host")]
pub mod shell;
pub mod skill_tool;
pub mod sleep_tool;
pub mod spawn_subagent;
pub mod synthetic_output_tool;
pub mod task_tools;
pub mod team_tools;
pub mod time;
pub mod tinyfish;
pub mod todo_write;
pub mod tool_search;
pub mod tool_trait;
pub mod tunnel;
pub mod undo_edit;
pub mod view_image;
pub mod visible_subagent_window;
pub mod wasm_compute;
pub mod web_search;
pub mod workspace_checkpoint;
#[cfg(feature = "host")]
pub mod worktree_pool;
#[cfg(feature = "host")]
pub mod worktree_tools;
pub mod write_file;

/// Tell a running language server that a file on disk changed.
///
/// Language servers are separate processes, so a host without `fork`/`exec`
/// has none to notify. Keeping the no-op behind this shim leaves the five
/// mutation tools that call it free of `#[cfg]` noise.
pub(crate) mod lsp_notify {
    use std::path::Path;

    #[cfg(feature = "host")]
    pub(crate) async fn document_mutated(workspace_root: &Path, path: &Path) {
        crate::lsp_tool::notify_document_mutated(workspace_root, path).await;
    }

    #[cfg(not(feature = "host"))]
    pub(crate) async fn document_mutated(_workspace_root: &Path, _path: &Path) {}
}

pub use cloudflare::CloudflareConfig;
pub use context::{
    CredentialPrompt, CredentialRequester, ExecutionResourceLimits, MessageDispatch,
    MessageDispatcher, PlanExit, SecretString, SpawnRuntimeKind, SubagentContinuity,
    SubagentSpawnOptions, SubagentSpawner, SubagentSupervisor, SubagentTerminalStatus,
    SubagentWaitOutcome, SubagentWorktreeOptions, TeamDefinition, TeamDirectory, ToolContext,
    UserQuestioner,
};
pub use file_service::{
    BatchEntryOutcome, BatchEntryReceipt, BatchMutationFailure, BatchMutationReceipt,
    BatchPathState, ExpectedPreimage, FileRevision, FileService, MutationBatchEntry,
    MutationConflict, MutationError, MutationIntent, MutationIntentKind, MutationReceipt,
    ReadReceipt, WorkspaceGenerationRevision, WorkspaceRestoreFailure, WorkspaceRestoreProvenance,
    WorkspaceRestoreReceipt,
};
pub use registry::{
    default_tool_capabilities, default_tool_render_classes, render_tool_capabilities_markdown,
    tool_render_class, CapabilityDescriptor, MobileToolCapabilities, ModelToolInterface,
    ToolCapability, ToolRegistry, ToolRenderClass,
};
#[cfg(feature = "host")]
pub use rightkit_sandbox::execution::ExecutionBackendRegistry;
pub use tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
pub use web_search::WebSearchConfig;
