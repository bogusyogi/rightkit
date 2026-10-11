# CutRight — RightKit migration handoff

Source root: /Volumes/D/claude/cutright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/cutright.json)

## rightkit-compose — N

Live Tauri playback calls CutRight compose_frame to select timed video/title layers & evaluate opacity/transforms locally. RightKit compose exposes pure time→FrameDescription evaluation for overlapping base video/still/caption/transform semantics; app decoder, GPU effects, timeline authoring & revision identity remain product-owned.

Adapt compatible timeline video/still/title timing & transform data into RightKit Composition, consume FrameDescription in native compositor adapter, then retire overlapping local base-layer evaluation. Preserve CutRight-only effects, decoder/GPU work, project revision identity & explicitly unsupported semantics; verify preview/export parity.

- Local: [cutright_compose::compose_frame](/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/playback.rs:667) — Active playback evaluates frame from local snapshot; snapshot is built at line970.
- Local: [compose_frame](/Volumes/D/claude/cutright/crates/cutright-compose/src/mapper.rs:240) — Locates timed clip, builds layers, evaluates title fades & clip opacity/transform.
- Owner: [evaluate](/Volumes/D/claude/rightkit/crates/rightkit-compose/src/eval.rs:133) — Pure Composition/time evaluation produces FrameDescription; native rendering/codec I/O are outside owner.

## rightkit-logs — N

CutRight actively emits native engine diagnostics through stderr sinks; it does not invoke RightKit structured JSONL logging.

Add rightkit-logs JsonlStore/RedactionPolicy sink for stderr & protocol events, then route diagnostics collection through owner APIs and prove privacy fields before removing eprintln sinks.

- Local: [with_stderr_sink](/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/engine_supervisor.rs:302) — EngineSupervisor sends stderr lines to eprintln with cutright prefixes.
- Local: [with_protocol_error_sink](/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/engine_supervisor.rs:304) — Malformed frames are reported through a second eprintln sink.
- Owner: [JsonlStore](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/lib.rs:21) — Owner exposes JsonlStore, EventRecord, RedactionPolicy, AsyncWriter & diagnostics collection APIs.

## rightkit-shell — N

CutRight’s active AppShell bridge directly calls Tauri minimize, toggleMaximize and close. Those window actions overlap RightKit shell’s native/renderer command contract, but no rightkit-shell route is registered.

Add shared rightkit-shell host handler for this three-action window subset, then remove direct duplicate command plumbing after Tauri desktop QA.

- Local: [shellBridge.window](/Volumes/D/claude/cutright/apps/cutright/src/lib/shell.ts:14) — Production AppShell bridge directly invokes getCurrentWindow().minimize, toggleMaximize and close.
- Owner: [rk_shell_window](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/commands.rs:107) — Owner implements same minimize/toggle-maximize/close actions as registered Tauri commands.

## @rightkit/shell — N

CutRight’s active AppShell bridge directly calls Tauri minimize, toggleMaximize and close. Those window actions overlap RightKit shell’s native/renderer command contract, but no rightkit-shell route is registered.

Construct @rightkit/shell createShell from shared host bridge, route AppShell caption actions through it, then verify macOS and Windows caption controls.

- Local: [shellBridge.window](/Volumes/D/claude/cutright/apps/cutright/src/lib/shell.ts:14) — Production AppShell bridge directly invokes getCurrentWindow().minimize, toggleMaximize and close.
- Owner: [createShell](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:228) — Owner renderer bridge routes minimize/toggleMaximize/close through rightkit-shell commands.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
