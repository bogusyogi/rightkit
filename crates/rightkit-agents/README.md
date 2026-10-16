# rightkit-agents

Supervised vendor agent sessions for Right Suite apps: one event model, the wire clients for the vendor CLIs the user has installed and signed in to, and (feature `session`) a blocking session that spawns or attaches to Claude Code or Codex, streams typed events, surfaces tool-approval requests for the host to approve or deny, cancels a turn, and tears the whole process tree down through `rightkit-child-process`. Rust speaks the CLI protocols directly. There is no SDK, no HTTP to vendor agent services, no MCP and no Node sidecar.

Catalog capability: `agent-sessions`. Release versions are tracked in [INDEX.md](../../INDEX.md) and `CHANGELOG.md`.

## What it owns

| Piece | Owns |
|---|---|
| `AgentEvent` | The vendor-neutral event stream: init, text, thinking, tool use and result, subagent start and finish, approval request and resolution, usage, compaction, result (cost, turns, permission denials, error subtype), stderr, unknown. |
| `claude` (feature) | Claude Code `claude -p --input-format stream-json --output-format stream-json` line parser (`StreamParser`), stdin frames (`user_turn_frame`, `interrupt_frame`), argv (`build_args`, `ConfigMode::Full` or `Bare`). |
| `codex` (feature) | Codex `codex app-server` JSON-RPC over stdio: `parse_line`, blocking `CodexClient` (`initialize`, `thread_start`, `thread_resume`, `turn_start`, `turn_interrupt`, `respond_approval`), schema pinned per CLI version. |
| `session` (feature) | `Session::spawn(SessionConfig)` over either backend: `events()` (`SessionEvent`), `send_user_message`, `approve` / `deny` (`ApprovalDecision`), `cancel`, `shutdown`. Attach by setting `resume_session_id` (Claude) or `resume_thread_id` (Codex). |
| fixtures and tests | Recorded transcripts, the Codex 0.161.0 schema snapshot, a test-only fake CLI, and the scope invariant test. |

## What it does not own

- One-shot model calls and BYOK HTTP clients: `rightkit-llm`.
- Process spawning and tree ownership: `rightkit-child-process` (used by `session`). Working-directory pinning and job records: `rightkit-fs`, `rightkit-job-queue` (not wired here).
- Send, publish and spend decisions: the owning app, through `rightkit-agent-tools` effects. The vendor permission mode is defence in depth only.
- Provider registries, profiles and editor protocols (ACP): the apps.

## The three backends

| Backend | Status in 0.1.0 |
|---|---|
| `claude` | Parser, writer, argv. Full user config for CodeRight, `--bare` for Pulse, as options; no behaviour yet. |
| `codex` | `app-server` client. Experimental upstream: the schema is pinned to codex-cli 0.161.0 and a drift test compares it to what the client handles. |
| `api` (later) | Tool loop over `rightkit-llm`. Not in this release. |

## Hard rules

- No HTTP client, async runtime, vendor SDK, MCP or Node dependency. `tests/scope.rs` fails the build if one appears.
- Only the user's own installed, signed-in CLI. Tokens are never read, copied or proxied. Anything sold runs on the `api` backend with the customer's key.
- Unknown vendor messages degrade to `AgentEvent::Unknown`; they never fail parsing.
- No dev override ships: the fake CLI exists only in test targets.
- Default features are empty. Enable `claude` and `codex` as needed.

## Usage

```rust
use rightkit_agents::claude::{StreamParser, user_turn_frame};

let mut parser = StreamParser::new();
let events = parser.parse_line(r#"{"type":"system","subtype":"init","session_id":"s1"}"#)?;
let frame = user_turn_frame("summarise the diff"); // write to the child's stdin
```
