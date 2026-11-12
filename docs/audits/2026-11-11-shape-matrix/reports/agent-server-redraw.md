# rightkit-agent-server: host-supplied names and data out, redraw note

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l` (work branch `redraw-agent-server`). Row: `rightkit-agent-server` (`../matrix.md`, `family-agent-platform.md`, `agent-platform-redraw.md` section B). Row for this note: `agent-server-redraw.rows.json`. Taken on the recommended option of the shape review, pending the owner's confirmation. Nothing here is on `main`.

The owner's goal: every app moves most of its code into RightKit, each package in its best shape, no duplicate code. CodeRight, the app this crate came from, is still to be decided, so a second app must be able to adopt the crate without CodeRight's product decisions. The previous redraw (`agent-platform-redraw.md`) deleted the crate's private copies of `rightkit-github`, `rightkit-scheduler`, the host observation shapes and the Telegram transport. Its deferred list is the scope here; the strings and the data come first because they decide whether a second app can adopt the crate.

## A. Product names come from the host

### What existed

About 530 `coderight` strings in `src/` (508 matched lines): about 150 in comments and help text, the rest in code. The code ones were product decisions written into the library:

| Kind | Examples |
|---|---|
| State and config paths | `.coderight/` in the workspace and the home directory (schedules, notifications, worktrees, skills, hooks, commands, plugins, constitution, understanding, sandbox settings, team and permission stores), `~/.config/coderight`, `coderight.db` |
| Wire ids and labels | `coderight.context-status.v1`, `coderight.error.v1`, `coderight.cli.*`, `coderight.session_handoff.v1`, `coderight.trigger/v1`, `coderight:cron`, `coderight-daemon`, `coderight_event_v1` |
| HTTP headers | `x-coderight-capability-token`, `x-coderight-driver-instance`, `x-coderight-client-id`, `x-coderight-authenticated`, the generic webhook's `x-coderight-signature-256`, `-delivery-id`, `-event` |
| Hash domains | inference selection, foreground memory cursor, background provider authority, trigger event and content dedupe, bake-off corpus secret |
| Identity | licence app id `EXPECTED_APP = "coderight"`, Github sign-in credential namespace, the Github App client id, the OpenRouter `HTTP-Referer` (CodeRight's repository URL), the MCP server name and the `mcp__coderight__` tool prefix |
| Messages | the clap command name, `coderight serve` / `coderight remote setup ...` hints, `[coderight]` log tag, `coderight> ` prompt, thread and temp-folder names |

### The shape now

New public module `host_identity`: `HostIdentity` and `HarnessRadarData`, re-exported at the crate root. `run(identity, skill_catalog, skill_host)` installs it first, the way it already installed the skill catalog and skill host. Every name derives from the app name unless set:

| Name | Default for `HostIdentity::new("myapp")` | Setter |
|---|---|---|
| command name, log tag, MCP server, schema and label prefix, header segment, hash domain prefix | `myapp` | (the name) |
| state folder | `.myapp` | `with_home_dir` |
| config folder | `~/.config/myapp` | `with_config_dir` |
| licence app id | `myapp` | `with_license_app` |
| credential namespace | `myapp` | `with_credential_namespace` |
| Github App client id | none (sign-in reports "not configured") | `with_github_client_id` |
| provider referer | none (no `HTTP-Referer`) | `with_homepage` |
| harness-radar watch-list | none (empty feed) | `with_harness_radar` |

`HostIdentity::new` refuses a name that is not a lowercase slug, because the name becomes a command, a folder and a header segment. Before `run` (or in a library use without it) the neutral name `agent-server` applies. Inside the crate small helpers build the names (`schema`, `header`, `prefixed`, `command`, `domain`); tables that need `&'static str` take an interned copy.

CodeRight's values live in the app: it passes `HostIdentity::new("coderight")`, its client id (the constant formerly in `src/github_signin.rs`), its homepage, its watch-list files (its own `assets/harness-radar/`) and the `benchmarks` feature. With that it gets byte-identical paths, ids, headers and digests to 0.1.1, except the tracing targets (`agent_server.workflow.*`, `agent_server.router_seam`: tracing needs a constant) and an internal no-tools sentinel. The CodeRight repository is not in this workspace, so that call is not written here; the CHANGELOG lists it.

Comments and help text say `{app}` for the app name (151 lines).

Left, owned by other crates (4 lines): `coderight_agent_id` is a field of `rightkit-agents`' driver binding record and a column in `rightkit-agent-engine-storage`'s migrations; `metadata.coderight` is `rightkit-agent-host-traits`' skill manifest section (`CodeRightSkillMetadata`, a file format). Renaming them is a breaking change in those crates and a migration.

## B. Benchmarks and data out of the library

| Item | Before | Now |
|---|---|---|
| `data/assets__harness-radar__*` (CodeRight's watch-list) | compiled in with `include_str!` | deleted; host-supplied through `with_harness_radar`; the feed code stays, it is generic watch-list parsing and diffing |
| harness-radar refresh prompt | named CodeRight's repository paths and "RightKit-relevant" | names the workspace's own watch-list |
| `data/engine__bins__coderight__tests__fixtures__cli-json__*` (6), `data/engine__fixtures__bots__bot-profile-v1.json` | unused since the tests were removed | deleted |
| `data/engine__crates__engine__src__steering.rs` | source kept under `data/` | `src/api_sessions/steering.rs` |
| `data/engine__fixtures__role-packs__*` | | `data/role-packs/*.v1.toml` (built-in role packs, generic) |
| `api_benchmarks` (4.1k lines: role benchmark and promotion, compare and judge, terminal-bench task pack, executed and corpus bake-offs, soak) | always built | feature `benchmarks`, off by default; without it a 70-line stand-in answers: no routes, no OpenAPI entries, `/benchmarks` empty, terminal-bench runs 501, a skill proposal's role benchmark scored but not recorded |

Why a feature and not a bench crate yet: the harness drives live sessions through `DaemonState` (`create_session_with_options`, `LiveSession`, `SessionRuntimeOptions`, the session map, storage, the agent registry) and is called from skill governance and the slash commands. A separate crate needs a public session-hosting API on `DaemonState`; making `ApiState` public today exposes every private field type. With the feature, a second app adopts the crate without the harness, and the move to `rightkit-agent-bench` is a listed step once that API exists.

`api_specs` (2.9k lines) stays: it is the spec workflow (feature specs, planning, evidence, constitution) wired into session bindings, `ApiState`'s spec store, the CLI and the slash commands, a coding-agent feature rather than CodeRight data, and its strings are now host-supplied. Whether spec-driven work belongs to the generic server or to the app is a product decision for the owner.

## C. Not done (listed)

1. **Scheduler runner into `rightkit-scheduler`** (execution, delivery, lifecycle, persistence; 8 of its files call `crate::api*`): it runs on `DaemonState`, the schedule stores in `api_schedules` and the toolsets. The move needs a runner trait in `rightkit-scheduler` (load/save schedules and runs, start an agent turn, deliver) implemented by the server. Not started.
2. **Workflow host into its own crate** (`src/workflow`, 7.2k lines with the scheduler 9.8k together): it calls `crate::api` 13 times and `crate::scheduler` 7 times; the daemon routes it calls must be split out first. Cannot go into `rightkit-workflow` (engine-storage depends on workflow). Not started.
3. **The ~30k lines the plan assigns elsewhere**: 11.7k engine (`rightkit-agent-engine-*`), 6.8k agents, 4.6k llm, 3.7k orchestration, 2.6k sandbox, 1.8k evidence. Not moved: the planned lines are spread over the `api_*`, `main_runtime_*` and `tui_host` files that hold `DaemonState` handlers, so no module is both contained and clearly owned by an existing crate without first splitting handlers from logic. The one contained candidate, the steering queue (`src/api_sessions/steering.rs`, 150 lines, std and serde only), belongs to the engine core and can move with the next engine-core release.
4. **`api_benchmarks` to `rightkit-agent-bench`** once `DaemonState` has a public session-hosting API (see B).
5. **The four field names** owned by `rightkit-agents`, `rightkit-agent-engine-storage` and `rightkit-agent-host-traits` (see A).
6. **The split the review asked for** (a small generic server: session hosting, daemon, API router, permission and session routes; product routes in the app): after A and B no product name or product data is left, so what remains is size and placement, not a product dependency.

## Verdict

`gaps`: a library a second app can adopt with its own `HostIdentity` and without the benchmark harness; remaining moves listed in C.

## Versions and checks

| Crate | Version | Note |
|---|---|---|
| `rightkit-agent-server` | 0.1.2 (open, absorbed; breaking: `run` takes a `HostIdentity`) | recipe PK172 updated: builds a second app's identity |

No other crate changed. No dependency changed, so `Cargo.lock` and the e2e-app lock are unchanged.

Checks run on the work branch (toolchain 1.99.1): `cargo clippy -p rightkit-agent-server --all-features -- -D warnings` and with default features, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`, `cargo fmt --check`, `cargo check` with default features (the crate has no dependents), `cargo metadata --locked`, the PK172 recipe run against the path dependency in a scratch directory, `node scripts/preflight.mjs --worktree`.
