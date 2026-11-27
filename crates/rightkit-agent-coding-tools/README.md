# rightkit-agent-coding-tools

The coding agent's host tools: file read, write, edit and patch, shell and PTY, background shell, git, browser, MCP sources, LSP, tasks, scheduling, checkpoints, worktrees and sandboxing. They sit behind an async `Tool` trait (`tool_trait::Tool`) and are registered in `registry::ToolRegistry`.

This is a different surface from `rightkit-agent-tools`, which is the synchronous tool contract, gate and `right-ctl` CLI every app uses over its control service. The two do not depend on each other.

```toml
rightkit-agent-coding-tools = "0.1"
```

```rust
use rightkit_agent_coding_tools::registry::ToolRegistry;
```

## Features

| Feature | Adds |
|---|---|
| `os-keyring` | The OS credential store for the MCP and provider secrets (`rightkit-mcp` `agent_mcp-os-keyring`, `rightkit-llm` `agent_providers-os-keyring`). Off by default |
| `host` | Browser, OCR, media, plugin and PTY tools (the PTY half of `rightkit-sandbox` and `portable-pty`) and host-environment overrides (implies `code-mode` and `os-keyring`). Without it the crate links neither `rightkit-pty` nor `portable-pty`, and still scales images when the host option `images_downscaled` is on |
| `code-mode` | The QuickJS code-mode tool |
| `embeddings` | Local embedding inference |
| `test-support` | Test doubles for dependents |

A host that must not let the environment choose a program, an endpoint, a data folder or weaken a control turns the read off once at start-up: `tool_options::{set_node_env_disabled, set_browser_no_sandbox_env_disabled, set_context7_api_base_env_disabled, set_web_search_url_env_disabled, set_skills_dir_env_disabled, set_resource_profile_env_disabled, set_workspace_trust_store_env_disabled, set_send_message_fixed_dir}` and `shell::set_sandbox_tier_env_disabled`. The variable is then not read at all. Each setter returns `false` when a choice was already made. See CHANGELOG 0.2.1 for every environment name the crate can read.

Moved from `rightkit-agent-tools::coding_tools` (see CHANGELOG).

License: MIT OR Apache-2.0.
