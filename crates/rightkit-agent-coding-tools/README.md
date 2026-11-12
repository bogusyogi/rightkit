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
| `host` | Browser, OCR, media, plugin and PTY tools, the OS keyring and host-environment overrides (implies `code-mode`) |
| `code-mode` | The QuickJS code-mode tool |
| `embeddings` | Local embedding inference |
| `test-support` | Test doubles for dependents |

Moved from `rightkit-agent-tools::coding_tools` (see CHANGELOG).

License: MIT OR Apache-2.0.
