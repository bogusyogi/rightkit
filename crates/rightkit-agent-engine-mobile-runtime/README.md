# rightkit-agent-engine-mobile-runtime

Agent engine mobile runtime: in-process engine host for platforms without fork/exec (iOS), with journal, FFI-friendly session API and portable tool profile

Moved into RightKit from a private mobile runtime crate on 2026-11-01; 0.1.2 carries the rest of that crate's behaviour behind host options.

## Features

| Feature | Adds |
|---|---|
| `os-keyring` | The OS credential store for provider, router and MCP secrets (`rightkit-llm`, `rightkit-mcp`, `rightkit-agent-engine-core`). Off by default: with no feature the dependency tree holds no `rightkit-credentials` and no `keyring` |
| `c-abi` (default) | The crate's own `host_mobile_*` C symbols, each a one-line wrapper around the Rust function of `ffi`, `team_fleet` or `wasm` named without the prefix. A host static library that exports its own symbol names depends on this crate with `default-features = false` and wraps the Rust functions itself |

## Host start-up

A host names itself to `rightkit-agent-config` (`set_app_name`, `set_state_dir_name`, `set_env_prefix`) and sets the once-per-process options of `host_options` (and of the engine and tools crates) before its first runtime, git request or capability query. The crate sets none of them.

| Option | Turns on |
|---|---|
| `set_interactive_tools` | `ask_user` and `exit_plan_mode`, answered through `resolve_question` / `resolve_plan` |
| `set_subagents` | `spawn_subagent` (one in-process child at a time) |
| `set_git_loopback_fixture` | the simulator-only `loopbackBase` of a git request |
| `set_small_model_structured_output` | JSON Schema answers for the small-model assists |
| `set_browser_control_capability` | `browserControl` in the capability list (unavailable, with the reason) |
