# CutRight RightKit cutover matrix

Reviewed 2026-10-11T08:01:04+05:30 at bf712c7b55db221995e8b2958000a9f933cdeb54. Targets: macOS & Windows Rust/Tauri desktop plus macOS headless Swift sidecars/runtime verifier.

Counts: **31 Y / 4 N / 0 M / 62 NA** across 97 catalog rows.

Shared production routes include RightKit media execution, audio, ASR/VAD, model store, ONNX runtime, transcript, speech service, jobs/process supervision, playback, cache, filesystem, settings, service discovery, LLM/HTTP/secrets, control, QA, React shell/media/onboarding/settings UI, legal gate/manifest, platform primitives, theme generation & release tooling. `rightkit-review` is Y because CutRight invokes `RuleSet::new` & `RuleSet::evaluate` for resolution; local decode, duration, frame, black/freeze, silence & caption checks remain distinct semantics. Cargo rows still carry local rk-snapshot patch entries; replace those with published pinned versions before release.

Remaining N rows:

- `cargo:rightkit-compose`: live playback calls local `compose_frame` for timed layers, fades & transforms. Adapt matching base evaluation through shared `evaluate`; retain decoder/GPU/effects policy & prove preview/export parity.
- `cargo:rightkit-logs`: native supervisor writes stderr diagnostics. Add `JsonlStore`/`RedactionPolicy` sink, route protocol events, prove privacy fields, then remove eprintln sinks.
- `cargo:rightkit-shell`: AppShell directly calls Tauri minimize, toggleMaximize & close. Register shared host commands, then remove duplicate command plumbing after desktop QA.
- `npm:@rightkit/shell`: construct `createShell` from shared host bridge, route caption actions through it, then verify macOS & Windows caption controls.

NA classifications are semantic/runtime exclusions: timestamped contiguous-word phrase matching is not shared query/FTS/character-snippet behavior; project/timeline JSON is not one of six shared contracts; webview keydown/static shortcut help is not native global hotkeys; React context-menu dismissal is not native overlay placement/hit testing; renderer logs client has no caller. macOS Swift helpers are JSONL media-worker services plus Speech timed-word verifier, not mobile/watch/SwiftUI routes; verifier emits timed segments while `RightKitSpeech` returns transcription text. Product contracts by themselves are not duplicate shared contracts.

Every catalog ID appears once in JSON. Evidence is source-level; no builds or runtime qualification were run.

## Composition evidence

`rightkit-compose` is N: [playback.rs:667](/Volumes/D/claude/cutright/apps/cutright/src-tauri/src/playback.rs:667) actively calls local [compose_frame](/Volumes/D/claude/cutright/crates/cutright-compose/src/mapper.rs:240). This evaluates timed layers, fades & transforms, overlapping shared [evaluate](/Volumes/D/claude/rightkit/crates/rightkit-compose/src/eval.rs:133). Migrate that base evaluation subset through a timeline adapter; retain decoder/GPU/effect policy & prove preview/export parity.
