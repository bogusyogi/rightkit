# GenRight — RightKit migration handoff

Source root: /Volumes/D/claude/genright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/genright.json)

## rightkit-asr — N

GenRight cargo production source actively implements rightkit-asr's capability locally, with no shared production route found.

Route transcribe through rightkit_asr::Whisper, preserve GenRight transcript/caption policy, then remove local whisper-cli runner/parser after parity.

- Local: [transcribe](/Volumes/D/claude/genright/crates/genright-studio/src/pipeline_workflows/asr.rs:73) — Production ASR workflow shells out to whisper.cpp & parses its output locally.
- Owner: [Whisper::load](/Volumes/D/claude/rightkit/crates/rightkit-asr/src/whisper/mod.rs:307) — Shared Whisper API selects WhisperCpp for a GGML library/model & decodes through its FFI backend.

## rightkit-audio — N

GenRight cargo production source actively implements rightkit-audio's capability locally, with no shared production route found.

Replace compose's untyped narration/music filter construction with rightkit-audio primitives where supported; retain GenRight video assembly, subtitle & loudness policy.

- Local: [build_args](/Volumes/D/claude/genright/crates/genright-studio/src/pipeline_workflows/compose.rs:211) — Production compose workflow constructs its FFmpeg narration/music amix, volume, trim & loudnorm filter chain.
- Owner: [Filter/dialogue_bus_fragment/Ducking](/Volumes/D/claude/rightkit/crates/rightkit-audio/src/lib.rs:30) — Shared typed FFmpeg filter, bus mixing & ducking APIs cover audio-filter primitives in the local chain.

## rightkit-framed-sidecar — N

GenRight cargo production source actively implements rightkit-framed-sidecar's capability locally, with no shared production route found.

Adopt rightkit-framed-sidecar wire/deadline APIs in engine supervisor & stdio engine while preserving EngineFrame semantics, then remove local framing.

- Local: [EngineFrame](/Volumes/D/claude/genright/crates/genright-engine/src/main_stdio.rs:27) — Production engine/supervisor exchange local NDJSON EngineFrame protocol.
- Owner: [framed JSON lines wire](/Volumes/D/claude/rightkit/crates/rightkit-framed-sidecar/src/lib.rs:8) — Shared bounded NDJSON framing/deadline API covers EngineFrame transport.

## rightkit-models — M

GenRight routes studio model installs through rightkit-models, while its active ASR provision route separately owns a pinned Whisper manifest, download lifecycle & checksum verification.

Express Whisper artifacts as a rightkit-models ModelSpec/Store install route, migrate existing files, then remove asr_provision's independent manifest/download/checksum lifecycle; replace local path patch before release qualification.

- Local: [asr_provision](/Volumes/D/claude/genright/crates/genright-studio/src/pipeline_workflows/asr.rs:110) — Live ASR provision command owns its Whisper model allowlist, pinned sizes/SHA-256 values, destination & post-download integrity handling.
- Shared: [install_cmd -> install::start](/Volumes/D/claude/genright/crates/genright-studio/src/model_management/mod.rs:610) — Live studio.models.install command creates a ModelSpec then starts shared Store::adopt_then_install.
- Owner: [Store::adopt_then_install](/Volumes/D/claude/rightkit/crates/rightkit-models/src/install.rs:329) — Shared model store accepts a ModelSpec & performs verified install with adoption, staging & activation.

Delivery: Current GenRight Cargo.toml uses a local [patch.crates-io] path; app manifest versions differ from catalog; switch to published RightKit version before release qualification.

## rightkit-shell — N

GenRight cargo production source actively implements rightkit-shell's capability locally, with no shared production route found.

Adopt rightkit-shell for hardened Tauri/window/menu/engine plumbing, preserve GenRight commands & Motion protocol, then remove duplicate builder wiring.

- Local: [tauri::Builder](/Volumes/D/claude/genright/src-tauri/src/lib.rs:19) — Production app owns Tauri builder/plugins/window/command wiring.
- Owner: [Shell builder](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/lib.rs:17) — Shared hardened Tauri shell/window/menu/engine wiring can host GenRight commands.

## rightkit-transcript — N

GenRight cargo production source actively implements rightkit-transcript's capability locally, with no shared production route found.

Use rightkit-transcript for SRT/VTT parsing/export & timing, preserve caption policy/FFmpeg integration, then remove local parser/export path.

- Local: [parse_srt](/Volumes/D/claude/genright/crates/genright-studio/src/pipeline_workflows/asr.rs:24) — Production ASR path locally parses SRT & returns caption segments.
- Owner: [parse_srt/to_srt](/Volumes/D/claude/rightkit/crates/rightkit-transcript/src/lib.rs:25) — Shared transcript parse/export/word-timing API covers local SRT path.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
