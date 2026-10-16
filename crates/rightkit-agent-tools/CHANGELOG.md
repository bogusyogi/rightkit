# Changelog

## Unreleased

- Agent demo contract (feature `demo`, non-default): `demo.request`, `demo.apps`, `demo.snapshot`, `demo.act`, `demo.batch` and `demo.end` as typed client tools over the host service `rightkit.agent.demo`. Per-session token, typed error codes, and a 20-step batch limit. The wire contract is in `docs/demo-contract.md`. The version bump waits for the owner's release.
- `right-ctl demo <tool> [args]` (feature `demo`). A default or release build contains no demo code.

## 0.1.5

- Added: `CallContextExt::peer_executable()` on `rightkit_daemon::CallContext`, returning the verified executable of the calling process (`Option<PathBuf>`), for a consent prompt that must show which program is asking. It runs the transport's own peer check (`CallContext::verify_peer_executable`: pid and start time recorded at accept, connection still open) and reads the executable the OS found; it replaces probing with an impossible path and reading `found_executable` from the error. `None` fails closed (a device caller, an unreadable or changed process, an unsupported platform). A fact for display and audit, not an authorisation. Needs `rightkit-daemon` 0.2.4 (`verify_peer_executable`), so the dependency floor is now 0.2.4.

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
