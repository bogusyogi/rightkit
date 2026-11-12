//! CLI surface for the `{app}` binary.

use clap::{Parser, Subcommand, ValueEnum};

include!("cli_definitions_command.rs");
include!("cli_definitions_launch.rs");
include!("cli_definitions_mcp_plugins.rs");
include!("cli_definitions_execution_session.rs");
include!("cli_definitions_remote_recipe.rs");
include!("cli_definitions_schedule_spec.rs");
include!("cli_definitions_skills_learning_diag.rs");

#[path = "command_registry.rs"]
pub mod command_registry;

pub(crate) use cli_definitions_command_run::{
    warn_deprecated_context_mode_flag, CliColor, ContextMode, OutputFormat,
};
