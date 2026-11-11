# Family review: Agent platform

Read-only review, 11 Nov 2026. Sources: RightKit working tree, `S/survey/{coderight,pulse,mailright,genright,cutright,postright}`, `S/coverage-rows.json`, and a per-file map of which RightKit unit holds each app file's lines (built for this review).

Depth: **deep** (code read on both sides) for `rightkit-agent-engine`, `rightkit-agent-gateway` + `rightkit-remote-gateway`, `rightkit-approvals` + `rightkit-owner-approvals`, `rightkit-agents`. **Light** for everything else: Cargo manifests, module lists, line-match placement, duplicate-file scan and brand-string counts, not behaviour. That includes eight packages over 5,000 planned lines that the brief asks to be read deeply (`agent-server`, `agent-tools`, `terminal-ui`, `agent-orchestration`, `llm`, `@rightkit/agent-ui`, `chat-bridge`, `ai-accounts`): they were not, on the coordinator's instruction to be economical. Legion source was not available.

## Family summary

Counts by shape (31 rows): best 5, gaps 21, wrong 4, missing 1.

1. **Shared names hide separate implementations.** In the four contested packages no app's shape 'won' by being merged; each app's code was placed in its own crate or folder:
   - `rightkit-agent-engine` (base crate) is Pulse's task contract; `rightkit-agent-engine-*` is CodeRight's engine. Nothing depends on the base crate.
   - `rightkit-agent-gateway` is MailRight's mail gateway; Pulse's gateway is `rightkit-remote-gateway`.
   - `rightkit-approvals` is GenRight's; Pulse's is `rightkit-owner-approvals` plus a queue in remote-gateway; MailRight's is inside agent-gateway.
   - `rightkit-agents` holds CodeRight's driver host beside the older client; Pulse's code went to `rightkit-agent-transcript`.
2. **Approvals exist five times**: three pending-request queues (GenRight files, Pulse memory, MailRight SQL) and two engine-side records (Pulse `ApprovalBinding`, CodeRight `PermissionGateRequest`). Pulse's rules are the reference. This is the largest violation of 'no duplicate code' in the family, and it is duplicate design more than duplicate lines, so the line match does not show it.
3. **`rightkit-agent-server` is CodeRight's application**, 164k lines with 532 'coderight' strings. It holds its own copies of the scheduler and GitHub App modules (the crates `rightkit-scheduler` and `rightkit-github` are used by nothing), and roughly 40k lines the plan assigned to other packages (engine, agents, llm, workflow, terminal-ui glue, sandbox, evidence, bot-messaging).
4. **Layering inversions**: `rightkit-llm` (agent_providers feature) depends on `rightkit-agent-policy`; `rightkit-github` depends on `rightkit-agent-orchestration`; `rightkit-hooks` depends on `rightkit-llm`; `@rightkit/agent-ui-approvals` depends on `@rightkit/mail-ui`; `rightkit-approvals` depends on `rightkit-media-gen`.
5. **Product strings in library code**: `heardright:control` in the task contract; 134 Pulse strings in agent-ui-approvals; Pulse/ChatGPT/Spotify in remote-gateway; 48 'coderight' in rightkit-agents.
6. **`RightKitAgent` (Swift) does not exist.** The plan maps 35k lines of app screens to it. Two designs for an agent on iPhone exist (CodeRight's Rust mobile runtime, Pulse's Swift loop with budget ledger and device-lock approval gate); choose before writing.
7. **Plan mapping errors found**: Pulse `membrane-transcript` targeted at rightkit-agents; Pulse `devicelink/phone.rs` and `winadmin/*` at rightkit-approvals; Pulse `windows/src/agent.rs` (engine supervisor) at agent-engine; PostRight `drafting.rs` at rightkit-agents; CodeRight binary sources at terminal-ui.

Order of work before publishing: approvals core, then gateway core, then the agent-engine rename and contract merge, then split agent-server. The single-app CodeRight crates can publish after their agent-server duplicates are removed.

## Packages

### rightkit-agent-engine

1. **Exists as:** `crates/rightkit-agent-engine`, `crates/rightkit-agent-engine-core`, `crates/rightkit-agent-engine-protocol`, `crates/rightkit-agent-engine-storage`, `crates/rightkit-agent-engine-history`, `crates/rightkit-agent-engine-tasks`, `crates/rightkit-agent-engine-mobile-runtime`, `crates/rightkit-agent-engine-runtime-middleware`, `crates/rightkit-agent-engine-runtime-resources`, `crates/rightkit-agent-engine-attention`, `crates/rightkit-agent-engine-cli`, `crates/rightkit-agent-engine-api-sessions`, `crates/rightkit-agent-engine-api-workflows`, `crates/rightkit-agent-engine-api-integrations`, `crates/rightkit-agent-engine-api-platform`
2. **Feeds (planned lines):** coderight 124,089, pulse 5,097
3. **Holds today:** Two unrelated things under one name: the base crate is Pulse's always-on task contract (core/src/agent); the 14 -* crates are CodeRight's turn-loop engine.
4. **Comparison:** see below.
5. **Shape:** `wrong`. The base crate and the -* family share a name but no code or dependency; nothing depends on the base crate. Task, approval and effect types exist twice with different meanings, and Principal/DeviceRef/EffectClass are copied in rightkit-device-link-types. Reference: coderight for the turn loop; pulse for the task/authority contract.
6. **Per app:** coderight: gaps (loop, protocol, storage are its code; 11.7k of its engine lines sit in agent-server instead); pulse: gaps (only core/src/agent contracts (452 lines) are in; Swift iOS loop, budget ledger, approval gate and Windows engine supervisor are not, and most of that is app shell or belongs to RightKitAgent/daemon)
7. **Boundary problems:** 
   - rightkit-agent-engine (Pulse contract) duplicates principal.rs, device.rs, effect.rs of rightkit-device-link-types almost line for line
   - voice.rs hard-codes VOICE_CHAT = "heardright:control" and a HeardRight R0/R1 routing model: a product decision in a contract crate
   - task_cli.rs (672 lines, a CLI) lives in a crate that says it has no runtime or IPC
   - Two task models: Pulse TaskEnvelope/TaskStatus (10 statuses, idempotency key, owner epoch) vs CodeRight TaskNode/TaskState (planning DAG) in engine-protocol + engine-tasks
   - Two approval records: Pulse ApprovalBinding (args_hash mandatory) vs CodeRight PendingApprovalContext (payload_digest optional)
   - Pulse planned files include windows/src/agent.rs (engine supervisor: belongs to rightkit-daemon/sidecar) and Swift UI panels (app shell)
8. **Before publishing:** 
   - Rename the base crate rightkit-agent-task-contract (or fold into engine-protocol) and make rightkit-agent-engine a facade over the -* crates
   - Delete the duplicated Principal/DeviceRef/EffectClass; one home (device-link-types or the contract crate), re-exported
   - Move voice.rs to the dictation/voice family or make chat label and policy ref host parameters
   - Move task_cli.rs out to a CLI crate
   - Adopt Pulse's rule in the CodeRight loop: approval always bound to a digest of the exact arguments, destination, expiry and revision; map ExternalEffectSettlement::InterruptedUnknown to TaskStatus::Uncertain
   - Rename CodeRight TaskNode/TaskState to PlanTask* so the two task concepts cannot be confused

Depth: deep. Confidence: high.

**Side by side (verified by reading).**
- Pulse (`survey/pulse/core/src/agent/*.rs`, now `crates/rightkit-agent-engine/src`): serde records and pure functions only. `TaskEnvelope` carries `idempotency_key`, `owner_epoch`, `intent` provenance, `target_device`, `authority` (`task.rs:33-69`); ten statuses with `Uncertain` and non-terminal `CancelRequested` (`task.rs:104-118`), a const transition table (`task.rs:149-172`). `ApprovalBinding` binds principal, task, effect class, `args_hash`, destination, executor, scope, expiry, revision (`approval.rs:17-40`). `IntentSource` records where authority came from and never lets a relay mint it (`intent.rs:33-84`).
- CodeRight (`crates/rightkit-agent-engine-core`, 33.5k lines; `-protocol`, `-storage`): the turn loop (`engine_loop*.rs`), tool dispatch with a permission gate (`engine_tool_dispatch_approval.rs`, `engine_permission_flow.rs`), compaction, goals, guardian. Its task type is a planning DAG node: `TaskNode`/`TaskState` with Open/Blocked/Done/Split/Merged (`rightkit-agent-engine-protocol/src/task.rs:4-70`). Its approval record is `PermissionGateRequest`/`PendingApprovalContext` with `payload_digest: Option<String>`, "optional for back-compat" (`approvals.rs:41-51`, `120-133`). Its unknown-outcome state is `ExternalEffectSettlement::InterruptedUnknown` (`effect_settlement.rs:8-16`).
- They do not conflict in function: CodeRight answers "how does one agent turn run", Pulse answers "who asked, with what authority, on which device, and what happens when the outcome is unknown". CodeRight's loop is plainly the more general design for the loop (Pulse has no Rust loop; its iOS loop is 91 lines of Swift with a Pulse system prompt, `ios/Sources/Agent/AgentLoop.swift:17-28`). Pulse's contract is the reference for task authority and approval binding because the owner uses it daily and it is stricter (digest mandatory, provenance recorded).
- Nothing in RightKit depends on `rightkit-agent-engine` (grep of every Cargo.toml and source: no user outside the crate). `principal.rs`, `device.rs`, `effect.rs` differ from `rightkit-device-link-types/src/*` only in comment lines (diffed).
- `voice.rs:36` sets `VOICE_CHAT = "heardright:control"`; `intent.rs:57` names "Astra per workspace rule".

**Best shape.** `rightkit-agent-engine` = facade re-exporting `-core`, `-protocol`, `-storage`. Pulse's records move to one contract crate (`rightkit-agent-task-contract`) that the engine protocol uses for authority: a tool call that needs approval carries an `ApprovalBinding`; an effect whose result was lost reports `Uncertain`. Today's package is not that shape.

### @rightkit/agent-ui

1. **Exists as:** `packages/agent-ui-core`, `packages/agent-ui-components`, `packages/agent-ui-hooks`, `packages/agent-ui-settings`, `packages/agent-ui-web`, `packages/agent-ui-workbench`, `packages/agent-ui-approvals`
2. **Feeds (planned lines):** coderight 118,530, mailright 1,395, pulse 1,308
3. **Holds today:** CodeRight's desktop UI split into six packages; agent-ui-approvals is MailRight's approval card/queue and Pulse's hub remote views placed side by side, not merged.
4. **Comparison:** single feeding app or no second implementation read; reference pulse for approvals behaviour; coderight for the workbench.
5. **Shape:** `gaps`. Six CodeRight packages are a faithful lift (95% of lines found). agent-ui-approvals is two apps' screens in two folders with no shared approval model, depends on @rightkit/mail-ui and carries 134 Pulse strings. Reference: pulse for approvals behaviour; coderight for the workbench.
6. **Per app:** coderight: fits (its own code; 74 'coderight' strings remain in the six packages); mailright: gaps (card, queue and settings are in; IPC client (agentApprovals.ts) landed in mail-ui); pulse: gaps (views are in verbatim under remote/, still Pulse-branded)
7. **Boundary problems:** 
   - agent-ui-approvals depends on @rightkit/mail-ui: a generic approvals UI must not pull the mail UI
   - agent-ui-approvals/src/remote holds Pulse hub pages that are not approvals (Devices, Guests, Monitor, Membrane, Overview, PhonePairing)
   - No package named @rightkit/agent-ui exists; the plan name maps to seven packages
8. **Before publishing:** 
   - Define one ApprovalItem model (id, kind, scope, origin, summary, detail, digest, expiry) matching the Rust approvals core and render both MailRight cards and Pulse lists from it
   - Move mail-specific card content behind a render prop; drop the mail-ui dependency
   - Move Pulse hub pages that are not approvals to a remote-gateway UI package
   - Replace product strings with the appName prop already present (appName.ts)

Depth: light. Confidence: medium.

### rightkit-terminal-ui

1. **Exists as:** `crates/rightkit-terminal-ui`
2. **Feeds (planned lines):** coderight 94,376
3. **Holds today:** CodeRight's ratatui TUI.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Crate is brand-free and self-contained (3 deps), but 16.7k planned lines from CodeRight's binary (engine/bins/coderight/src) landed in agent-server, so the TUI host glue is not in this package. Reference: coderight.
6. **Per app:** coderight: gaps (73% of lines in target; host glue in agent-server)
7. **Boundary problems:** 
   - TUI host/session glue sits in rightkit-agent-server
8. **Before publishing:** 
   - Decide whether the session-facing TUI host belongs here behind a trait or stays in agent-server; document the seam

Depth: light. Confidence: medium.

### @rightkit/terminal-ui

1. **Exists as:** `packages/terminal-ui`
2. **Feeds (planned lines):** coderight 1,370
3. **Holds today:** CodeRight's web terminal component.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `best`. Small, 98% of CodeRight's lines present, no second implementation to reconcile. Reference: coderight.
6. **Per app:** coderight: fits
7. **Boundary problems:** none found
8. **Before publishing:** none

Depth: light. Confidence: medium.

### rightkit-agent-server

1. **Exists as:** `crates/rightkit-agent-server`
2. **Feeds (planned lines):** coderight 91,539
3. **Holds today:** CodeRight's coderight binary and HTTP API, 164k lines, plus code planned for nine other packages.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `wrong`. It is CodeRight's application binary under a library name: 532 'coderight' strings, benchmarks, harness-radar data, and its own copies of the scheduler (3.0k), GitHub App (3.5k), workflow (7.2k) and Telegram (0.7k) modules. It does not depend on rightkit-scheduler or rightkit-github. Reference: coderight.
6. **Per app:** coderight: gaps (its code, but a second app cannot adopt it without CodeRight's product surface)
7. **Boundary problems:** 
   - src/github_app duplicates rightkit-github (forge.rs 97% identical, 10 files)
   - src/scheduler duplicates and extends rightkit-scheduler; neither crate uses the other
   - src/workflow (7.2k) vs rightkit-workflow; src/telegram vs rightkit-bot-messaging
   - api_benchmarks, api_specs, learning, membrane_binding, data/harness-radar are CodeRight product features
   - Holds 11.7k lines planned for agent-engine, 6.8k for agents, 4.6k for llm, 3.7k for orchestration, 2.6k for sandbox, 1.8k for evidence
8. **Before publishing:** 
   - Split: a small generic rightkit-agent-server (session hosting, daemon, API router, permission and session routes) and leave CodeRight's product routes in the app
   - Delete src/github_app and src/scheduler in favour of the crates; move execution/delivery/persistence into rightkit-scheduler
   - Move each misplaced module to the package the plan named, or change the plan

Depth: light. Confidence: high.

### rightkit-agent-orchestration

1. **Exists as:** `crates/rightkit-agent-orchestration`, `crates/rightkit-agent-orchestration-handoff`
2. **Feeds (planned lines):** coderight 37,870
3. **Holds today:** CodeRight's orchestration crate; -handoff also matches some Pulse transcript lines.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. 80% of CodeRight's lines present; single app, 57k lines, 14 RightKit deps including repo-graph and code-audit-provider-sdk, which makes it a coding-agent orchestration, not a general one. Reference: coderight.
6. **Per app:** coderight: fits (3.7k lines still in agent-server)
7. **Boundary problems:** 
   - Depends on rightkit-repo-graph and rightkit-code-audit-provider-sdk: coding-product coupling
   - rightkit-github depends on this crate (inverted)
8. **Before publishing:** 
   - Put repo-graph and code-audit coupling behind features
   - Not read for behaviour

Depth: light. Confidence: low.

### rightkit-evidence

1. **Exists as:** `crates/rightkit-evidence`, `crates/rightkit-evidence-application`, `crates/rightkit-evidence-commands`, `crates/rightkit-evidence-contracts`, `crates/rightkit-evidence-decisions`, `crates/rightkit-evidence-effects`, `crates/rightkit-evidence-governance`, `crates/rightkit-evidence-report`
2. **Feeds (planned lines):** coderight 5,738
3. **Holds today:** rightkit-evidence holds a quarter of CodeRight's planned evidence code; most (1.8k of 2.2k lines) is in agent-server. The seven -* crates (27k lines) are not fed by any planned app file in this family.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Planned code is mostly in agent-server; the family's larger crates have no surveyed app source to compare against. Reference: coderight.
6. **Per app:** coderight: gaps (move evidence modules out of agent-server)
7. **Boundary problems:** 
   - Evidence code split between rightkit-evidence and agent-server
   - Origin of rightkit-evidence-* not verified
8. **Before publishing:** 
   - Move CodeRight evidence modules from agent-server
   - Establish which app owns the -* crates before publishing

Depth: light. Confidence: low.

### rightkit-agent-gateway

1. **Exists as:** `crates/rightkit-agent-gateway`, `crates/rightkit-remote-gateway`
2. **Feeds (planned lines):** mailright 14,620, pulse 20,758
3. **Holds today:** rightkit-agent-gateway is MailRight's mail gateway (drafts, forward, send-with-approval, Gmail seams); Pulse's gateway is a separate crate, rightkit-remote-gateway.
4. **Comparison:** see below.
5. **Shape:** `wrong`. The package named 'agent gateway' is a mail product: it depends on rightkit-mail and its methods are mail verbs. Pulse's gateway shares none of its code. The common part (pairing, gate pipeline, audit, pending approvals, untrusted-content wrapping, redaction) exists twice and is not factored out. Reference: pulse for gate order, approval binding, redaction and refusal codes; mailright for the paired-agent registry and SQL audit (the more general design: several agents, each with a profile and account scope).
6. **Per app:** mailright: fits (its own code; 34% matched because 9k of its 14.6k planned lines are tests and Gmail adapters that stay in the app); pulse: wrong (would have to adopt mail tables and verbs; its code is in rightkit-remote-gateway)
7. **Boundary problems:** 
   - rightkit-agent-gateway depends on rightkit-mail and rightkit-search; description says 'for a mail-style app'
   - Approvals implemented inside the gateway (approval*.rs, 1.3k lines) instead of rightkit-approvals
   - redact.rs here only detects Google token markers (a release-gate check); rightkit-owner-approvals/redact.rs masks secrets in output: same name, different job
   - rightkit-remote-gateway carries Pulse product decisions: ChatGPT gateway (gptchat.rs), Spotify/Music control, 'owner app-state.json' switches, 31 Pulse strings
   - Pulse gateway/src/tools.rs and provider.rs (1.9k lines, the ChatGPT-facing server) are in no RightKit crate
8. **Before publishing:** 
   - Extract a generic core as rightkit-agent-gateway: agent registry (hashed key, profile, scopes, revoke), gate pipeline (key, lock, profile, scope, rate limit, handler, one audit row), audit store trait (SQL and JSONL), Origin/door, Refusal with stable codes, untrusted-content wrapper
   - Rename today's crate rightkit-mail-agent-gateway (mail family) on top of the core
   - Rebase rightkit-remote-gateway on the same core; move ChatGPT and media-app specifics behind host-registered tools
   - Move all three approval stores to rightkit-approvals
   - One redaction module (Pulse's masking) plus MailRight's leak assertion as its test helper

Depth: deep. Confidence: high.

**Side by side (verified by reading crate sources, which the line match shows are the app files: MailRight `src-tauri/src/agent/*.rs` 95-100% per file, Pulse `core/src/remote/*.rs` 95-100% per file).**
| | MailRight (`rightkit-agent-gateway`) | Pulse (`rightkit-remote-gateway`) |
|---|---|---|
| Caller identity | Paired agents in SQL, hashed key, `Profile` (read / drafts / send-with-approval), per-agent account list, revoke (`registry.rs:12-182`, `pairing.rs:71-140`) | `Origin` door (local, engine, gateway) plus `Requester` and client key `chat:<id>`, `agent`, `gateway:<name>` (`unlock.rs:86-130`); switches from the owner app's state file (`settings.rs`) |
| Gate | One function: key, lock, profile, account, rate limits, handler, one audit row (`gate.rs:142`, `lib.rs:3-6`) | One `execute` entry for CLI, engine and gateway; manifest check, settings switch, per-chat unlock, approval (`lib.rs:63-67`) |
| What agents can do | Mail only: read, draft, forward, label, export, attach (`gate.rs:484` method names) | 20 tool families: files, docs, terminals, processes, screen, apps, media, clipboard, chats, saved logins |
| Approval | SQL row per request, `kind` send/forward/delete, bound to draft hash and `boot_id`, atomic `claim_approve`, decline cooldown 10 min, hourly/daily delete caps (`approval_store.rs:22-43`, `379-388`) | In-memory item, `kind` act/session, `bind` SHA-256 of exact arguments, owner click bound to `request_digest` of what was displayed, 10-minute validity, session and standing grants, max 16 pending (`approvals.rs:33-75`, `395`) |
| Audit | SQL table `agent_audit`, paged (`audit.rs:60-65`) | `remote-activity.jsonl`, one line per call (`audit.rs:18`) |
| Redaction | Detects Google token markers in responses, used as a release gate (`redact.rs:8-30`) | Masks sk-, ghp_, xox, AKIA, AIza, Bearer, PEM, key=value secrets in returned text (`rightkit-owner-approvals/src/redact.rs:1-40`) |
| Untrusted text | Wraps sender-written mail text (`content.rs`) | Stamps ChatGPT messages as unverified (`gptchat.rs`) |

**Whose is the reference.** Pulse for the gate's rules (approval bound to a digest of what the owner saw, single owner decides locally, output masking, stable refusal codes). MailRight for the caller registry: several named agents each with a profile and scope is the more general design; Pulse has one owner and identifies callers by door, which a second app with several paired agents cannot use. Both keep one audit row per call; the store should be a trait.

**Best shape.** A core crate with no product verbs, and two tool crates on it. Today's package is not that shape: its name promises the core and its content is the mail tool set.

### RightKitAgent

1. **Exists as:** does not exist
2. **Feeds (planned lines):** coderight 30,723, pulse 4,258
3. **Holds today:** none.
4. **Comparison:** single feeding app or no second implementation read; reference pulse.
5. **Shape:** `missing`. No Swift module exists. CodeRight's 30.7k lines are its whole iOS app; Pulse's are iOS screens. The reusable part is small: provider protocol, budgeted loop step, approval gate with device-lock confirmation, pairing. Reference: pulse.
6. **Per app:** coderight: n/a (package missing; its iOS app uses the Rust mobile runtime (rightkit-agent-engine-mobile-runtime)); pulse: n/a (package missing)
7. **Boundary problems:** 
   - Plan maps whole app screens (HomeView, SettingsView, CodeRight iOS app) to a library
   - Pulse's Swift loop and CodeRight's Rust mobile runtime are two answers to 'agent on iPhone'
8. **Before publishing:** 
   - Decide first: Swift module wrapping rightkit-agent-engine-mobile-runtime over FFI (CodeRight) with Pulse's approval gate and budget ledger as the Swift-side policy; then write it
   - Re-map screens to SHELL

Depth: light. Confidence: medium.

### rightkit-chat-bridge

1. **Exists as:** `crates/rightkit-chat-bridge`
2. **Feeds (planned lines):** pulse 26,856
3. **Holds today:** Pulse's chat-between-chats code.
4. **Comparison:** single feeding app or no second implementation read; reference pulse.
5. **Shape:** `gaps`. 75% of Pulse's lines present and the host supplies brand and state folder, but 52 Pulse strings remain and it depends on rightkit-ai-accounts (Claude account switching) for delivery. Reference: pulse.
6. **Per app:** pulse: fits
7. **Boundary problems:** 
   - Hard dependency on rightkit-ai-accounts
   - Claude Code/Codex chat discovery overlaps rightkit-agent-transcript discovery and rightkit-agents discovery (suspicion, not diffed)
8. **Before publishing:** 
   - Remove remaining Pulse strings
   - Make ai-accounts an optional feature
   - Check discovery overlap with agent-transcript

Depth: light. Confidence: medium.

### rightkit-agent-policy

1. **Exists as:** `crates/rightkit-agent-policy`
2. **Feeds (planned lines):** coderight 16,018
3. **Holds today:** CodeRight's policy crate.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. 85% present, 4 deps, 3 brand strings. Pulse's remote-gateway has its own command allow/deny policy (term.rs remoteTermAllow/Deny) and effect classes that do not use it. Reference: coderight.
6. **Per app:** coderight: fits
7. **Boundary problems:** 
   - Second command policy in rightkit-remote-gateway/term.rs; rightkit-policy-rules also matches ~300 lines
8. **Before publishing:** 
   - Have remote-gateway's terminal command policy call this crate's classifier, or state why not
   - Check overlap with rightkit-policy-rules

Depth: light. Confidence: medium.

### rightkit-ai-accounts

1. **Exists as:** `crates/rightkit-ai-accounts`
2. **Feeds (planned lines):** pulse 17,592
3. **Holds today:** Part of Pulse's Claude account switching (29% of planned lines).
4. **Comparison:** single feeding app or no second implementation read; reference pulse.
5. **Shape:** `gaps`. Single app, Claude-specific by design; two thirds of Pulse's planned code is not in. Reference: pulse.
6. **Per app:** pulse: gaps (most planned files absent)
7. **Boundary problems:** 
   - Name says AI accounts; content is Claude Desktop/CLI only
8. **Before publishing:** 
   - Port the remaining Pulse modules or re-map them to SHELL
   - Not read for behaviour

Depth: light. Confidence: low.

### rightkit-approvals  (with rightkit-owner-approvals)

1. **Exists as:** `crates/rightkit-approvals`, `crates/rightkit-owner-approvals`
2. **Feeds (planned lines):** pulse 13,104, mailright 2,083, genright 1,485
3. **Holds today:** rightkit-approvals is GenRight's paid-draft approval (launch-secret HMAC token, 'eyes' release, blender_finish). Pulse's signed owner approvals are in rightkit-owner-approvals and its pending queue in rightkit-remote-gateway/approvals.rs. MailRight's are in rightkit-agent-gateway/approval*.rs.
4. **Comparison:** see below.
5. **Shape:** `wrong`. Three apps, three unconnected approval implementations in four crates; the crate with the shared name depends on rightkit-media-gen and has GenRight draft kinds built in, so no other app can use it. Reference: pulse.
6. **Per app:** pulse: wrong (nothing of Pulse's model is here; uses owner-approvals + remote-gateway); mailright: wrong (0% in target; its store is inside agent-gateway); genright: fits (its own code)
7. **Boundary problems:** 
   - rightkit-approvals depends on rightkit-media-gen (proto types) and names blender_finish and eyes
   - Pending-request queue exists three times: remote-gateway/approvals.rs (in memory, digest-bound), agent-gateway/approval_store.rs (SQL, boot-bound), approvals/lib.rs (files, nonce + .used marker)
   - A fourth and fifth approval record live in agent-engine (ApprovalBinding) and agent-engine-protocol (PermissionGateRequest)
   - Plan maps Pulse devicelink/phone.rs (3.6k) and winadmin service (3.5k) here; they are device-link and elevated-helper code
8. **Before publishing:** 
   - Redraw rightkit-approvals as the core: Request {id, kind, scope, origin, client, bind digest of exact arguments, summary, detail, created, expires}; states pending/approved/denied/consumed/expired; single-use claim; caps and decline cooldown; ApprovalStore trait (memory, file, SQL); Proof trait (owner click with request digest, launch-secret HMAC, signed approver key)
   - Keep Pulse's rules as the reference: only the owner's app decides, the click is bound to a digest of what was shown, an approval is spent by one act
   - Move GenRight kinds (eyes, blender_finish) to a media-gen approvals crate that registers kinds; drop the media-gen dependency
   - Fold rightkit-owner-approvals in as the 'signed' Proof (or keep as -signed sub-crate); move its redact.rs and audit.rs out
   - Port agent-gateway/approval_store.rs and remote-gateway/approvals.rs onto the core

Depth: deep. Confidence: high.

**Side by side (verified by reading).**
- GenRight (`crates/rightkit-approvals/src/lib.rs`, from `genright-studio/src/approvals.rs`): preflight issues a single-use challenge (nonce, SHA-256 of canonical inputs, 30-minute expiry, `lib.rs:77`, `382`); approve needs `HMAC-SHA256(launch secret, kind|draft id|input hash|nonce)` minted by the app shell (`lib.rs:8-14`); consumed by creating a `.used` file (`lib.rs:447`). Draft kinds registered by host (`register_kind`, `lib.rs:546`), but `blender_finish` and `eyes` are built in and the protocol types come from `rightkit_media_gen::host::approval` (`lib.rs:58`). The crate documents that this is not a boundary against local code (`lib.rs:25-33`).
- Pulse (`crates/rightkit-owner-approvals/src/approval_sig.rs` 1.6k, `admin_sig.rs` 1.1k, `protected_guard/*`; queue in `rightkit-remote-gateway/src/approvals.rs`): requests are created pending by tools only; only the owner's app decides through an owner-only engine call (`approvals.rs:20-23`); the click carries the digest of kind, scope, door, bind and summary as displayed (`approvals.rs:392-402`); signed variants use approver keys (`decide_signed`, `approvals.rs:735`). iOS adds device-lock confirmation per action (`ios/Sources/Agent/AgentApprovalGate.swift:4-6`).
- MailRight (`crates/rightkit-agent-gateway/src/approval.rs`, `approval_store.rs`, `approval_kind.rs`): pending row with expiry and boot id, user acts in the app window, `claim_approve` is the single atomic transition (`approval_store.rs:379`), kinds send/forward/delete map to words and guard actions (`approval_kind.rs:18-49`).
- Common to all three: a pending request with an id, a kind, a hash of the exact thing approved, an expiry, single use, decision only in the app's own window. Different: proof of who decided (HMAC launch secret; owner click with display digest or signature; in-process UI call) and storage (files; memory; SQL).

**Whose is the reference.** Pulse: it is the daily-used one and the strictest (display-digest binding, per-client session grants, caps on pending). MailRight's store adds two useful rules to keep: decline cooldown and boot binding. GenRight's HMAC is one Proof implementation.

**Best shape.** See actions. Today's package is not that shape.

### rightkit-scheduler

1. **Exists as:** `crates/rightkit-scheduler`
2. **Feeds (planned lines):** coderight 9,315
3. **Holds today:** A model-only subset (816 lines) of CodeRight's scheduler.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Only 11% of CodeRight's scheduler is here; the rest (execution, delivery, lifecycle, persistence) is a second copy in agent-server/src/scheduler, and no crate depends on this one. Reference: coderight.
6. **Per app:** coderight: gaps (agent-server must use it)
7. **Boundary problems:** 
   - model.rs and time.rs duplicated in agent-server (78-87% identical)
   - timezone.rs duplicated in rightkit-agent-tools schedule_cron_tool (100%)
   - Schedule spec also exists in agent-engine-protocol/schedule.rs
8. **Before publishing:** 
   - Move execution, delivery, lifecycle, persistence here behind a runner trait; delete the copies

Depth: light. Confidence: high.

### rightkit-github

1. **Exists as:** `crates/rightkit-github`
2. **Feeds (planned lines):** coderight 8,586
3. **Holds today:** CodeRight's GitHub App module.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Brand-free, but the same ten files are still in agent-server/src/github_app and the crate depends on rightkit-agent-orchestration and rightkit-review. Reference: coderight.
6. **Per app:** coderight: gaps (agent-server does not depend on it)
7. **Boundary problems:** 
   - Duplicate of agent-server/src/github_app
   - Depends on agent-orchestration: a forge client should not need the orchestration runtime
8. **Before publishing:** 
   - Delete the agent-server copy
   - Cut the orchestration and review dependencies down to shared contract types

Depth: light. Confidence: high.

### rightkit-bot-messaging

1. **Exists as:** `crates/rightkit-bot-messaging`
2. **Feeds (planned lines):** coderight 6,489
3. **Holds today:** Under half of CodeRight's bot messaging; the rest and the Telegram client are in agent-server.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Right seam (4 deps, brand-free) but incomplete. Reference: coderight.
6. **Per app:** coderight: gaps
7. **Boundary problems:** 
   - agent-server/src/telegram
8. **Before publishing:** 
   - Move telegram and remaining bridge code here

Depth: light. Confidence: medium.

### rightkit-agent-demo

1. **Exists as:** `crates/rightkit-agent-demo`
2. **Feeds (planned lines):** cutright 4,434
3. **Holds today:** CutRight's agent demo mode.
4. **Comparison:** single feeding app or no second implementation read; reference cutright.
5. **Shape:** `gaps`. 79% present; needs Tauri shell, record, overlay, hotkeys. CutRight carries the least weight as a reference, and the crate names Pulse and GenRight in source. Reference: cutright.
6. **Per app:** cutright: fits
7. **Boundary problems:** 
   - 8 Pulse and 4 GenRight strings in a generic crate
   - Hard dependency on rightkit-tauri-shell
8. **Before publishing:** 
   - Remove product names
   - Not read for behaviour

Depth: light. Confidence: low.

### rightkit-agents

1. **Exists as:** `crates/rightkit-agents`
2. **Feeds (planned lines):** coderight 44,458, pulse 2,835, postright 339
3. **Holds today:** Two layers: RightKit's original sans-IO Claude Code and Codex clients with a blocking Session, plus CodeRight's driver_host (async executor, six provider profiles, ACP) behind features. Pulse's planned code is in rightkit-agent-transcript.
4. **Comparison:** see below.
5. **Shape:** `gaps`. CodeRight's driver_host won and is the right base, but it sits beside the older client rather than replacing it: two event models (AgentEvent, NormalizedEvent), two binary lookups (discovery.rs, driver_host/binary_lookup.rs), two session lifecycles, 48 'coderight' strings. Reference: coderight (driver_host is plainly the more general design; Pulse has no vendor-session driver, it reads transcripts and delivers through hooks).
6. **Per app:** coderight: gaps (driver crate is in; 13k lines of api_drivers and vendor-turn runtime sit in agent-server); pulse: fits (nothing to adopt here; its transcript normaliser is correctly a separate crate, plan target was wrong); postright: n/a (planned files are LLM drafting (drafting.rs), not agent sessions; belongs with rightkit-llm or content crates)
7. **Boundary problems:** 
   - discovery.rs and driver_host/binary_lookup.rs both locate vendor CLIs
   - session.rs (blocking threads) and driver_host/cli_runtime/executor.rs (tokio) both run a vendor session
   - driver_host depends on rightkit-agent-tools, sandbox, app-settings, sidecar: heavy for apps that only want to parse stream-json
   - Third reader of Claude Code/Codex output: rightkit-agent-transcript (Pulse) parses the on-disk transcripts
8. **Before publishing:** 
   - Split: rightkit-agents (sans-IO protocol codecs + AgentEvent + discovery) and rightkit-agent-drivers (driver_host, providers, ACP)
   - One event model: map NormalizedEvent onto AgentEvent or the reverse, delete the other
   - One binary lookup
   - Remove 'coderight' strings; re-target Pulse transcript files to rightkit-agent-transcript and PostRight drafting to an LLM package

Depth: deep. Confidence: high.

**Side by side (verified by reading).**
- Original RightKit client: sans-IO `claude.rs` (1174 lines) and `codex.rs` (765), `AgentEvent` (`event.rs:152`), blocking `session.rs` (954), `discovery.rs` for PATH lookup. Used by `rightkit-code-audit-engine` with feature `claude`.
- CodeRight `driver_host/` (from `engine/crates/driver_host/src`, 29k planned lines): `DriverKind`, `LaunchSpec`, `NormalizedEvent` (`driver_host/contracts.rs:8-170`), tokio executor, recovery, attestation, provider profiles for claude, codex, cline, cursor, grok, commandcode, ACP transport. It reuses a few helpers from `crate::claude` (`claude_harness/runtime.rs:229`) and `crate::event::RateLimitInfo`, but has its own `claude.rs` (745) and `codex.rs` (504) and its own `binary_lookup.rs`.
- Pulse: its 15 planned files are `membrane-transcript` (reads Claude Code and Codex transcript files into `TranscriptEventV1`); 1,126 of 1,670 lines are in `rightkit-agent-transcript`, 94 in `rightkit-agents`. Pulse does not drive vendor sessions, so there is no competing behaviour here.
- PostRight: `crates/engine/src/drafting.rs`, `crates/protocol/src/drafting.rs` (339 lines), 10% matched; by name these are LLM drafting, not agent sessions (suspicion, not read).

**Whose shape won.** CodeRight's, added beside the older one. It is the right one for driving vendor CLIs (six providers, recovery, ACP); it has not yet replaced what it supersedes.

### rightkit-agent-tools

1. **Exists as:** `crates/rightkit-agent-tools`
2. **Feeds (planned lines):** coderight 86,283, genright 302
3. **Holds today:** RightKit's right-ctl tool interface extended with CodeRight's coding tools (94% of its lines).
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. One crate now carries both the cross-app tool interface every app needs and 50k lines of coding tools, with 25 RightKit deps (automation, browser, speech, ocr, ffmpeg, embed, llm, mcp). Reference: coderight.
6. **Per app:** coderight: fits; genright: gaps (its one file (302 lines) is not in; needs only the thin typed-tool interface)
7. **Boundary problems:** 
   - Thin tool contract and heavy coding tools in one crate
   - schedule_cron_tool/timezone.rs duplicates rightkit-scheduler
   - 18 'heardright' strings
8. **Before publishing:** 
   - Split a thin rightkit-agent-tool-api (trait, schema, right-ctl) from rightkit-agent-coding-tools
   - Use rightkit-scheduler timezone

Depth: light. Confidence: medium.

### rightkit-agent-host-traits

1. **Exists as:** `crates/rightkit-agent-host-traits`
2. **Feeds (planned lines):** no plan target feeds it
3. **Holds today:** CodeRight host seam traits (not fed by a plan target).
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `best`. Small trait crate used by core, tools, orchestration; nothing to reconcile. Reference: coderight.
6. **Per app:** n/a
7. **Boundary problems:** none found
8. **Before publishing:** none

Depth: light. Confidence: low.

### rightkit-agent-plugins

1. **Exists as:** `crates/rightkit-agent-plugins`
2. **Feeds (planned lines):** no plan target feeds it
3. **Holds today:** CodeRight plugin discovery.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `best`. Small, single consumer; not compared. Reference: coderight.
6. **Per app:** n/a
7. **Boundary problems:** none found
8. **Before publishing:** none

Depth: light. Confidence: low.

### rightkit-agent-recipes

1. **Exists as:** `crates/rightkit-agent-recipes`
2. **Feeds (planned lines):** no plan target feeds it
3. **Holds today:** CodeRight recipes.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `best`. Small; not compared. rightkit-prompt-recipes also exists (possible overlap, not checked). Reference: coderight.
6. **Per app:** n/a
7. **Boundary problems:** 
   - Possible overlap with rightkit-prompt-recipes (suspicion)
8. **Before publishing:** none

Depth: light. Confidence: low.

### rightkit-agent-skills

1. **Exists as:** `crates/rightkit-agent-skills`
2. **Feeds (planned lines):** no plan target feeds it
3. **Holds today:** CodeRight skills.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `best`. Not compared; Pulse's chat-bridge installs its own chat skill (suspicion of overlap). Reference: coderight.
6. **Per app:** n/a
7. **Boundary problems:** none found
8. **Before publishing:** none

Depth: light. Confidence: low.

### rightkit-agent-transcript

1. **Exists as:** `crates/rightkit-agent-transcript`
2. **Feeds (planned lines):** pulse 2,835
3. **Holds today:** Pulse's membrane-transcript crate (planned under rightkit-agents).
4. **Comparison:** single feeding app or no second implementation read; reference pulse.
5. **Shape:** `gaps`. Clean single-dep crate holding Pulse's normaliser; 16 'coderight' strings and CodeRight's native_transcript_exporter (1k lines, in agent-server) is a second transcript reader. Reference: pulse.
6. **Per app:** pulse: fits (67% matched; rest is tests); coderight: gaps (its exporter should use this)
7. **Boundary problems:** 
   - CodeRight native_transcript_exporter.rs in agent-server
8. **Before publishing:** 
   - Point CodeRight's exporter at TranscriptEventV1
   - Fix plan target

Depth: light. Confidence: medium.

### rightkit-llm

1. **Exists as:** `crates/rightkit-llm`
2. **Feeds (planned lines):** coderight 33,990, mailright 941
3. **Holds today:** RightKit's original BYOK clients (anthropic.rs, openai.rs, failover) plus CodeRight's agent_providers and agent_router (15.7k lines) behind features.
4. **Comparison:** single feeding app or no second implementation read; reference heardright/viewright for the plain client (existing users); coderight for agent providers.
5. **Shape:** `gaps`. Two Anthropic and two OpenAI clients in one crate; the agent_providers feature pulls rightkit-agent-policy and agent-engine-protocol into an LLM crate. Default features stay light, so simple apps are not harmed. Reference: heardright/viewright for the plain client (existing users); coderight for agent providers.
6. **Per app:** coderight: gaps (67% in target, 4.6k lines in agent-server); mailright: gaps (its 941 lines are not in (5% matched))
7. **Boundary problems:** 
   - anthropic.rs vs agent_providers/anthropic_client.rs; openai.rs vs agent_providers
   - agent_providers depends on agent-policy (inverted layering)
8. **Before publishing:** 
   - Move agent_providers and agent_router to rightkit-agent-llm; keep rightkit-llm as the wire clients both use
   - Compare MailRight's four files with the plain client and adopt or drop

Depth: light. Confidence: medium.

### rightkit-mcp

1. **Exists as:** `crates/rightkit-mcp`
2. **Feeds (planned lines):** coderight 10,970
3. **Holds today:** RightKit MCP client/server with CodeRight's (74%).
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. Depends on agent-engine-protocol and sandbox; 1.1k CodeRight lines still in agent-server. Reference: coderight.
6. **Per app:** coderight: fits
7. **Boundary problems:** 
   - Depends on agent-engine-protocol
8. **Before publishing:** 
   - Not read for behaviour

Depth: light. Confidence: low.

### rightkit-workflow

1. **Exists as:** `crates/rightkit-workflow`
2. **Feeds (planned lines):** coderight 26,076
3. **Holds today:** RightKit scenario runtime; under half of CodeRight's workflow code.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. 6.6k of CodeRight's workflow lines are in agent-server/src/workflow (7.2k) instead; two workflow engines. Reference: coderight.
6. **Per app:** coderight: gaps
7. **Boundary problems:** 
   - agent-server/src/workflow
8. **Before publishing:** 
   - Decide whether CodeRight workflows run on this state machine; move or delete the agent-server copy

Depth: light. Confidence: medium.

### rightkit-hooks

1. **Exists as:** `crates/rightkit-hooks`
2. **Feeds (planned lines):** coderight 5,423
3. **Holds today:** Claude Code/Codex hook payloads; CodeRight 66% in; also used by Pulse's chat-bridge.
4. **Comparison:** single feeding app or no second implementation read; reference pulse.
5. **Shape:** `gaps`. Fed in practice by two apps (CodeRight plan, Pulse via chat-bridge); depends on rightkit-llm and sandbox, and has 18 Pulse strings. Reference: pulse.
6. **Per app:** coderight: fits
7. **Boundary problems:** 
   - Depends on rightkit-llm
   - Pulse strings
8. **Before publishing:** 
   - Drop the llm dependency or gate it
   - Remove product names

Depth: light. Confidence: low.

### rightkit-sandbox

1. **Exists as:** `crates/rightkit-sandbox`
2. **Feeds (planned lines):** coderight 7,177
3. **Holds today:** RightKit sandbox; a quarter of CodeRight's sandbox code.
4. **Comparison:** single feeding app or no second implementation read; reference coderight.
5. **Shape:** `gaps`. 2.6k of CodeRight's sandbox lines are in agent-server; 10 'coderight' strings. Reference: coderight.
6. **Per app:** coderight: gaps
7. **Boundary problems:** 
   - Sandbox code in agent-server
8. **Before publishing:** 
   - Move it

Depth: light. Confidence: low.

### rightkit-pty

1. **Exists as:** `crates/rightkit-pty`
2. **Feeds (planned lines):** no plan target feeds it
3. **Holds today:** RightKit's own.
4. **Comparison:** single feeding app or no second implementation read; reference none.
5. **Shape:** `gaps`. Not fed by the plan. rightkit-remote-gateway uses portable-pty directly for term.rs instead of this crate. Reference: none.
6. **Per app:** n/a
7. **Boundary problems:** 
   - rightkit-remote-gateway/term.rs uses portable-pty, not rightkit-pty
8. **Before publishing:** 
   - Port remote-gateway terminals to rightkit-pty or state why not

Depth: light. Confidence: medium.

### rightkit-remote-gateway

1. **Exists as:** `crates/rightkit-remote-gateway`
2. **Feeds (planned lines):** pulse 20,758
3. **Holds today:** Pulse core/src/remote, 75% of its planned gateway lines.
4. **Comparison:** see below.
5. **Shape:** `gaps`. Faithful lift of the daily-used behaviour, but it is a product bundle (20 tool families, ChatGPT door, media apps, saved logins and payments) on 15+ RightKit crates rather than a gateway core plus tools. Reference: pulse.
6. **Per app:** pulse: fits
7. **Boundary problems:** 
   - See rightkit-agent-gateway: shares no core with MailRight's gateway
   - approvals.rs is a third pending-approval queue
   - agent_secrets.rs (2k lines: saved logins, virtual cards, spend ledger) is a separate product capability
8. **Before publishing:** 
   - Rebase on the gateway core and approvals core
   - Split agent_secrets into its own crate
   - Remove Pulse and ChatGPT names from library text

Depth: deep. Confidence: high.

See `rightkit-agent-gateway` for the side-by-side table.
