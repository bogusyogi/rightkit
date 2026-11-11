# Agent platform: gateway core and agent-server duplicates, redraw note

Date: 2026-11-11. Branch `claude/busy-hypatia-e30h7l` (work branch `redraw-agent-platform`). Rows: `rightkit-agent-gateway`, `rightkit-remote-gateway`, `rightkit-agent-server` (`../matrix.md`, `family-agent-platform.md`), the owners whose duplicates were deleted (`rightkit-github`, `rightkit-scheduler`, `rightkit-bot-messaging`, `rightkit-owner-approvals`), and the new `rightkit-mail-gateway`. Rows for this note: `agent-platform-redraw.rows.json`. Taken on the recommended option of the shape review, pending the owner's confirmation. Nothing here is on `main`.

The owner's goal is that every app moves most of its code into RightKit with no duplicate code, each package in its best shape; reference order HeardRight, ViewRight, then Pulse, then CodeRight, MailRight and the rest. The approvals core (`approvals-core-design.md`) and the engine contract (`agent-engine-contract-design.md`) had already landed; this note builds on both.

## A. One gateway core, two adapters

### What existed

`rightkit-agent-gateway` 0.2.0 was MailRight's mail gateway (depends on `rightkit-mail`, methods are mail verbs). Pulse's gateway is `rightkit-remote-gateway`. The common part existed twice with no shared code:

| Piece | MailRight (`rightkit-agent-gateway` 0.2.0) | Pulse (`rightkit-remote-gateway` 0.1.3) |
|---|---|---|
| Caller | SQL registry of paired agents, hashed key, profile, account list, revoke (`registry.rs`, `pairing.rs`) | the door (`Origin` in `lib.rs`), a client label |
| Gate | key, lock, method table, profile, account, handler, one audit row; no row, no result (`gate.rs`) | manifest, settings, gateway switch, read-only door, unlock, tool; activity line best effort (`lib.rs` `run`, `execute_args`) |
| Refusal | `AgentError` with `code()` | `Refusal` (the approvals core's) |
| Audit | `agent_audit` table (`audit.rs`) | `remote-activity.jsonl`, a hand-built line (`audit.rs`) |
| Untrusted text | JSON wrapper, source `email` (`content.rs`) | a stamp line for what ChatGPT sends (`gptchat.rs`, `chat_start.rs`) |
| Redaction | a leak check (`redact.rs`, release gate) | masking, in `rightkit-owner-approvals/src/redact.rs` |
| Pending approvals | the approvals core (`SqlStore`) | the approvals core (`MemoryStore`) |

### The shape now

* **`rightkit-agent-gateway` 0.3.0: the core, no product verbs.** The name now says what it holds. Modules: `origin` (`Origin`, Pulse's door, moved), `gate` (a `Level` trait for permission levels; a static `Method` table row with name, minimum level, scoped, changes; the ordered checks `lookup`, `allow_level`, `allow_scope`, `allow_door`; `Denied` with stable codes; `conclude` writes one audit row per call with `Logging::Required` (MailRight's rule) or `Logging::BestEffort` (Pulse's)), `audit` (`Entry` with door and structured non-content fields; `AuditSink` trait; `SqlAudit` over the host's `agent_audit`), `registry` and `pairing` (MailRight's, generic over the level: several agents, each with a level and a scope list, the more general registry), `access` (`ConnAccess`), `content` (wrapper with the source as a parameter, `Budget`, `stamped`), `redact` (Pulse's masking plus MailRight's leak check: one module). `Refusal` is the approvals core's, re-exported. Feature `sql` (default) carries the registry, pairing, `access` and `SqlAudit`; without it the core has no database dependency (Pulse uses it that way).
* **`rightkit-mail-gateway` 0.1.0 (new, PK230): the mail adapter.** Every mail verb, the mail approval policy (`approval_store::SqlStore`, windows, caps, cooldown), drafts, forward, attachments and the `GatewayHost` seam, moved with `git mv` from the old crate. `Profile` implements `gate::Level`; the gate runs `lookup`, `allow_level`, `allow_scope` over a `Method` table and `conclude(Logging::Required)`; `Denied` maps to `AgentError` so the agent wire codes are unchanged (`bad_request` for an unknown method, `account_not_allowed`). `registry` and `pairing` are thin wrappers at the mail levels, keeping the Settings wording.
* **`rightkit-remote-gateway` 0.1.3 on the same core** (without `sql`): `Origin` and `redact` are the core's (re-exported under the old paths); the read-only door is `gate::allow_door` (code `read_only_tool` unchanged); the activity line goes through `audit::AuditSink` (`audit::ActivityFile`, same JSON) and `gate::conclude(Logging::BestEffort)`; the ChatGPT stamp is `content::stamped`.
* **`rightkit-owner-approvals` 0.1.2**: `redact` left (it is a gateway concern, not signed approvals). `rightkit-web-session` reads it through `rightkit_remote_gateway::redact`.

Deleted as duplicates: Pulse's local `Origin`, its hand-built activity line and read-only check, MailRight's `audit.rs` and `redact.rs`, the wrapper half of `content.rs`, `invisible_or_control` (one copy in the core registry), and the second redaction module (owner-approvals). Pending approvals were already one core.

Wire-visible renames (MailRight, unpublished): `Agent.accounts` is `scopes`; the Settings views are the core's (`AgentView.scopes`, `AuditView.scope`); `audit::Entry.account_id` is `scope`. The SQL schema is unchanged (`agent_accounts.account_id`).

### Not done (gateway)

* Pulse's per-tool switches, manifest and unlock are still its own code in `run`; its tools are not yet a core `Method` table (the manifest is runtime JSON). Next step: build the `Method` table from the manifest and run `lookup` and `allow_door` from it.
* Pulse product decisions remain in `rightkit-remote-gateway` (ChatGPT door, `gptchat.rs`, Spotify/Music in `media.rs`, `agent_secrets.rs` 2k lines of saved logins and cards): the review's actions "split agent_secrets into its own crate" and "remove Pulse and ChatGPT names from library text" are open.
* `rightkit-owner-approvals/audit.rs` still owns the activity and security files that `ActivityFile` appends to; a JSON-lines sink in the core would move the file writer, not just the line.

## B. `rightkit-agent-server`: private copies deleted

| Copy in agent-server | Lines | Owner | Now |
|---|---|---|---|
| `src/github_app/` (11 files) | 3,503 | `rightkit-github` (95-100% identical) | deleted; the TUI pull-request pane calls `rightkit_github::forge_client`. User-Agent is the crate's neutral one |
| `src/github_signin.rs` | 260 | `rightkit-github::signin` (74%, the crate is the host-configurable copy) | thin host module: installs client id, `coderight` credential namespace and keychain switch with `signin::configure`, then calls the crate |
| `src/scheduler/model.rs`, `time.rs` | 412 | `rightkit-scheduler` (identical but for visibility) | re-exports |
| bot profile and lifecycle types in `api_schedules.rs` | 206 | `rightkit-scheduler::profile` (identical) | re-export |
| `rightkit-agent-tools` `schedule_cron_tool/timezone.rs` | 181 | `rightkit-scheduler::timezone` (identical) | re-export (agent-tools 0.3.5, optional dependency under `coding_tools`) |
| `src/membrane_binding/host_observation.rs` | 300 | `rightkit-context-protocol::host_observation` (75%, same v1 JSON) | re-export; `HostReceiptProvenanceV1` is the protocol's `HostObservationProvenanceV1` |
| `src/telegram/` (loop, Bot API, HTTP daemon client, status) | 677 | missing from `rightkit-bot-messaging` (it held only the decision layer) | moved to `rightkit-bot-messaging::telegram_poll`; the binding store path is a parameter, the `.coderight` fallback stays in the host |

About 4.6k lines of copies are gone; 0.7k lines moved to their owner. `rightkit-agent-server` now depends on `rightkit-github`, `rightkit-scheduler` and `rightkit-context-protocol`, so both previously unused crates have a user. A line-set scan of every agent-server file against every other crate finds no file above 50% shared after this change (the two highest: a 57-line CLI enum at 50%, a 49-line status enum at 36%).

### Checked, not a duplicate

* `src/workflow/` (7.2k lines) is the daemon host of `rightkit-workflow`'s control plane: supervisor, leased queue, dispatcher, executors, cron and monitor workers over `rightkit-agent-engine-storage`'s SQLite repository and the daemon's routes. It shares under 16% with any `rightkit-workflow` file; it uses the crate's state machine. It cannot move into `rightkit-workflow` (engine-storage depends on workflow). It belongs in a workflow-host crate over engine-storage once the daemon routes it calls are split out.
* `src/scheduler/` runner (execution, delivery, lifecycle, persistence, 2.7k lines): not a copy; it runs on `DaemonState`. The review's "move it into rightkit-scheduler behind a runner trait" is open.

### Not done (agent-server split)

The full split (a small generic agent-server: session hosting, daemon, API router, permission and session routes; CodeRight's product routes in the app) is larger than one change and is not done. Open:

1. Product strings host-supplied: about 530 `coderight` strings remain (paths `.coderight/`, the credential namespace, the client id, wire labels like `coderight.provider.context-assembly`).
2. Benchmarks (`api_benchmarks`), `api_specs`, `learning`, `membrane_binding` and the harness-radar feed and `data/harness-radar` out of the library, to the app or a bench crate.
3. The 11.7k lines planned for `rightkit-agent-engine`, 6.8k for `rightkit-agents`, 4.6k for `rightkit-llm`, 3.7k for orchestration, 2.6k for sandbox, 1.8k for evidence: move each to its package or change the plan.
4. Scheduler runner into `rightkit-scheduler` behind a runner trait; workflow host into its own crate.

## Versions

| Crate | Version | Note |
|---|---|---|
| `rightkit-agent-gateway` | 0.3.0 (breaking: now the core) | new content; versions 0.1 and 0.2 were the mail gateway |
| `rightkit-mail-gateway` | 0.1.0 (new, PK230) | the mail verbs |
| `rightkit-remote-gateway` | 0.1.3 (open, absorbed) | on the core |
| `rightkit-owner-approvals` | 0.1.2 (open, absorbed; breaking: `redact` moved) | |
| `rightkit-agent-server` | 0.1.2 (open, absorbed) | duplicates deleted |
| `rightkit-bot-messaging` | 0.1.2 (open, absorbed; additive) | `telegram_poll` |
| `rightkit-agent-tools` | 0.3.5 (open, absorbed) | timezone from the scheduler |
| `rightkit-web-session` | Unreleased entry | reads redaction through remote-gateway |

`rightkit-github`, `rightkit-scheduler` and `rightkit-context-protocol` are unchanged; their rows change because their copies are gone.
