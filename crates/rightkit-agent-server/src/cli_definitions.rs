//! CLI surface for the `coderight` binary.

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

#[cfg(test)]
mod tests {
    use super::*;
    include!("cli_definitions_tests_1.rs");
    include!("cli_definitions_tests_2.rs");
    include!("cli_definitions_tests_3.rs");
    include!("cli_definitions_tests_4.rs");
    include!("cli_definitions_tests_5.rs");
    include!("cli_definitions_tests_6.rs");
    include!("cli_definitions_tests_7.rs");
    include!("cli_definitions_tests_8.rs");
    include!("cli_definitions_tests_9.rs");
    include!("cli_definitions_tests_doctor_flag_parse.rs");
    include!("cli_definitions_tests_launch.rs");
}
