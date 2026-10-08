# agent-room-core

Agent Room core: the state model and local host for a shared review room in which agent seats and a human seat exchange events, findings, dispositions and seal receipts. The crate is not published (`publish = false`, `license = "UNLICENSED"`).

## What it owns

Catalog capability: `agent-room`.

| Module | Owns |
|---|---|
| `protocol` | Room phases, event kinds, seat config and budgets, findings, dispositions, contests, receipts, and the `Room` state machine. |
| `store` | `RoomStore`: the SQLite-backed room store (`rusqlite`, bundled), with export and prune receipts. |
| `auth` | Seat join and claim. `AUTHORITY_ROLES` (`human`, `moderator`, `implementer`) require a host-minted, seat-bound bootstrap token. Other roles claim without one. |
| `runtime` | `RuntimeInfo` discovery file (`runtime.json`) and owner-only file checks. Tokens never appear in the base URL. |
| `server` | `ServerState` and `router()`: an axum HTTP host with JSON endpoints, SSE streams and a dashboard. |
| `bin/room` | The `room` CLI: `spawn`, `watch`, `say`, `abort`, `export`, `prune`. |

## When to use it

- Use it to run a multi-seat review room whose phases, findings and seals must persist in one local SQLite file.
- Use it when a human seat must hold authority that agent seats cannot claim without a token.

Do not use it for general chat, for hosted or multi-machine rooms, or as a product UI. The crate has no remote transport beyond a local HTTP host.

## Cargo features

- `http` (default): the `server` module (axum HTTP and SSE host) and the `room` bin. Library-only users
  opt out with `agent-room-core = { ..., default-features = false }`; that drops axum, tokio and
  tokio-stream from the dependency tree.

## Usage

CLI, taken from `tests/p0_cli.rs`:

```sh
room spawn --run-dir <run-dir> --room-id cli-smoke --workspace <workspace> --seats claude,codex
```

Library, taken from `tests/p0_runtime.rs`:

```rust
use agent_room_core::runtime::{load_runtime, write_runtime, RuntimeInfo};

let path = write_runtime(run_dir, &info)?;   // writes runtime.json, refuses overwrite
let loaded = load_runtime(run_dir)?;         // round-trips RuntimeInfo
```

## Platform support

Rust and SQLite are portable, but the crate has not been verified off macOS. The `room` binary opens the dashboard with the macOS `open` command (`src/bin/room.rs`).

## Version and changes

Published version: not published. See [INDEX.md](../../INDEX.md) for the workspace version list. Version history: [CHANGELOG.md](CHANGELOG.md).
