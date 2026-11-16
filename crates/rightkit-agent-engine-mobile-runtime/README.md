# rightkit-agent-engine-mobile-runtime

Agent engine mobile runtime: in-process engine host for platforms without fork/exec (iOS), with journal, FFI-friendly session API and portable tool profile

Moved into RightKit from `coderight-mobile-runtime` (CodeRight) on 2026-11-01.

## Features

| Feature | Adds |
|---|---|
| `os-keyring` | The OS credential store for provider, router and MCP secrets (`rightkit-llm`, `rightkit-mcp`, `rightkit-agent-engine-core`). Off by default: with no feature the dependency tree holds no `rightkit-credentials` and no `keyring` |
