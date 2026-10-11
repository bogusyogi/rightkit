# Decision: agent-session extraction (Pulse "Chat engine" to RightKit)

Status: adjudication, read-only. I changed no files. Date 2026-10-11. I could not run commands (no shell), so `rightkit owns` and all tests are unrun. Claims about Claude Code's own files and behavior are as stated in Pulse's docs and code comments, not observed by me.

## 0. Verdict in five lines

1. Do not merge this into `agent-room-core`. Create two new crates, `rightkit-agent-sessions` (read-only) and `rightkit-agent-chat` (writes into a live session). `agent-room-core` stays untouched.
2. Catalog slugs are `agent-sessions` and `agent-chat`. Both crates are continuous owners and declare `runtime-budget.json`.
3. Both set `rust-version = "1.98.0"` and `edition = "2024"`. The 1.90 pin in the rightkit-send brief is stale.
4. Pulse's bridge has 3 `#[test]`s today, and none covers a B-finding. "Parity against both engines" therefore needs a characterization suite first (M0).
5. A copy hosted in the Pulse repo is not compatible with RightKit's invariants. Develop in `rightkit/crates/` from day one. Consumers use a dev-only local path patch until a version is published.

## 1. One crate or two

### What agent-room-core is
- It is a shared review room. Seats (agents and a human) exchange events, findings, dispositions and seal receipts (`agent-room-core/README.md:3`).
- Modules: `protocol` (the `Room` state machine), `store` (bundled SQLite), `auth` (host-minted seat tokens), `runtime` (`runtime.json`), `server` (axum HTTP and SSE), and the `room` bin (`README.md:9-16`).
- It is `publish = false` and UNLICENSED, with `rust-version = "1.98.0"` (`Cargo.toml:3-7`).
- The README states its scope: "Do not use it for general chat ... The crate has no remote transport beyond a local HTTP host" (`README.md:23`).
- Seat launch is Windows-only, and the crate is "not verified off macOS" (`README.md:40,53`).

### Consumers
- A grep of `Cargo.toml`, `*.rs` and `*.lock` under `/Volumes/D/claude` finds the crate only in its own tests and bin, `Cargo.lock`, and one mention in `rightkit-oauth/examples/server_axum.rs`.
- `@rightkit/agent-room-mcp` talks HTTP to a room server and declares "Rust pair: None" (`packages/agent-room-mcp/README.md:8`).
- The capability entry `agent-room` is `declaration-only` (`rightkit-ownership.json`; `CAPABILITY-MAP.md:16`).
- I found no consumer in pulse, legion, membrane, genright, cutright, heardright or postright. The coderight checkout could not be searched, because the glob timed out.

### Pulse's bridge
- It discovers running chats (`roster.rs`) and writes a wrapped frame into another process's Unix socket or named pipe (`deliver_claude.rs`).
- It shells to `codex queue` (`deliver_codex.rs`) and registers itself in Claude's own session registry (`hub.rs:113-220`).
- It listens on sockets and pipes for replies (`ReplyHub`).

### Why not one crate
- **No shared types and no shared runtime.** One is a persistent multi-seat state machine with a server. The other is a stateless-per-call adapter to a foreign process's private protocol.
- **Different trust models.**
  - agent-room: host-minted, seat-bound bearer tokens over local HTTP.
  - bridge: the same-OS-user boundary plus Claude's own `peerToken`, with no identity for individual chats (B01).
- **Weight and platform.** Merging would force bundled SQLite, axum, tokio and tokio-stream onto CodeRight's phone connector. It would also tie a crate that is unverified off macOS to one that must work on macOS and Windows.
- **Release cadence.** The bridge's cadence is set by Claude Code and Codex releases. The room's is set by its own protocol (CHANGELOG 0.3.x). The room is private and the sessions crates are planned public, which `CARGO-PUBLICATION.md:10` shows are held to different publication policies.
- **Possible future link.** A room seat adapter could call `rightkit-agent-chat` to inject room events into a live Claude session. That is a one-way dependency, room to chat, to be decided when it is needed. No type moves between them now.

### Why two new crates, not one
This is a closer call than the first question.
- **Trust.** `agent-sessions` only reads (`~/.claude/sessions`, `projects/**/*.jsonl`, the Codex state DB). `agent-chat` writes into a running assistant (sockets and pipes, the registry entry for a poster, `codex queue`, key files). A read-only crate can be reviewed lighter, and consumers (such as Pulse's notch status) can take it without the injection code.
- **Drift cadence.** The transcript format changes between Claude releases (`ClaudeTranscript.swift:19-25,89-96`: bookkeeping record kinds "are added to between releases"). The wire protocol is pinned to `peerProtocol 1` and the 2.1.295 wrapper rules. A history-parser fix must not force re-qualification of the injector.
- **Dependency direction.** `agent-chat` depends on `agent-sessions`; the reverse must not happen. The shared type is `SessionRef`/`SessionRecord`: `{kind, cli_session_id, host_session_id, pid, proc_start, pid_domain, config_root, messaging_socket, peer_protocol, version?}`.
- **Key files.** `agent-sessions` never reads key files or `peerToken`. `agent-chat` reads them.

### Rejected alternatives
- **Fold into `agent-room-core`.** Rejected for the reasons above.
- **One crate with features `discover`, `history`, `deliver`.** Cargo feature unification means any dependent enabling `deliver` enables it for every dependent in that build. That undoes the read-only/write split. It also forces one release, one budget file and one re-qualification for both halves.
- **Three crates (sessions, history, chat).** `history` needs `SessionRecord` to resolve a transcript path, and they share a drift owner (Claude's on-disk state). The split buys nothing.
- **Name `rightkit-transcript` for the reader.** That crate already owns ASR transcript contracts (`rightkit-transcript/src/parse.rs`, `align.rs`), so the name collides. The module is `history`, and the docs say "agent history".

## 2. Names, features, toolchain

### Crates and slugs

| Crate | Slug | Class | Notes |
|---|---|---|---|
| `rightkit-agent-sessions` | `agent-sessions` | continuous | watcher, tail follow |
| `rightkit-agent-chat` | `agent-chat` | continuous | reply listener, poster registration |

- Before creating either, run `rightkit owns agent-sessions` and `rightkit owns agent-chat`. My greps of `CAPABILITY-MAP.md`, `INDEX.md` and the crate manifests found nothing. `agent-room` and `agent-tools` exist but do not overlap.
- Ownership files should use `detection: "declaration-only"` first. Add fingerprints only after M5, as agent-room-core's file does for lack of them.
- Only Adrian-authenticated trust-sync activates ownership. These briefs request it; they do not do it.

### Features

| Crate | Feature | Default | Adds |
|---|---|---|---|
| sessions | `claude` | on | session registry roster and history |
| sessions | `codex` | off | thread discovery from `state_<N>.sqlite` via `rightkit-db` (read-only). CodeRight does not need it. Pulse enables it. |
| sessions | `watch` | on | `watch_sessions`, tail follow |
| chat | `claude` | on | socket/pipe delivery, poster, reply listener |
| chat | `codex` | off | `codex queue` delivery, capability probe |

Library only: no bin, no tokio, threads and blocking I/O as in Pulse (`deliver_claude.rs:717`). A feature must not change behavior of existing items.

### Toolchain
- Pulse pins `1.98.0` (`pulse/rust-toolchain.toml:2`; `pulse/Cargo.toml:8`; edition 2024 at `Cargo.toml:7`) and uses let-chains (`deliver_claude.rs:185-186,206-207`).
- The crates use `rust-version = "1.98.0"` and edition 2024. Every other `rightkit-*` crate I checked is 1.98.0 (db, fs, logs, secrets, watch, transcript), so they may depend on each other.
- What 1.98.0 forbids:
  - Any API or feature stabilized after 1.98.0.
  - The `rightkit-send` precedent of staying on 1.90 and avoiding 1.98 siblings (`rightkit-send/Cargo.toml:5`) does not apply here.
  - Anything that would raise the floor above Pulse's pin is refused until Pulse moves its pin.
- Unverified: CodeRight's toolchain. My grep found no matching `rust-version`/`channel`. The M1 CodeRight consumer check must record it. If CodeRight is below 1.98.0, that is a blocker to raise with Adrian, not a reason to lower the crate.
- Dependencies: `serde`, `serde_json`, `libc`, `thiserror`, `getrandom`, `sysinfo` (pin as Pulse does, `=0.37.2`; confirm it fits the status API), and `windows =0.61.3` with Pulse's feature set (`pulse/core/Cargo.toml:34-44`). Nothing from Pulse's `crate::` namespace.

## 3. Module split

### Moves to `rightkit-agent-sessions`
- `roster.rs`: `parse_claude`, `claude_sessions_in`, `codex_status`, `live_pids`, `title_for`.
- `deliver_claude.rs` items: `sessions_dir`, `start_identity`, `text_of`, `process_matches` and the Windows `win::parse_start`/`creation_epoch_secs`.
- Under `codex`: `list_threads`, `find_thread_by_id`, `DiscoveryError`, `codex_home`, and the stale-cache-with-age behavior (`deliver_codex.rs:28-37`).
- `is_chat` (`hub.rs:107-109`), as `SessionRef::matches(id)` covering `hostSessionId`.

### Moves to `rightkit-agent-chat`
- The Claude wire: `open_session` (`:268-353`), `connect`, `classify`, `wrap_content`, `defuse_tags`, `sender_label`, `attr_safe`, `is_format_char`, `parse_wrapper`, `deliver_via`'s ack loop, `status_receipt`/`record_status`.
- The Windows overlapped-pipe module.
- `ReplyHub`, `reply_pipe_name`, `reply_socket_path`.
- `register()` (`hub.rs:113-220`), as `Poster`.
- Under `codex`: `codex queue`, `codex_binary`, `codex_capability`.
- Receipts and errors, as public typed enums with no English-only API: `ReceiptState` (delivered, queued, sent, refused, unsupported, unknown) and `DeliverError`.

### Stays in Pulse
- `envelope.rs` and `links.rs` (ssh, base64).
- `store.rs` (inbox, registered chats, reply routes kept 24 h).
- `control.rs`.
- Hub heartbeat and UI, `install.rs`, the skill.
- The `Held` state. The crate has no inbox, so Held is a Pulse-layer receipt (`mod.rs:21-23`).
- The bridge on/off switch (B03). The crate offers `Engine::disable()`; Pulse owns the policy.
- The peer-key to remote-device mapping (`note_last_peer`/`last_peer`).
- The product `via Pulse` suffix.

### Protocol authority, kept by Pulse
- A checked-in `protocol/claude-peer.json` carries the wire facts: `peerProtocol`, wrapper attribute order, the allowed `from-mode` values, the sender-name rules, and a version table. Each row records the Claude Code version proven, evidence reference and date, plus the same for Windows.
- Pulse is the only party that may add or change rows. A CHANGELOG entry that touches that file must name the Pulse approval.
- The crate treats the file as data to check against, not as a negotiable default.

### Defects to fix, not copy
- Global statics (`CHAT_SOCKET_DIR`, `hub_slot`, last-peer maps) become instance state on a `Poster`/`Engine` value.
- `hub.rs:185` calls `crate::localsend::proto::random_hex` and `deliver_codex.rs:42` imports `rightkit_db`. Both are gated on Pulse's `localsend` feature (`core/Cargo.toml:27,32`), so the bridge does not build with `--no-default-features`.
  - The crate uses `getrandom`.
  - **Ordering hazard:** the rightkit-send extraction deletes `core/src/localsend/` (send brief, line 3). Pulse's bridge must be unhooked from `random_hex` first, or both extractions break each other.
- `hub.rs:212-217` writes the key file before `chmod 0600` (B12). Create it with `0600` atomically (`O_EXCL`, mode set at open).
- `wrap_content` hard-codes `from-mode="bypass"` (see conflicts, §9).
- The roster excludes any `entrypoint` other than `claude-desktop` and `claude-vscode` (`roster.rs:139-144`), and reads one config root (`deliver_claude.rs:151-156`).
- The poster's registry `entrypoint` must be a per-product constant. Roster consumers must exclude all known poster entrypoints, not just `pulse-hub`.

### API shapes for the consumer's list

**Sessions**
- `list_sessions(&Scan) -> Vec<SessionRecord>` and `watch_sessions(&Scan) -> SessionWatcher`, which yields `Added`/`Changed`/`Removed` events.
- `Scan { config_roots: Vec<PathBuf> }`. If it is empty, use `CLAUDE_CONFIG_DIR` or the default. Each record carries the `config_root` it came from, so CodeRight can show an account/profile.
- `kind` is `desktop | cli | other(entrypoint)`, from `entrypoint`. Unknown values do not become deliverable.
- Fields:
  - `status`: only what the registry says. Desktop sessions write none (`ClaudeTranscript.swift:5-10`), so `status` is `Option`, with a separate `History::turn_state()` fallback.
  - `alive`: a typed enum `Live | Stale | Unknown`, not a bool. B14 says liveness is a guess.
  - `transcript_path`: `Option<PathBuf>`, resolved by slug first, then a cached scan (see `ClaudeTranscript.swift:36-69`, which maps `/` and `.` to `-`).
  - Windows slug rule: unverified. M1 must capture it from a real Windows install.

**Chat**
- `Poster::register(name) -> Poster` (clean unregister on `Drop`), `poster.deliver(&SessionRef, &Outbound, deadline) -> Receipt`, and `poster.replies() -> Receiver<Reply>`.
- The free `deliver(session, text, from_name, deadline)` the consumer listed is impossible without a registered poster. macOS resolves the poster by process ancestry and the status notices go only to the poster's socket (`bridge.md:39`; `hub.rs:113`). Deliver is a method on the poster.
- `Reply { msg_id, in_reply_to, host_session_id, from_session_id, text, origin: Unverified | PeerChecked }`.

## 4. Trust and safety boundary

### Invariants that must be named tests
These are the B-findings that belong to this engine. B-findings about ssh, inbox, CLI, skill or Telegram stay in Pulse and are listed at the end.

| Id | Carried invariant (one line) | Test name |
|---|---|---|
| B01 | A reply is never presented as verified: `origin` is `Unverified` unless peer credentials (LOCAL_PEERPID, or named-pipe client process) match the session's pid or a descendant. A forged `from-session` from another process is dropped when peer-checking is available. | `b01_reply_origin_never_overstated` |
| B01 | `from_name` is passed through the label cleaner, so a caller cannot choose another chat's identity string. | `b01_sender_label_sanitized` |
| B02 | Reply guidance and wrapper never contain a caller-controlled shell or command fragment; `from-mode` is not a hard-coded claim of authority. | `b02_no_command_in_wrapper`, `b02_from_mode_follows_target_class` |
| B03 | A disabled engine returns `Refused(Disabled)` from deliver, listener and register, queuing nothing. | `b03_disabled_engine_refuses_all` |
| B05 | One `msg_id` is never delivered twice by the engine itself: no automatic retry after a timeout, and the receipt for an ambiguous send is `Unknown`. | `b05_no_internal_retry`, `b05_reused_msg_id_rejected` |
| B08 | Silence or EOF without a verdict is `Sent`, never `Delivered`. `Queued` is only an explicit accept/hold. | `b08_silence_is_sent_not_delivered` |
| B09 | Delivery refuses if the registry's `sessionId`, `procStart` or `pidDomain` changed since the scan, and picks the key file by matching start, not by first filename. | `b09_reused_pid_refused`, `b09_key_generation_selected` |
| B12 | Key files are `0600` and owned by the user at creation. Session dir and files must be owned by us with no group/world write. Session ids are never used as a path component. | `b12_key_created_0600_atomically`, `b12_foreign_owner_refused`, `b12_id_never_joined_to_path` |
| B14 | Liveness is `Live`/`Stale`/`Unknown`; a reused pid never reads as `Live`; a future timestamp is never "recent". | `b14_reused_pid_not_live`, `b14_future_timestamp_not_recent` |
| B15 | A failed Codex discovery is an error with cache age, never "no chats"; an exact id resolves outside the newest-100 cap. | `b15_discovery_failure_is_error`, `b15_exact_id_beyond_cap` |
| B17 | A Codex binary without `queue` is `Unsupported`, probed once per path and mtime. | `b17_missing_queue_unsupported` |
| B18 | Bodies are bounded before allocation; a body that fits only when raw but not once serialized is refused with a typed error; Windows argv limit (8000) and Unix (100000) are enforced. | `b18_oversize_refused_before_alloc`, `b18_argv_limit_per_os` |
| B19 | Ack reader stops at 10 s or 64 KiB; replies at 30 s, 1 MiB per connection, 64 connections; stop joins threads. | `b19_ack_bounds`, `b19_reply_bounds`, `b19_stop_joins` |
| B20 | Listener cleanup is by generation: an old thread never unlinks a replacement's socket; bind failure returns reply-unavailable, never a silent placeholder-only success. | `b20_stale_cleanup_keeps_new_socket`, `b20_bind_failure_surfaced` |
| B21 | A second poster in the same session does not silently duplicate; the Windows reply pipe is current-user-only DACL. | `b21_pipe_dacl_current_user`, `b21_duplicate_poster_refused` |

**Stays in Pulse (not this engine):** B04 and B11 (inbox), B06 (route persistence), B07 (aliases), B10 (ssh), B13 and B16, B22 and B23 (UI and lifecycle), B24 to B27 (CLI, skill, install, Pulse E2E).

### What the crate must refuse (fail closed, typed)
- **Unknown kind.** Only `claude` and `codex` (feature-gated) are deliverable. `other(entrypoint)` and any unrecognized `kind` yields `Unsupported(KindUnknown)`.
- **Version skew.**
  - `peerProtocol != 1` gives `Unsupported` (today's gate, `deliver_claude.rs:285-290`).
  - Pulse does not read the Claude Code version today. Only `peerProtocol` is gated, but the docs say the wrapper rules are "proven against Claude Code 2.1.295" (`bridge.md:38`).
  - M1 must find where the real session file records the CLI version. I could not verify that Claude writes a `version` field. Pulse writes one in its own record (`hub.rs:192`), which suggests it mirrors Claude's schema.
  - If the version is absent or not in the protocol table: `Unsupported(VersionUnproven{seen, proven})`. This is the default. An explicit `Policy::unproven = AttemptAndReport` lets CodeRight try anyway, and then every receipt is stamped `unproven_version`.
  - The cost is real: Claude auto-updates, so each release needs a Pulse re-proof of the table. That is the intended price of protocol authority.
- **Sockets and pipes not owned by the user.**
  - Unix: `lstat` the path. It must be a socket owned by our uid, in a parent that is ours or sticky-and-not-world-writable (`/tmp/cc-socks`). After connecting, check the server's uid with `getpeereid`.
  - Windows: reject names with a host component other than `.`. Check the server process's owner SID after connecting (`GetNamedPipeServerProcessId`), and use `SECURITY_SQOS_PRESENT` with anonymous impersonation level on the client.
  - The listener's pipe uses the current-user DACL and `PIPE_REJECT_REMOTE_CLIENTS`.
- **Path traversal.** Session ids must match a strict charset and length. They are never joined to a path. Registry filenames are parsed as `<u32 pid>.json`. A transcript path is canonicalized and must stay under the projects root (no symlink escape).
- **Oversized frames.**
  - Body at most 64 KiB (`envelope.rs:11`), also bounded by the serialized frame.
  - Ack 64 KiB, reply 1 MiB, 64 connections (`deliver_claude.rs:60-67`).
  - Max JSONL line size on the history reader; an oversize line becomes an `Oversize` event, not an allocation.
- **Silent drop hazard.** The doc says an off-form wrapper is parked and then dropped "without a word" while the receipt says Sent (`deliver_claude.rs:553-556`). Because a drop looks like success, the engine treats an unexplained `held`/`dropped` notice as drift (below).

### Drift detection, not guessing
1. A static table (`protocol/claude-peer.json`) plus the version gate above.
2. A runtime drift latch: if Claude sends a `held`, `refused` or `dropped` notice for a message the engine built in the proven form, the engine moves to `Degraded` and refuses further delivers until restarted or a table update. `status()` exposes the latch.
3. An unknown top-level record type or action from the chat is surfaced as `Unrecognized{raw}` and counted. It is never mapped to Delivered.
4. `classify` (`:403-449`) matches `action.contains("hold")`, `contains("refus")`, `contains("auth")`: substring matching on host-controlled text. Replace with exact match against the table, and log near-misses as unrecognized.
5. A Codex probe runs `queue --help` once per binary path and mtime (`deliver_codex.rs:12-13`).

## 5. Runtime budget class

Both crates are continuous. The gate scans non-test `src` for thread-spawn, sleep, timer, listener and watcher patterns (`RUNTIME-BUDGETS.md:63`), and both crates match. A `continuous = false` declaration would be refused. Each carries `runtime-budget.json`.

### Continuous parts and what to measure
- `watch_sessions`: directory polling and liveness checks. Measure idle CPU, wake-ups/s and disk ops/s at the declared scale. Also measure the cost of `ps`-per-session on macOS (`start_identity` runs `/bin/ps` per session, `deliver_claude.rs:189`). That may need a batched call.
- `History::tail`: per-file `stat` at a stated interval, and a read only when a file grows (the Swift reader's pattern, `ClaudeTranscript.swift:147-150`). Measure resident memory with many files followed, and read amplification on a multi-megabyte file.
- Reply listener: threads and fds per peer, the 64-connection cap, memory per connection. Measure with 64 held-open connections.
- Poster registration: the registry entry and key file are written once, plus cleanup on exit.

### Required fields
- Ceilings: resident bytes, idle CPU %, wake-ups/s, disk ops/s, each non-null.
- `requiredScale`, both with a unit and minimum count: for sessions, `{unit: "sessions", minCount: 200}` with 2000 transcript files; for chat, `{unit: "connections", minCount: 64}`.
- `status()` exposing `residentMemoryBytes` as a `pub fn`/`pub struct` in the declared file, and printing the exact `RESULT budget ...` line.
- Measured on darwin and win32 (both required hosts), and the first consumer run is recorded at `installed` scope (Pulse hub or CodeRight daemon).

### Ceilings: a proposal, not a source
Per the rule, ceilings are never inferred (`RUNTIME-BUDGETS.md:14`). The rightkit-send file is still all-null and refused (`rightkit-send/runtime-budget.json`). So the owner must state the ceilings and this brief becomes the cited source once approved. My proposal:
- Sessions: 40 MiB resident, 0.5% idle CPU, 2 wake-ups/s, 20 disk ops/s.
- Chat: 20 MiB resident, 0.1% idle CPU, 1 wake-up/s, 1 disk op/s.
Both need ratification by Adrian or the RightKit lead before they go into the JSON.

## 6. Parity plan and milestones

### Fact that reshapes the plan
Pulse's bridge has 3 `#[test]`s: `envelope.rs:189`, `links.rs:646` and `tests/bridge.rs:611`, the last a fake-ssh single-OS run (the B26 finding still stands). There is no suite to "run against both engines". So M0 builds one against Pulse's current engine, then every later milestone must pass it.

### Parity rig
- Shared vectors (JSON fixtures) plus a fake Claude peer rig: a temp config root with a `<pid>.json` and key file, a socket or pipe that records frames, and scripted verdicts.
- A thin `Engine` trait with two adapters: the crate, and a Pulse adapter inside the Pulse repo that wraps `pulse_core::bridge` against the same vectors. The vectors are copied by hash, not shared by path.
- Vectors cover:
  - Byte-exact wire frames (`deliver_claude.rs:672-681`).
  - Wrapper strings, including `defuse_tags` and `sender_label` edge cases (names over 64 chars, format characters, tag fragments).
  - The verdict classification table (`classify`) and the `status_receipt` mapping, including `expired`+`refused`.
  - Receipt state by scenario.
  - Roster fixtures (stale pid, reused pid, desktop vs vscode vs cli, hostSessionId).
  - The Windows reply-pipe name.
- Where Pulse's behavior is a recorded defect, the vector marks `pulse: legacy`, `crate: fixed`, and Pulse's adapter is allowed to differ only on those listed vectors.

### Milestones (each independently verifiable)

| # | Deliverable | Acceptance | Executor |
|---|---|---|---|
| M0 | Parity rig and vectors against Pulse's engine; list of defect rows | Vectors pass on Pulse unchanged on macOS and Windows; each defect row cites path:line | Sonnet. A wrong vector compiles and silently blesses a bug. Oracle reviews the vectors. |
| M1 | `agent-sessions`: roster, profiles, `cli` kind, `transcript_path`, version source captured from a real 2.1.295 file on each OS | Roster vectors pass; B09/B12/B14/B15 tests; CodeRight consumer toolchain recorded | Haiku for the move, Sonnet for identity checks (pid reuse is subtle) |
| M2 | **NEW** `History`: paged both ways, `tail`, typed events, cursor, rotation, half-written last line | See below | Sonnet; Opus (Sage) reviews cursor/rotation design before code |
| M3 | `watch_sessions`, `status()`, budget bench, `runtime-budget.json` for sessions | RESULT line on both OSes at the required scale | Sonnet. Windows and concurrency, with OS calls. |
| M4 | `agent-chat` deliver (Claude, Unix first), receipts, version gate, drift latch, refusal rules | Wire and verdict vectors; B02/B05/B08/B09/B12/B18 tests | Sonnet; Oracle reviews the trust boundary |
| M5 | `Poster` register, reply listener, Windows pipe, peer-credential check, budget | B01/B03/B19/B20/B21 tests; `Drop` unregisters; installed run with a real chat | Sonnet, not Haiku: unsafe FFI and privileged pipe code. Oracle review. |
| M6 | `codex` feature; Pulse re-pins | B15/B17 tests; vectors pass on both engines; Pulse deletes its copy in the same change | Haiku for the move, with M0 vectors as the check |

### First cut CodeRight can pin
- Minimum: M1 through M5 for Claude, without the `codex` feature. That delivers items (1) through (5) of the consumer's list.
- CodeRight can pin at M5. Pulse re-pins only after M6 and a real two-OS run.
- No milestone includes publication. Publication goes through `right-release publish cargo` by the RightKit lead, and each crate needs its own budget file at the published version.

### M2 acceptance (the new reader)
- **Cursor.** Opaque to callers: `{file_identity, offset, generation}`.
  - File identity is dev+inode on Unix and the volume/file index on Windows.
  - Offsets are of line starts. Cursors are monotonic within a generation.
- **Paging.** `page_forward(cursor, n)` and `page_backward(cursor, n)` read at most `n` events each. Backward reads work from the file tail in blocks, discard a leading partial line, and never return a half-parsed line.
- **Tail.** `tail(cursor)` returns events appended since the cursor, with a poll loop or wake-up supplied by the caller; it does not require a thread of its own.
- **Half-written last line.** A final line without `\n` is not emitted, and the cursor does not advance past it. It is emitted once terminated. Tested with a writer that splits a record across two appends.
- **Rotation.** If the file identity changes, or the file is shorter than the cursor, the reader emits `Reset{generation}` and restarts at offset 0. It never reads garbage past the end or skips silently.
- **Events (typed, non-exhaustive).** `UserText`, `AssistantText`, `ToolUse`, `ToolResult`, `Interruption`, `Meta{kind}` and `Unrecognized{kind, raw_len}` for bookkeeping types. Sidechain records carry `is_sidechain` (the Swift reader skips them for turn state, `ClaudeTranscript.swift:110`).
- **Malformed and oversize.** A bad JSON line mid-file emits `Malformed{offset, len}`; a line over the cap emits `Oversize`. The reader never aborts a page.
- **Fixtures.** Golden JSONL captured from real 2.1.295 sessions on each OS (sanitized, Adrian's machines), plus synthetic edge files. No fixtures written from memory.
- **Compatibility.** The Swift `turn(inTail:)` is reproduced as a function over the same fixtures, so the notch could adopt the Rust reader later without behavior change.

## 7. Interim: Pulse hosting the first cut

### Not compatible with RightKit's invariants
- **Shared source in the Pulse repo.** "Keep shared source only under `rightkit/`" (`rightkit/AGENTS.md`, Locked invariants).
- **CodeRight pinning a Pulse commit.** "Make apps consume published `@rightkit/*` without Git, link, file, or workspace specs." A Git commit pin of a Pulse subdirectory is a Git spec.
- **A second copy in Pulse after RightKit has one.** Adrian rejected a long-lived second copy for rightkit-send (`rightkit-send-design.md:3`: "no long-lived second copy").
- **Copy-vendoring into CodeRight,** or CodeRight reimplementing roster/delivery meanwhile.

### Compatible
- **Develop in `rightkit/crates/` from the first commit.** The owner repo hosts the work. Pulse's chat contributes parity vectors and protocol facts, and is the sign-off party, without hosting the code.
- **A dev-only local path patch.** Pulse and CodeRight can point at the unpublished crate with a local `[patch.crates-io]` path during development. The precedent is that apps already do this for second-wave crates (`PLATFORM-MATRIX.md:44`: genright lists `rightkit-align` "only under `[patch.crates-io]`").
  - Their committed `Cargo.toml` names an exact version (`=0.1.0`).
  - The patch must not appear in any release build, and the release gate must refuse a path spec.
  - Verify that with the actual `right-release` checks before relying on it.
- **Pulse's adapter and vectors can live in the Pulse repo.** They are tests, not shared source.
- **First cut shape before publication.** `publish = false` like `rightkit-send` and `agent-room-core`. When a version is published, consumers move to it and the patch is deleted in the same change.

## 8. Risks and decisions for Adrian

1. **Ceilings.** Ratify or change the budget proposals in §5. Without them the publish gate refuses both crates.
2. **Poster identity.** CodeRight's daemon must register a per-product `entrypoint` in Claude's registry. That adds an entry to every other tool that lists sessions. It needs a one-line coordination with Pulse's roster filter.
3. **`from-mode`.** What the engine claims to Claude about permission mode is a Pulse authority decision (conflict 1 below). The crate cannot choose it alone.
4. **CLI sessions.** CodeRight's `kind: cli` wants delivery to terminal sessions. Pulse deliberately excludes them (`roster.rs:135-139`: reached "over ssh, not the bridge"). I did not verify that a terminal session exposes `messagingSocketPath`. M1 must prove it on both OSes before `cli` is deliverable.
5. **Pulse re-pin date** depends on the rightkit-send extraction removing `localsend` (see §3 ordering hazard).

## 9. Doc and code conflicts (path:line)

1. **from-mode.** `docs/bridge.md:38` says `from-mode` equals the chat's own class (`bypass` for a bypass chat, else `prompting`). The code always writes `from-mode="bypass"` (`deliver_claude.rs:560`), although its comment at `:551-556` describes class matching. `bridge-astra-review.md` B02 asks for removal of this assertion.
2. **Claude version.** `deliver_claude.rs:11` says the wire was captured from 2.1.293. `bridge.md:38` and `deliver_claude.rs:551` say 2.1.295.
3. **Windows reply pipe.** `deliver_claude.rs:33` describes one pipe per peer (`pulse-bridge-<fnv>`). The code uses one shared `cc-msg-<32 hex>` pipe and ignores the peer key (`:1007-1020`), as `bridge.md:41` says.
4. **Windows unsupported.** `mod.rs:96` and `mod.rs:172-173` list "Windows" among reasons a chat cannot be pushed to. The code has a full Windows pipe client (`deliver_claude.rs:369-373`) and `bridge.md:42` documents it. The review's matrix also called the Windows reply listener missing (B25); it exists.
5. **Handoff wrapper.** `coderight-handoff.md:14` gives `from-mode="bridge" provenance="agent-unverified"` and a chat pipe `\\.\pipe\LOCAL\cc-msg-<hash>`. The code writes four attributes only, with `from-mode="bypass"` (`deliver_claude.rs:560`), and reads the chat's socket path from the session file (`:291-295`). A CodeRight author following the handoff would build a wrapper Claude drops.
6. **Handoff roster claims.** `coderight-handoff.md:13` implies `$CLAUDE_CONFIG_DIR` profiles, busy/idle/waiting status and both Desktop and CLI chats. The code reads one root (`deliver_claude.rs:151-156`), Desktop sessions write no status (`ClaudeTranscript.swift:5-10`), and the roster drops `cli` (`roster.rs:139-144`).
7. **Stale review items.** `bridge-astra-review.md` was written at commit a619e38f. The code now addresses B03 (`mod.rs:26-28`), B04 (`mod.rs:21-23`), B08 (`deliver_claude.rs:749-751`), B09 (`:268-352`), B15 (`deliver_codex.rs:28-37`) and B17 (`:12-13`). B01 (`deliver_claude.rs:1315-1335`, the auth frame is ignored and only a known session id is checked) and B12 (`hub.rs:212-217`) remain open. None of the fixed items has a test, since the bridge still has 3.
8. **Build coupling.** The bridge needs the `localsend` feature to compile (`hub.rs:185`, `deliver_codex.rs:42`, `core/Cargo.toml:27,32`). Nothing in `bridge.md` mentions it.
9. **Registration without a start time.** `hub.rs:206-207` registers the hub with no `procStart`, so readers check only that the pid is running (the weaker identity check, B09). A CodeRight poster should write one.

## Files read
- `/Volumes/D/claude/rightkit/crates/agent-room-core/{README.md,Cargo.toml,rightkit-ownership.json}`
- `/Volumes/D/claude/rightkit/packages/agent-room-mcp/README.md`
- `/Volumes/D/claude/rightkit/docs/{RUNTIME-BUDGETS.md,CAPABILITY-MAP.md,PLATFORM-MATRIX.md,CARGO-PUBLICATION.md,plans/2026-10-10-rightkit-send-design.md}`
- `/Volumes/D/claude/rightkit/crates/{rightkit-send/runtime-budget.json,rightkit-agent-tools/README.md}`
- `/Volumes/D/claude/pulse/{rust-toolchain.toml,Cargo.toml,core/Cargo.toml}`
- `/Volumes/D/claude/pulse/core/src/bridge/{roster,deliver_claude,deliver_codex,mod,hub}.rs`
- `/Volumes/D/claude/pulse/docs/{bridge.md,coderight-handoff.md,bridge-astra-review.md}`
- `/Volumes/D/claude/pulse/mac/Notch/Sources/Sessions/ClaudeTranscript.swift`

Not read: `store.rs`, `control.rs`, `links.rs`, `envelope.rs` (beyond constants) and the Windows module body. The coderight checkout was not searched because the glob timed out.

---
Source: Sage adjudication, 2026-10-11. Gap ledger row 100. Third wave; not started. Open with the owner: budget ceilings (section 5), poster identity, `from-mode`, CLI-session delivery.

## Addendum: Pulse's answers as protocol authority (2026-10-11)

Recorded from the Pulse Mac chat. Two crates accepted.

1. **`from-mode`.** The rule is the host's: the value must equal the RECEIVING chat's own class (`bypass` if it bypasses permissions, else `prompting`); any other value, or none while the receiver bypasses, is held. Read from Claude Code 2.1.295 and proven with hand-built frames. The registry does not expose the receiver's class, so the crate takes the class as a required parameter with no silent default, sends it, and reports a mismatch as a refused receipt (the host answers with a `peer_message_status` "held" notice). B02 as written ("never claim bypass") cannot stand; the forwarded meaning is carried by the sender name. Test name becomes `b02_from_mode_is_caller_supplied_and_mismatch_is_refused`.
2. **Version.** Proven: 2.1.295 (wrapper pattern, hold policy and notice path, exercised on the Mac and the Dell). 2.1.293 is only where the frame shape was first captured: unproven for the notice path. A real session file carries `"version"` in `~/.claude/sessions/<pid>.json`; the protocol table gates on that field and only Pulse changes it.
3. `pulse/docs/coderight-handoff.md` corrected by Pulse to the wrapper the host accepts.
4. **CLI sessions:** unverified (no terminal session available to inspect). `kind: cli` stays out of the first cut, listed as unproven.
5. **Ordering hazard:** Pulse unhooks `core/src/bridge` from the `localsend` feature before either extraction deletes anything, and reports the commit.
6. B01 and B12 are fixed in the crate; Pulse also fixes B12 in place.
7. **M0:** Pulse owns the characterization vectors; the RightKit lead reviews them. Format: JSON case files (one file per area, each case with id, B-finding ids, input, exact expected frame bytes as base64 plus a readable form, expected receipt or error code, and `pulse: legacy | crate: fixed` markers for recorded defects), plus a thin `Engine` trait with one adapter per engine that loads the same files; vectors are copied into RightKit by content hash, not referenced by path.
