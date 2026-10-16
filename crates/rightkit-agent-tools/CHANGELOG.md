# Changelog

## 0.1.3

- `batch` and `wait` control methods (rightkit-qa-driver 0.4): one call for many steps, and one call that waits in the page. A batch is checked step by step against the app profile, so a sensitive app (HeardRight) cannot reach `eval`, `dom`, `ax` or `screenshot` through it; `wait` and `batch` count as generic read channels. `screenshot` accepts `inline`.
- One connection per app profile is reused across tool calls (it was one connect and hello per call).
- README: driving an app is documented as one `right-ctl run` or `batch` call, not one command per step.

## 0.1.2

First published release. Earlier 0.1.x versions were internal to the repository and never reached crates.io.

- One typed `Toolbox` for agents: named tools with JSON argument schemas and an effect class, every call passing one gate (hard exposure rules, per-caller allowlist, argument validation, admission hook, journal).
- Tool families: `<app>.*` over each app's in-app control service through `rightkit-daemon`; `desktop.*` over `rightkit-automation` (feature `automation`); browser and speech tools (features `browser`, `speech`).
- The `right-ctl <app> <command>` CLI.
- MCP hosting removed; the agent surface is the CLI contract.
