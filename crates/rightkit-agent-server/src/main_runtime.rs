use crate::api::{self, build_router, now_iso, DaemonState, SharedStorage};
use crate::cli::{
    AppCommand, Cli, CliRuntimeCommand, Command, CompletionShell, ContextMode, DiagnosticsCommand,
    ExecutionCommand, HooksCommand, InsightsCommand, LaunchCli, LearningCommand, LocalModelCommand,
    McpCommand, OutputFormat, PermissionCommand, PluginCommand, ProviderCommand, RecipeCommand,
    RemoteCommand, ScheduleCommand, SessionCommand, SkillCommand, SpecCommand, ToolsetCommand,
};
use crate::provider_client::{self};
use crate::startup::generate_token;
use crate::{
    api_recipes, api_specs, api_toolsets, audit, doctor, evo_db, journal, learning, mcp_server,
    observation, scheduler, telegram_task, trajectory,
};
use clap::{CommandFactory, Parser};
use rightkit_agent_orchestration::registry::AgentRegistry;
use coderight_recipes::{
    build_recipe_deeplink, recipe_checksum, render_recipe, stable_recipe_id,
    validate_recipe_with_catalog, InstalledRecipe, Recipe, RecipeRun, RecipeRunStatus,
    RecipeSource, RecipeStore, RecipeVerdict,
};
use rightkit_daemon::loopback::{LoopbackRecord, RecordLabels};
use std::io::{BufRead, Write};
use std::sync::Arc;

include!("main_runtime_interactive_session.rs");
include!("main_runtime_entry.rs");
include!("main_runtime_cli_management.rs");
include!("main_runtime_mcp_catalog.rs");
include!("main_runtime_cli_controls.rs");
include!("main_runtime_diagnostics.rs");
include!("main_runtime_specs.rs");
include!("main_runtime_recipes.rs");
include!("main_runtime_recipes_plan.rs");
include!("main_runtime_remote_setup.rs");
include!("main_runtime_remote_daemon.rs");
include!("main_runtime_skills.rs");
include!("main_runtime_learning.rs");
mod headless_input {
    include!("main_runtime_input.rs");
}
include!("main_runtime_run_input_format.rs");
include!("main_runtime_run_types.rs");
include!("main_runtime_run_exit_code.rs");
include!("main_runtime_daemon_session_client.rs");
include!("main_runtime_run_session.rs");
include!("main_runtime_run_output_schema.rs");
include!("main_runtime_output.rs");
include!("main_runtime_vendor_turn.rs");
include!("main_runtime_daemon_resolve.rs");
include!("main_runtime_daemon_status.rs");
include!("main_runtime_daemon_sessions.rs");
include!("main_runtime_daemon_streams.rs");
include!("main_runtime_daemon_actions.rs");
include!("main_runtime_daemon_exports.rs");
include!("main_runtime_server.rs");
include!("main_runtime_config.rs");

#[cfg(test)]
mod tests {
    use super::*;
    include!("main_runtime_tests_1.rs");
    include!("main_runtime_tests_2.rs");
    include!("main_runtime_tests_3.rs");
    include!("main_runtime_tests_4.rs");
}
