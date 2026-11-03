use rightkit_agent_host_traits::skill_host::{CommandAction, CommandResult};
use std::path::PathBuf;

use crate::api::{discover_instruction_files, load_workspace_task_records, workspace_tasks_path};
use crate::api::{lock_recover, DaemonState};
use crate::api_benchmarks::{
    benchmark_history_json, benchmark_proof_json, run_terminal_bench_task, TerminalBenchRunRequest,
};
use crate::api_command_mcp::configured_mcp_server_count;
use crate::api_plugins::{load_plugin_marketplace, plugin_manifests_with_roots};
use crate::api_schedules::{
    load_api_schedule_runs, load_api_schedules, schedule_runs_for, schedules_store_path,
};
use crate::api_skills::render_skills_command;

include!("api_command_renderers_runtime.rs");
include!("api_command_renderers_schedules.rs");
include!("api_command_renderers_specs_dispatch.rs");
include!("api_command_renderers_specs_core.rs");
include!("api_command_renderers_specs_plan.rs");
include!("api_command_renderers_specs_helpers.rs");
include!("api_command_renderers_skills_plugins.rs");
include!("api_command_renderers_shared.rs");
