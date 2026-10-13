# Changelog

## 0.1.2

First published release. Earlier 0.1.x versions were internal to the repository and never reached crates.io.

- One typed `Toolbox` for agents: named tools with JSON argument schemas and an effect class, every call passing one gate (hard exposure rules, per-caller allowlist, argument validation, admission hook, journal).
- Tool families: `<app>.*` over each app's in-app control service through `rightkit-daemon`; `desktop.*` over `rightkit-automation` (feature `automation`); browser and speech tools (features `browser`, `speech`); optional MCP hosting (feature `mcp`).
- The `right-ctl <app> <command>` CLI.
