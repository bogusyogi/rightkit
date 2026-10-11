# HeardRight — RightKit migration handoff

Source root: /Volumes/D/claude/heardright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/heardright.json)

## rightkit-align — N

HeardRight command alignment remains local on CoreMlParakeet; no rightkit_align production import/call.

Migrate terminal-command alignment caller to rightkit_align::align after mapping timed words; preserve HeardRight control suffix policy.

- Local: [CoreMlParakeet::align_control_suffix](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/coreml_asr/command_alignment.rs:100) — production TDT command alignment entry point
- Owner: [align](/Volumes/D/claude/rightkit/crates/rightkit-align/src/lib.rs:267) — RightKit product-neutral word alignment API

Delivery: No shared production route found; local implementation remains active.

## rightkit-asr — M

rightkit-asr is live through heardright-parakeet-core re-exports for desktop beam/scoring/detokenization, while HeardRight retains its own CoreMlParakeet decode/display engine.

Move CoreMlParakeet decode/display/command-alignment callers onto rightkit_asr::CoreMlParakeet APIs; retain HeardRight command policy & UI display policy at app boundary.

- Local: [CoreMlParakeet::transcribe_pieces_timed_with_policy](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/coreml_asr/engine.rs:546) — local CoreML engine decodes then assembles display pieces
- Local: [CoreMlParakeet::align_control_suffix](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/coreml_asr/command_alignment.rs:100) — local command-alignment engine remains active
- Shared: [pub use rightkit_asr::parakeet_core::{backend, beam, scoring}](/Volumes/D/claude/heardright/tauri-app-next/heardright-parakeet-core/src/lib.rs:22) — production inference crate re-exports RightKit beam/backend/scoring used by heardright-inference
- Shared: [heardright_parakeet_core::DetokPolicy::canonical_rust](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/asr/runtime.rs:50) — desktop runtime calls re-exported RightKit policy in ASR path
- Owner: [RecurrentTdtBackend / beam exports](/Volumes/D/claude/rightkit/crates/rightkit-asr/src/parakeet_core/mod.rs:22) — RightKit owner API used by app re-export
- Owner: [CoreMlParakeet export](/Volumes/D/claude/rightkit/crates/rightkit-asr/src/lib.rs:115) — RightKit also owns production CoreML ASR API for future full cutover

Delivery: App currently uses published package through parakeet_core only; full engine cutover remains pending.

## rightkit-control — N

Desktop AX/UIA focus, identity, & text-input probing is implemented locally without rightkit_control.

Replace local macos_input/windows_uia target probes with rightkit_control adapters; preserve paste safety gates & target policy.

- Local: [crate::macos_input::focused_text_input](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/delivery/backend.rs:358) — live delivery gate reads local macOS AX editable target
- Local: [windows_focused_text_input](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/delivery/targets.rs:802) — live Windows UIA target probe
- Owner: [Control](/Volumes/D/claude/rightkit/crates/rightkit-control/src/embedded/mod.rs:203) — RightKit native input/accessibility owner

Delivery: No shared production route found; local implementation remains active.

## rightkit-db — N

Desktop history uses local FileHistoryStore/SQLCipherHistoryStore rather than rightkit_db connection/bootstrap APIs.

Replace local SQLCipher bootstrap/migration/recovery with rightkit_db; preserve HeardRight history schema, encryption key source, & recovery policy.

- Local: [SQLCipherHistoryStore::open_default](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/runtime/engine.rs:264) — production encrypted history store open path
- Local: [impl HistoryStore for SQLCipherHistoryStore](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/history/sqlcipher_store.rs:122) — local SQLCipher connection/query/migration implementation
- Owner: [open_existing_writer](/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:393) — RightKit SQLite/SQLCipher connection owner
- Owner: [migrate](/Volumes/D/claude/rightkit/crates/rightkit-db/src/migrate.rs:407) — RightKit migration owner

Delivery: No active caller or compatible runtime in source census.

## rightkit-fs — N

Desktop settings/history/model paths implement local atomic files, locks, & directories.

Route settings/history state files through rightkit_fs atomic/lock primitives; preserve app paths, formats, & retention policy.

- Local: [save_blob_atomic_to_path_with_replace](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/settings.rs:71) — production settings atomic write
- Local: [impl HistoryStore for FileHistoryStore](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/history/file_store.rs:80) — local history file persistence/locking path
- Owner: [atomic_write](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:141) — RightKit atomic file owner
- Owner: [with_lock](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lock.rs:595) — RightKit cross-process lock owner

Delivery: No active caller or compatible runtime in source census.

## rightkit-hotkeys — N

Global shortcut registration & conflict handling remain local in Tauri desktop host.

Route shortcut parsing, registration, conflict checks, & PTT hooks through rightkit_hotkeys; keep product shortcut defaults.

- Local: [hotkey::register_shortcuts](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/hotkey_setup.rs:41) — startup registers active desktop shortcuts
- Local: [register_shortcuts](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/hotkey.rs:96) — local CGEvent/Windows registration implementation
- Owner: [parse_chord](/Volumes/D/claude/rightkit/crates/rightkit-hotkeys/src/lib.rs:23) — RightKit global shortcut owner
- Owner: [should_arm_ptt](/Volumes/D/claude/rightkit/crates/rightkit-hotkeys/src/ptt.rs:282) — RightKit push-to-talk policy API

Delivery: No shared production route found; local implementation remains active.

## rightkit-http — N

Lease and Windows-link calls use local ureq agent/post helper; no rightkit_http client route.

Replace local lease_post/ureq transport with rightkit_http::Client; keep HeardRight endpoint paths, response policy, & error wording.

- Local: [lease_post](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/lease.rs:163) — production device-lease endpoint adapter sends local ureq requests
- Owner: [Client](/Volumes/D/claude/rightkit/crates/rightkit-http/src/client.rs:674) — RightKit blocking HTTP client owner

Delivery: No shared production route found; local implementation remains active.

## rightkit-license — M

rightkit_license::verify_token is live for entitlement tokens, while local verify_lease_envelope_signature duplicates RightKit lease-envelope signature verification; endpoint/device-lease policy stays app-owned.

Delete local verify_lease_envelope_signature and call rightkit_license::verify_lease_envelope; retain HeardRight lease endpoint adapters, device policy, & lifecycle decisions.

- Local: [verify_lease_envelope_signature](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/lease.rs:260) — active local Ed25519 verifier for device-lease envelope
- Shared: [rightkit_license::verify_token](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/lease.rs:457) — production refresh path verifies signed entitlement token
- Shared: [rightkit_license::verify_token](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/keychain.rs:211) — production secure-license restore verifies cached token
- Owner: [verify_token](/Volumes/D/claude/rightkit/crates/rightkit-license/src/lib.rs:396) — shared entitlement-token verifier
- Owner: [verify_lease_envelope](/Volumes/D/claude/rightkit/crates/rightkit-license/src/lease.rs:164) — shared device-lease envelope verifier matching local duplicate

Delivery: Pinned Cargo dependency; source audit does not qualify built/runtime artifact.

## rightkit-models — M

rightkit_models::hash::sha256_file is live, while HeardRight model_fetch owns active manifest, download, checksum, & rename flow overlapping RightKit model installation.

Replace model_fetch ensure/download/manifest flow with rightkit_models Store::ensure_ready/download_resumable; preserve HeardRight model IDs, backend selection, & CoreML validation.

- Local: [ensure_model](/Volumes/D/claude/heardright/tauri-app-next/heardright-model-store/src/model_fetch.rs:68) — engine startup ensures local model directory
- Local: [download_and_verify](/Volumes/D/claude/heardright/tauri-app-next/heardright-model-store/src/model_fetch.rs:150) — active resumeless download, digest check, & atomic rename
- Shared: [pub use rightkit_models::hash::sha256_file](/Volumes/D/claude/heardright/tauri-app-next/heardright-model-store/src/model_fetch.rs:177) — production model store calls shared hash helper
- Owner: [ensure_ready](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/store.rs:166) — RightKit verified model readiness/activation owner
- Owner: [download_resumable](/Volumes/D/claude/rightkit/crates/rightkit-models/src/download.rs:190) — RightKit download lifecycle owner

Delivery: Pinned app dependency; source audit does not qualify built/runtime artifact.

## rightkit-ort — N

Desktop VAD and ASR probes invoke local ort sessions directly; no rightkit_ort runtime route.

Replace local ORT discovery/init/session setup with rightkit_ort; preserve VAD model path & explicit CoreML probe policy.

- Local: [Session::run](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/vad.rs:437) — live Silero VAD inference runs local ONNX Runtime session
- Local: [initialize_ort_for_probe](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/vad.rs:506) — production runtime probe initializes local ORT
- Owner: [configure_runtime](/Volumes/D/claude/rightkit/crates/rightkit-ort/src/lib.rs:484) — RightKit runtime resolution/configuration owner
- Owner: [Session::build_from_memory](/Volumes/D/claude/rightkit/crates/rightkit-ort/src/session.rs:168) — RightKit ORT session owner

Delivery: No shared production route found; local implementation remains active.

## rightkit-overlay — N

Tauri desktop pill uses local hitbox, hover, window placement, & transparent overlay lifecycle.

Replace local pill hitbox/hover/placement primitives with rightkit_overlay; preserve HeardRight pill visuals, auxiliary windows, & platform no-activate rules.

- Local: [set_pill_hitbox](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/pill/state.rs:587) — production overlay geometry state
- Local: [position_webview_pill](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/pill/state.rs:1170) — production cross-platform overlay placement
- Owner: [OverlayModel](/Volumes/D/claude/rightkit/crates/rightkit-overlay/src/lib.rs:349) — RightKit overlay state/hit-testing owner

Delivery: No active caller or compatible runtime in source census.

## rightkit-secrets — N

Desktop license, DB-key, & device stores use local keyring implementations.

Route license/device/DB-key persistence through rightkit_secrets::Store; preserve app service/account names & migration policy.

- Local: [secure_license_get_inner](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/license/keychain.rs:174) — production OS keyring read
- Local: [get_generic_password](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/credential_store.rs:149) — production SQLCipher keychain read
- Owner: [Store](/Volumes/D/claude/rightkit/crates/rightkit-secrets/src/store.rs:123) — RightKit OS credential store owner

Delivery: No shared production route found; local implementation remains active.

## rightkit-settings — N

Tauri settings snapshot/patch path persists local SettingsBlob with custom atomic replacement.

Map SettingsBlob schema & migrations to rightkit_settings::Store; preserve HeardRight defaults, keys, & renderer contract.

- Local: [apply_settings_patch](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/commands/settings.rs:27) — renderer production settings mutation entry point
- Local: [save_to_disk](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/settings.rs:66) — local SettingsBlob persistence
- Owner: [Store](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/store.rs:133) — RightKit versioned settings store owner

Delivery: No shared production route found; local implementation remains active.

## rightkit-shell — N

Desktop startup, tray/menu, single-instance, & window creation remain local Tauri shell code; no rightkit_shell production route.

Replace local Tauri startup/tray/single-instance/window shell with rightkit_shell::ShellBuilder/modules; preserve HeardRight tray actions, menu policy, window labels, & platform lifecycle.

- Local: [tauri_plugin_single_instance::init](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/app_run.rs:841) — production desktop builder installs local single-instance handler
- Local: [build_tray_menu](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/tray.rs:98) — production desktop builds tray menu and lifecycle locally
- Owner: [ShellBuilder::builder](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/lib.rs:195) — RightKit shell builder API matching desktop startup integration
- Owner: [arbitrate](/Volumes/D/claude/rightkit/crates/rightkit-shell/src/instance.rs:69) — RightKit single-instance arbitration API matching local handler

Delivery: Pinned app dependency; source audit does not qualify built/runtime artifact.

## rightkit-sync — N

Desktop CloudKit sync uses local cloud_sync helper protocol, tombstone ledger, & pull/push policy.

Replace local cloud sync ledger/pull/push engine with rightkit_sync adapter; preserve CloudKit helper transport, HeardRight scope, & transcript mapping.

- Local: [cloud_sync_pull](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/cloud_sync.rs:870) — production CloudKit pull command
- Local: [cloud_sync_push](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/cloud_sync.rs:1205) — production CloudKit push command
- Owner: [SyncEngine](/Volumes/D/claude/rightkit/crates/rightkit-sync/src/engine.rs:164) — RightKit change-token/conflict engine owner
- Owner: [ConflictPolicy](/Volumes/D/claude/rightkit/crates/rightkit-sync/src/traits.rs:62) — RightKit conflict policy owner

Delivery: No active caller or compatible runtime in source census.

## rightkit-transcript — N

Desktop history export implements local transcript format selection & formatting.

Replace local history_export formatters with rightkit_transcript export/parse APIs; preserve HeardRight history metadata & UI format choices.

- Local: [history_export::format_history](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/dictation_commands.rs:598) — production export path formats selected transcript history
- Local: [format_history](/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/history_export.rs:24) — local SRT/VTT/JSON/plain-text formatting owner
- Owner: [to_srt](/Volumes/D/claude/rightkit/crates/rightkit-transcript/src/export.rs:42) — RightKit transcript export API
- Owner: [parse_srt](/Volumes/D/claude/rightkit/crates/rightkit-transcript/src/parse.rs:105) — RightKit transcript parse API

Delivery: No shared production route found; local implementation remains active.

## rightkit-vad — N

Desktop SpeechVad owns live Silero scoring, frame carry, hysteresis, & ORT execution.

Map SpeechVad thresholds/frame carry/backend adapter to rightkit_vad::detect; preserve HeardRight calibration & telemetry.

- Local: [VadBackend::Ort](/Volumes/D/claude/heardright/tauri-app-next/heardright-inference/src/vad.rs:435) — live frame scoring invokes local ORT backend
- Owner: [detect](/Volumes/D/claude/rightkit/crates/rightkit-vad/src/spans.rs:61) — RightKit VAD detection owner

Delivery: No shared production route found; local implementation remains active.

## @rightkit/platform-ui — M

AppShell reaches RightKit platform UI for caption controls, while renderer keeps local platform detection & chord formatting in active components.

Remove local getPlatform/formatChord implementations & route shortcut display, platform capabilities, & window control adapters through @rightkit/platform-ui; preserve HeardRight chord policy & visual tokens.

- Local: [getPlatform](/Volumes/D/claude/heardright/tauri-app-next/src/hooks/usePlatform.ts:20) — production renderer independently detects macOS/iOS/Windows/Linux
- Local: [formatChord](/Volumes/D/claude/heardright/tauri-app-next/src/components/Kbd.tsx:53) — production renderer independently formats platform-specific shortcut labels
- Shared: [AppShell](/Volumes/D/claude/heardright/tauri-app-next/src/App.tsx:347) — production renderer mounts AppShell; bridge path reaches app-shell CaptionControls and RightKit platform-ui controls
- Owner: [getPlatform](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:90) — RightKit platform detection API matching local getPlatform
- Owner: [formatChord](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:100) — RightKit shortcut display API matching local formatChord
- Owner: [createWindowControls](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:144) — RightKit window-control adapter used by app-shell CaptionControls

Delivery: Pinned app dependency; source audit does not qualify built/runtime artifact.

## @rightkit/qa — N

HeardRight QA scripts run local CDP journeys, focus assertions, recovery checks, & artifact cleanup; no @rightkit/qa production script route.

Replace local CDP driver, journey plumbing, ownership/cleanup, & native QA guards with @rightkit/qa APIs; preserve HeardRight journey selectors, product fixtures, & expected error policy.

- Local: [openBrowser](/Volumes/D/claude/heardright/tauri-app-next/qa/hub/driver.mjs:25) — production QA driver owns browser launch, CDP transport, health checks, & cleanup
- Local: [journeys](/Volumes/D/claude/heardright/tauri-app-next/qa/hub/journeys.mjs:22) — production QA journey registry owns navigation, modal focus, error recovery, & screenshot assertions
- Owner: [CdpClient](/Volumes/D/claude/rightkit/packages/qa/src/cdp.ts:22) — RightKit shared CDP transport matching local driver
- Owner: [validateNativeQaContract](/Volumes/D/claude/rightkit/packages/qa/src/guard.ts:8) — RightKit QA contract guard matching app QA gate surface

Delivery: Pinned app dependency; source audit does not qualify built/runtime artifact.

## @rightkit/shell — N

Renderer windows use local tauriBridge invoke/listen transport; no @rightkit/shell production route.

Replace local tauriBridge transport/window commands with @rightkit/shell createShell; preserve HeardRight command/event names & QA transport.

- Local: [invoke](/Volumes/D/claude/heardright/tauri-app-next/src/lib/tauriBridge.ts:59) — production renderer IPC bridge
- Local: [listen](/Volumes/D/claude/heardright/tauri-app-next/src/lib/tauriBridge.ts:72) — production renderer event bridge
- Owner: [createShell](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:228) — RightKit typed renderer shell owner

Delivery: No active caller or compatible runtime in source census.

## RightKitArtifacts — N

Native model download/install lifecycle calls local ModelArtifactInstaller for archive validation, activation, rollback, & cleanup.

Replace ModelArtifactInstaller calls with RightKitArtifacts.ArtifactInstaller; preserve CoreML stage validation, model policy, & app-group publication.

- Local: [ModelArtifactInstaller.finalizePreviousInstall](/Volumes/D/claude/heardright/ios-native/HeardRight/ModelInstallation.swift:22) — production validated model activation finalization
- Local: [ModelArtifactInstaller](/Volumes/D/claude/heardright/ios-native/HeardRight/ModelArtifactInstaller.swift:349) — local archive/install/recovery implementation
- Owner: [ArtifactInstaller](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitArtifacts/ArtifactInstaller.swift:122) — shared verify/stage/activate/rollback owner

Delivery: No active caller or compatible runtime in source census.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
