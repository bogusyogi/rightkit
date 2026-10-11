# VoiceRight — RightKit migration handoff

Source root: /Volumes/D/claude/voiceright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/voiceright.json)

## rightkit-audio — N

Local WAV validation/write overlaps shared WAV I/O; no RightKit route.

Route WAV parsing/writing through rightkit-audio or rightkit-tts; preserve sample-rate/export checks.

- Local: [read_wav_info](/Volumes/D/claude/voiceright/src-tauri/src/engine/wav.rs:8) — Local RIFF/WAVE parser validates sample rate, duration, and bytes.
- Local: [write_wav](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:91) — Local QTS adapter converts PCM and writes generated WAV.
- Owner: [write_wav_mono_i16](/Volumes/D/claude/rightkit/crates/rightkit-audio/src/wav.rs:66) — Owner writes 16-bit PCM WAV from mono f32 samples, matching VoiceRight's generated-WAV boundary.

## rightkit-fs — N

Local app-data/staged/generated/voice writes overlap shared state/file safety; no RightKit route.

Adopt rightkit-fs for app-data/staged/generated/voice writes; remove direct std::fs after parity.

- Local: [set_artifact_root](/Volumes/D/claude/voiceright/src-tauri/src/engine/mod.rs:150) — Local app-data engine root uses std::fs::create_dir_all and mutex.
- Local: [stage_audio_file](/Volumes/D/claude/voiceright/src-tauri/src/commands.rs:70) — Local staged audio copy uses std::fs directly.
- Owner: [StateDir::child](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/state.rs:105) — Owner pins an owner-only state directory then creates validated child state directories.

## rightkit-models — N

Local QTS model/ORT bundle provisioning overlaps shared model installation; no RightKit route.

Adopt rightkit-models manifest/verification/activation for QTS model and ORT provisioning.

- Local: [provision_from_bundle](/Volumes/D/claude/voiceright/src-tauri/src/engine/mod.rs:170) — Local model/ORT checks and bundle copy provision artifacts.
- Local: [models_exist](/Volumes/D/claude/voiceright/src-tauri/src/engine/artifacts.rs:10) — Local manifest is two hard-coded model files with is_file checks.
- Owner: [Store::adopt_then_install](/Volumes/D/claude/rightkit/crates/rightkit-models/src/install.rs:329) — Owner adopts existing pinned model files, verifies them, then activates one revision atomically.

## rightkit-ort — N

Direct ort and local ORT runtime setup overlap rightkit-ort's runtime binding API; no RightKit route.

Route ORT setup through rightkit-ort; remove direct ort and local env/path setup.

- Local: [ort dependency](/Volumes/D/claude/voiceright/src-tauri/Cargo.toml:15) — Direct optional ort dependency instead of rightkit-ort.
- Local: [configure_runtime](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:216) — Local adapter sets ORT_DYLIB_PATH and prepends runtime directory.
- Owner: [configure_runtime](/Volumes/D/claude/rightkit/crates/rightkit-ort/src/lib.rs:484) — Owner resolves approved runtime candidates before setting ORT_DYLIB_PATH.

Delivery: Manifest requests ort 2.0.0-rc.9; Cargo.lock resolves 2.0.0-rc.12; no rightkit-ort dependency.

## rightkit-qts — N

Embedded engine calls local-path qts despite a matching rightkit-qts API; no RightKit route.

Replace eval/qts-upstream with published rightkit-qts; preserve embedded and voice-prompt contracts.

- Local: [qts dependency](/Volumes/D/claude/voiceright/src-tauri/Cargo.toml:16) — qts resolves from ../eval/qts-upstream path, not published RightKit.
- Local: [Qwen3TtsEngine::from_model_dir](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:113) — Embedded production feature loads local QTS engine.
- Owner: [Qwen3TtsEngine](/Volumes/D/claude/rightkit/crates/rightkit-qts/src/lib.rs:139) — Owner provides direct embedded Qwen3 engine loading plus synthesis and clone-prompt APIs.

Delivery: Configured local path dependency ../eval/qts-upstream is absent from this checkout; classification follows the active manifest route and embedded caller, not a build claim.

## rightkit-qts-ggml — N

Active local qts dependency brings qts_ggml transitively; no RightKit GGML route is active.

After qts adoption, remove local qts_ggml path and verify Vulkan/CPU behavior.

- Local: [qts_ggml package](/Volumes/D/claude/voiceright/src-tauri/Cargo.lock:2843) — Local qts path resolves active qts_ggml dependency.
- Local: [Qwen3TtsEngine::from_model_dir](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:113) — App call chain reaches local GGML implementation transitively.
- Owner: [Context](/Volumes/D/claude/rightkit/crates/rightkit-qts-ggml/src/lib.rs:12) — Owner's deprecated facade supplies the same GGML context wrapper present in VoiceRight's active local QTS lock chain.

Delivery: Transitive qts_ggml comes from the configured local qts path; no RightKit route is active.

## rightkit-qts-ggml-sys — N

Active local qts dependency brings qts_ggml_sys transitively; no RightKit GGML FFI route is active.

After qts adoption, remove local qts_ggml_sys path and verify native linkage.

- Local: [qts_ggml_sys package](/Volumes/D/claude/voiceright/src-tauri/Cargo.lock:2850) — Local qts path resolves active qts_ggml_sys dependency.
- Local: [Qwen3TtsEngine::from_model_dir](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:113) — App call chain reaches local GGML FFI transitively.
- Owner: [generated ggml bindings](/Volumes/D/claude/rightkit/crates/rightkit-qts-ggml-sys/src/lib.rs:9) — Owner exposes GGML FFI bindings used by the QTS stack; VoiceRight reaches its local equivalent through qts_ggml.

Delivery: Transitive qts_ggml_sys comes from the configured local qts path; no RightKit route is active.

## rightkit-shell — N

Local Tauri builder/config own shared shell concerns; no rightkit-shell route.

Adopt rightkit-shell for Tauri shell wiring; keep product commands local.

- Local: [tauri::Builder](/Volumes/D/claude/voiceright/src-tauri/src/lib.rs:7) — Local Tauri builder owns plugins, setup, handler, and run.
- Local: [app.windows](/Volumes/D/claude/voiceright/src-tauri/tauri.conf.json:12) — Local config owns window/security shell behavior.
- Owner: [ShellPluginBuilder::build](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/modules.rs:183) — Owner installs a configured Tauri shell plugin; VoiceRight instead registers its own Tauri shell and frameless window configuration.

## rightkit-tts — N

Local Rust QTS adapter owns shared TTS concerns; no rightkit-tts route.

Adopt rightkit-tts or document retained product-specific adapter; remove duplicate path when clone is covered.

- Local: [synth_text](/Volumes/D/claude/voiceright/src-tauri/src/engine/mod.rs:63) — Local engine owns TTS command and voice-prompt routing.
- Local: [Qwen3TtsEngine::synthesize](/Volumes/D/claude/voiceright/src-tauri/src/engine/qts_embedded.rs:81) — Local QTS synthesis and voice-clone adapter are active.
- Owner: [QtsClient::synthesize](/Volumes/D/claude/rightkit/crates/rightkit-tts/src/client.rs:79) — Owner QTS client exposes synthesis and clone-to-library calls through its managed worker boundary.

## @rightkit/app-shell — N

Local React frame implements app-shell layout; no shared app-shell import.

Adopt app-shell primitives; preserve VoiceRight compose/voice-library/result policy.

- Local: [App](/Volumes/D/claude/voiceright/src/App.tsx:199) — Local React frame owns titlebar, rail, workspace, compose, and results.
- Owner: [AppShell](/Volumes/D/claude/rightkit/packages/app-shell/src/react/layout.tsx:518) — Owner provides titlebar/sidebar/page layout through app-owned slots, matching VoiceRight's local frame boundary.

## @rightkit/media-ui — N

Local HTML audio preview implements media playback; no media-ui import.

Adopt media-ui player for WAV preview; preserve output/export controls.

- Local: [audio](/Volumes/D/claude/voiceright/src/App.tsx:301) — Local HTML audio previews generated WAV output.
- Owner: [MediaPlayer](/Volumes/D/claude/rightkit/packages/media-ui/src/MediaPlayer.tsx:209) — Owner renders WebView media playback and accepts an app media source, matching VoiceRight's generated-WAV preview.

## @rightkit/platform-ui — N

Local platform detection and minimize/maximize/close controls overlap platform-ui's explicit adapters; no shared route.

Adopt platform-ui detection/window controls; preserve titlebar styling.

- Local: [windowAction](/Volumes/D/claude/voiceright/src/components/WindowControls.tsx:1) — Local controls call Tauri window APIs.
- Local: [getPlatform](/Volumes/D/claude/voiceright/src/hooks/usePlatform.ts:14) — Local platform detection drives titlebar behavior.
- Owner: [createWindowControls](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:144) — Owner maps minimize, toggleMaximize, and close adapters; it also supplies loadPlatform at line 70.

## @rightkit/shell — N

Local WindowControls directly performs minimize, toggleMaximize, and close, matching createShell().window; product engine invokes remain app-owned.

Adopt @rightkit/shell typed bridge; preserve VoiceRight command payloads.

- Local: [windowAction](/Volumes/D/claude/voiceright/src/components/WindowControls.tsx:1) — Live titlebar controls call Tauri minimize, toggleMaximize, and close directly.
- Local: [WindowControls caller](/Volumes/D/claude/voiceright/src/components/TitleBar.tsx:12) — Non-macOS production titlebar renders those window controls.
- Owner: [createShell().window](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:239) — Owner calls rk_shell_window for minimize, toggleMaximize, and close.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
