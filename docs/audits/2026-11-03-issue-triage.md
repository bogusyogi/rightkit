# Issue triage, 2026-11-03

On Adrian's instruction (2026-11-03): "Close all issues that are superseded or stale. All open issues, work through them and close them".
Five read-only agents triaged the 885 open issues of bogusyogi/rightkit against `origin/main` and the registries; the lead spot-checked DONE verdicts before closing.
Closed through the REST API without a comment (GitHub's secondary rate limit refused comments); this file is the reason of record for each closure.

Closed: 424 (DONE 354, STALE 43, SUPERSEDED 27). Still open: issues judged OPEN or PUBLISH.

| Issue | Verdict | Evidence |
|---|---|---|
| rightkit#79 | DONE | embed 0.6.x on registry has batched DirectML ingest, load pre-warm and the opt-in parity verdict cache (embed CHANGELOG) |
| rightkit#82 | SUPERSEDED | disk hygiene is now the hourly `right-release disk run --auto` (release CHANGELOG 304-310, 3aebe9ef5); docs/rules/disk-hygiene.md no longer exists; see #346 |
| rightkit#117 | DONE | docs/design/android-minimum.md holds the design and ordered crate list; rightkit-android 0.3.0 is on crates.io. |
| rightkit#118 | DONE | rightkit-sync `drive` module (Google Drive appDataFolder, rightkit#161), published in 0.2.2 |
| rightkit#133 | DONE | rightkit-daemon `relay` feature and module (rk-relay/1); daemon 0.5.1 is on crates.io |
| rightkit#138 | DONE | swift-check lane is in packages/ci (swift-check.test.mjs); the exact-commit hosted-CI proof is moot now that CI is not the route |
| rightkit#140 | DONE | `right-release apple-profile-create` issues the MAC_APP_DEVELOPMENT profile (2d498baa6); @rightkit/release 0.6.36 published |
| rightkit#152 | DONE | Android compile pass recorded in the rightkit-app-settings and rightkit-http CHANGELOGs (docs-only, nothing needed gating); consumer run belongs to HeardRight. |
| rightkit#155 | DONE | rightkit-sync `schedule` / `sync_once` (commit 3064aa2a8), published in 0.2.2 |
| rightkit#161 | DONE | rightkit-sync `drive` feature and `DriveBackend`; sync 0.2.2 published |
| rightkit#162 | DONE | rightkit-google-identity 0.2.0 is on crates.io; rightkit-oauth google feature published |
| rightkit#163 | DONE | rightkit-vault split-key vault (67d2ea1a4); 0.2.0 on crates.io |
| rightkit#166 | DONE | rightkit-search 0.4.x (on crates.io) has Binding::SqlMulti, ChildScope::negated_text_like and op_sources. |
| rightkit#170 | DONE | rightkit-pdf `scan` module with `raster` feature (0a00eaa41), published 0.3.4 |
| rightkit#172 | DONE | rightkit-search `Binding::SqlByShape` and `negated_text_unescaped`; search 0.4.x published |
| rightkit#176 | DONE | rightkit-record 0.3.3 published; CHANGELOG records the seal reason and host clock (rightkit#176) |
| rightkit#177 | DONE | rightkit-automation `probe_capture_safety` (4b52e7700); 0.6.4 published |
| rightkit#178 | DONE | rightkit-onnx 0.2.9 (on crates.io): verify_library_in_manifest in runtime_bundle.rs and configure_runtime_verified. |
| rightkit#179 | DONE | rightkit-review 0.4.0 verdicts bound to policy hash and check versions (787c40e9e), published 0.4.1 |
| rightkit#180 | DONE | rightkit-hotkeys bounded PTT timers and F21–F24 chords; 0.1.2 published |
| rightkit#181 | DONE | rightkit-hotkeys 0.1.2 published: middle-mouse AX lookup moved off the tap callback, plus tap-disable recovery |
| rightkit#187 | DONE | rightkit-http `ProxyMode::SystemForUrl` (07c9045ff); 0.3.9 published |
| rightkit#188 | DONE | rightkit-automation 0.4.3: El::set_point/set_size/set_frame and marker::set_event_marker; 0.6.4 is published. |
| rightkit#192 | SUPERSEDED | rightkit-chat-bridge 0.1.0 is published; rightkit-agents covers sessions; Pulse vectors tracked in #907 |
| rightkit#193 | STALE | one-off Pulse ledger run; file-watch rows shipped (file-watch 0.4.x), replaced by the Pulse ownership-exception audit |
| rightkit#194 | DONE | rightkit-oauth 0.7.0 shipped durable server stores, bare refresh with rotation and Mastodon/X presets; 0.10.0 is current |
| rightkit#196 | DONE | rightkit-asr `ReportedLanguage` / CoreMlParakeet language (cf9314304); 0.4.0 published |
| rightkit#201 | DONE | rightkit-models tests/lease_unbounded.rs drives the give_up path of the unbounded acquire. |
| rightkit#215 | STALE | refers to @rightkit/qa 0.4.7 (npm is 0.6.2) and a Windows CI leg; D6 says no CI; PLATFORM-MATRIX row is old. |
| rightkit#220 | STALE | one-off cold-start timing report for the old packages/qa browser timeouts |
| rightkit#221 | DONE | rightkit-browser CHANGELOG records the 34 s vs 152 s sandbox-access measurements (commit 6f286164d) |
| rightkit#222 | DONE | policy_e2e fails closed without Chrome for Testing, component-update arg (7d6f0a6e3); rightkit-browser 0.3.7 published |
| rightkit#224 | DONE | rightkit-models E43 download route and whole-archive SHA-256 pin (CHANGELOG cites #224); 0.5.3 is published. |
| rightkit#226 | DONE | rightkit-llm agent_providers `http.rs` / `call_options` cover raw body, UA, loopback, timeouts; llm 0.3.2 published |
| rightkit#228 | DONE | rightkit-fsindex 0.6.0 published; Windows Search backend shipped in 0.4.0 (commit 906ce756d) |
| rightkit#234 | STALE | one-off Dell clean-up pass of CutRight QA homes (run-only) |
| rightkit#236 | DONE | rightkit-review `ladder` module ported from CutRight (0.3.0); 0.4.1 is published. |
| rightkit#238 | DONE | rightkit-mcp 0.7.0 concurrent stdio with progress/cancel, `InitializeOverrides`, response hook (9b10bae36), published 0.7.2 |
| rightkit#241 | DONE | rightkit-cache `move_to` and `set_budget` (0.2.3); `@rightkit/i18n` 0.2.0 sync `t()` |
| rightkit#243 | DONE | rightkit-mcp client delivers server notifications (list_changed, progress, message); 0.7.2 published |
| rightkit#244 | DONE | daemon `verify_peer_executable`, hotkeys `KeyEventDetail`, file-watch Windows backend, automation AX query (all published); RightKitSwift 0.9.0 tagged |
| rightkit#248 | DONE | rightkit-audio 0.3.0 `playback::OutputSession` (1a85a6079), published 0.3.1 |
| rightkit#256 | DONE | @rightkit/release fixed-path in-place install with a single `.previous` (release CHANGELOG, shipped by 0.6.36) |
| rightkit#264 | DONE | rightkit-motion 0.2.1 and rightkit-typeset 0.1.0 published ("Initial version", rightkit#264) |
| rightkit#268 | DONE | rightkit-license is on ed25519-dalek 3; windows 0.62 is the suite-wide pin in every crate, and the Tauri-side 0.61 pairing is an app concern. |
| rightkit#273 | DONE | @rightkit/pdf-ui 0.2.0 `findLiteralMatches` fold and whitespace fix |
| rightkit#274 | DONE | rightkit-pdf `redact` feature (commit 4327ecd62); rightkit-pdf 0.3.4 published |
| rightkit#275 | DONE | rightkit-pdf organize ops (8e3623e08); 0.3.4 published |
| rightkit#276 | DONE | rightkit-pdf `forms` feature (list_fields, set_values, reset_fields) in 0.3.1; 0.3.4 is published. |
| rightkit#277 | DONE | rightkit-pdf `optimize` feature (a07d6f64a), published in 0.3.x |
| rightkit#278 | DONE | rightkit-pdf `tests/extract_quality.rs` (word F1 and reading-order vs poppler); pdf 0.3.4 published |
| rightkit#279 | DONE | rightkit-oauth opens the default browser (commit a6e3af87b); ownership catalog has the oauth-sign-in rules |
| rightkit#282 | DONE | rightkit-pdf `write::Document` with JPEG, ICC and text (02d519a45, cdb6e8a73); 0.3.4 published |
| rightkit#283 | DONE | rightkit-mcp 0.7.0 to 0.7.2 published: request transform, strict validation, notify_progress_number; 0.7.2 is on crates.io. |
| rightkit#286 | DONE | rightkit-browser consented real-session mode (ProfileSpec::ImportedFrom, Chrome/Edge attach), published 0.3.7 |
| rightkit#287 | DONE | rightkit-browser `ProfileSpec::ImportedFrom` and consent mode; browser 0.3.7 published |
| rightkit#288 | DONE | rightkit-embed 0.6.0 added batch-4/8 DirectML ingest graphs and the int8 token table (commit e2a0e5055); 0.6.2 published |
| rightkit#289 | DONE | rightkit-models Io error code and `download_resumable_capped` (c33984e6e); 0.5.3 published |
| rightkit#290 | DONE | rightkit-automation 0.4.4: send_media_key(MediaKey) in media_keys.rs; 0.6.4 is published. |
| rightkit#293 | DONE | build-control-policy 0.1.2 guard fires only for local Mac/Dell execution (c4c377564); package is private so no publish is owed |
| rightkit#296 | DONE | rightkit-workflow 0.2.0 `sequence` feature (resumable linear run), commit 1f989a681; 0.2.1 published |
| rightkit#297 | DONE | rightkit-llm tool executor (`ToolRegistry`, `ToolGate`) in 0.3.2, published |
| rightkit#298 | DONE | rightkit-llm `ContentBlock::Image`/Frame (af68e351b); 0.3.2 published |
| rightkit#299 | DONE | rightkit-platform-rules 0.1.0 is on crates.io. |
| rightkit#300 | DONE | rightkit-ffmpeg 0.2.9 quickprobe reports size_bytes, codec and frame_count (0321f2fe3), published 0.2.13 |
| rightkit#301 | DONE | rightkit-spend crate (reserve/allowance/CAS ledger); 0.2.0 published |
| rightkit#302 | DONE | rightkit-llm `PriceCatalogue` / `CatalogueRow` (commit 7bbc3dc7d), published |
| rightkit#303 | DONE | rightkit-llm exact-match pricing, `PriceError`/`try_cost_of` (816590c62); 0.3.2 published |
| rightkit#304 | DONE | rightkit-llm AttemptBilling/Spend and ChatRequest::report_failed_usage (CHANGELOG cites #304); 0.3.2 is published. |
| rightkit#312 | DONE | rightkit-contracts 0.1.8 `EYES_APPROVAL_REQUIRED` and `EyesGate`; published |
| rightkit#313 | DONE | rightkit-llm 0.3.2, rightkit-review 0.4.1 and rightkit-contracts 0.1.8 are on crates.io; rightkit-scenario no longer exists in crates/ |
| rightkit#315 | SUPERSEDED | docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md rules Hetzner out; Linux lanes go through the Linux direct origin/qualification |
| rightkit#317 | DONE | rightkit-llm `cli` feature (CliProvider, claude_code, codex_exec) is in 0.3.0; 0.3.2 is on crates.io. |
| rightkit#321 | DONE | rightkit-motion 0.2.x: beat templates plus `cmd::contactsheet`; 0.2.1 published (bracketed-script span contract unconfirmed) |
| rightkit#324 | DONE | rightkit-asr `LanguageTranscription` reports the language on every backend (commit cd92a8b4c); asr 0.4.0 published |
| rightkit#325 | DONE | rightkit-asr `decoder_intra_threads` (05adcc264); 0.4.0 published |
| rightkit#326 | DONE | rightkit-audio 0.3.1: read_wav_16k_mono makes a single open with a capacity hint. |
| rightkit#328 | DONE | rightkit-spend `Scope` (19f113635), published 0.2.0 |
| rightkit#329 | DONE | rightkit-channel-memory 0.2.0 structured `PermittedClaim`; 0.2.2 published |
| rightkit#330 | DONE | rightkit-channel-memory has `validFrom` / `validUntil` (CHANGELOG #330); published |
| rightkit#331 | DONE | rightkit-channel-memory exact `Decimal` proof value (CHANGELOG #331); 0.2.2 published |
| rightkit#334 | DONE | rightkit-review rules.rs carries the det.text.* rule kinds including text claim proof; 0.4.1 is published. |
| rightkit#335 | DONE | rightkit-contracts 0.1.6 `ruleRevisionRef` on briefs and envelopes (8f6cebb61), published 0.1.8 |
| rightkit#344 | DONE | rightkit-sync paged pass (`list_page`) and no per-mutation ledger clone, commits d744aa879 and 538fa9bdc; 0.2.2 published |
| rightkit#345 | DONE | @rightkit/release `winPackage` / `macPackage` targets and the one entrypoint shipped in 0.2.173 (commit 4dfad124c); release is at 0.6.36 |
| rightkit#346 | DONE | release disk job (hourly, stray cargo targets, scratch/holding swept after 1 day; CHANGELOG 304-310, 3aebe9ef5); 0.6.36 published |
| rightkit#350 | DONE | rightkit-compose `SignedTicks`, `frame_to_ticks`, `Rational::from_frames_rounded`, published 0.2.2 |
| rightkit#351 | DONE | rightkit-compose `opacity` module (validated `Opacity`, `FrameFade`); 0.2.2 published |
| rightkit#352 | DONE | rightkit-speech-client 0.4.1 published; CHANGELOG covers staging unreadable inputs inside the client |
| rightkit#353 | DONE | rightkit-ffmpeg `EncoderInventory` encoder selection (CHANGELOG #353); 0.2.13 published |
| rightkit#354 | DONE | rightkit-ffmpeg 0.2.9 handle module: ToolHandle<V> as Verified or Development; 0.2.13 is published. |
| rightkit#356 | DONE | rightkit-review `qa` report model with Pass/Fail/NotApplicable and Warning severity-aware blocking, published 0.4.1 |
| rightkit#357 | DONE | rightkit-review `delivery` rules (`DELIVERY_CHECK_VERSION`); 0.4.1 published |
| rightkit#359 | DONE | rightkit-playback `set_asset_scope` (rightkit#359) in 0.3.0, published |
| rightkit#360 | DONE | rightkit-automation Windows UIA gaps closed (CHANGELOG #360); 0.6.4 published |
| rightkit#361 | DONE | RightKitSwift 0.9.0 is tagged: RightKitOverlay product, setClickThrough, mutable canBecomeKey; macOS 14 stays the floor. |
| rightkit#363 | DONE | RightKitSwift 0.9.0 (tagged): RightKitAudio `WarmAudioCapture` plus recorder |
| rightkit#364 | DONE | RightKitSwift 0.9.0 (tag exists): public CoreMLModelValidator, loader, model-download coordinator |
| rightkit#365 | DONE | RightKitSwift tag 0.9.0 exists; RightKitHTTP, RightKitAccount link-code and license-refresh APIs are in Sources |
| rightkit#366 | DONE | http 0.3.9, db 0.5.3, spend 0.2.0, platform-rules 0.1.0, credentials 0.4.4, qa 0.10.0 and @rightkit/theme are all on the registries |
| rightkit#367 | DONE | rightkit-time 0.1.0/0.1.1 is published (RFC 3339, offsets, civil dates); a named-zone tz database was deliberately not covered. |
| rightkit#369 | DONE | rightkit-time 0.1.0 RFC3339 and HTTP-date helpers (rightkit#367/#369), published 0.1.1 |
| rightkit#370 | DONE | rightkit-crypto 0.1.0 (`hmac_sha256`, `ct_eq`, base64/base64url); published |
| rightkit#379 | DONE | rightkit-daemon `remote-loopback` feature has bearer credentials with per-IP limits; daemon 0.5.1 published |
| rightkit#386 | DONE | rightkit-tauri-shell (0.7.0 published): MenuEntry check and submenu, webview2_menu.rs, print.rs ShowPrintUI, WindowSpec.parent, open_paths. |
| rightkit#390 | DONE | rightkit-logs recording gate and batch cap, shell `rightkit_logs_*` commands, `@rightkit/logs` 0.1.6 recorder and crash capture |
| rightkit#399 | DONE | rightkit-fs `trash` module (`move_to_trash`, `list`, `lookup`, `restore`); fs 0.4.9 published |
| rightkit#408 | DONE | rightkit-mesh 0.1.1 is on the registry, with a credentials-backed KeyStore feature (Cargo.toml `dep:rightkit-credentials`) |
| rightkit#409 | DONE | rightkit-mesh `devicelink` (pairing, resume tickets, grants, registry); 0.1.1 published |
| rightkit#419 | DONE | crates/rightkit-agent-transcript has claude_events, codex_events and discover_open (adapters.rs, source.rs) |
| rightkit#424 | DONE | RightKitSwift 0.9.0 Pulse notch parity (event tap hub, keystroke post, processes, login item) |
| rightkit#425 | DONE | RightKitSwift 0.9.0 tag (commit 03640c242) carries the RightKitHub/engine move |
| rightkit#429 | DONE | rightkit-onnx 0.2.6 is published; registry now has 0.2.9 |
| rightkit#432 | DONE | packages/release/msix-appinstaller.mjs (MSIX install, interrupt, local feed); 0.6.36 published |
| rightkit#437 | DONE | @rightkit/ci `workflows` manifest block (#465, #437) and swift-toolchain step; ci 0.3.2 published |
| rightkit#439 | DONE | `right-git artifacts fetch` shipped in @rightkit/ci (CHANGELOG #439); ci 0.3.2 published |
| rightkit#441 | DONE | RightKitAccessibility `boundsForRange`/`parameterized` in RightKitSwift 0.9.0 (tag on Orthic-Labs/rightkit) |
| rightkit#449 | DONE | rightkit-audio 0.3.1 published with `playback::OutputSession` |
| rightkit#454 | DONE | rightkit-audio `align_mic` (CHANGELOG #454); 0.3.1 published |
| rightkit#457 | DONE | rightkit-ffmpeg DecodeOptions gains max duration and stream selector (CHANGELOG cites #457); 0.2.13 is published. |
| rightkit#459 | DONE | rightkit-transcript 0.4.0 `TimedWord::source_ms`, `LineMetrics`, `wrap_lines` |
| rightkit#460 | DONE | rightkit-platform-rules 0.1.0: hashtag max, `LinkPlacement`, route dimension, revision-id parse; published |
| rightkit#461 | DONE | rightkit-typeset 0.1.0 includes the `font` module (FontFace::parse, advance_units, measure); published |
| rightkit#462 | DONE | rightkit-spend 0.2.0 `reserve_in`, multi-allowance, carry-over (CHANGELOG) |
| rightkit#463 | DONE | rightkit-browser 0.3.4 (set_transparent_background, screenshot_with) and rightkit-motion render/encode 0.2.0; both are published. |
| rightkit#465 | DONE | @rightkit/ci 0.3.0 `workflows` manifest block with custom lanes, paths triggers and steps (rightkit#465), published 0.3.2 |
| rightkit#468 | DONE | rightkit-qa 0.10.x scenario worlds: isolated home, control token file, `userdata::Guard`; 0.10.0 published (0.10.2 staged) |
| rightkit#471 | DONE | rightkit-spend 0.2.0 has all five shims natively (commit-unknown, account scope, schema v2), published |
| rightkit#473 | DONE | duplicate of #462; same rightkit-spend 0.2.0 |
| rightkit#476 | DONE | rightkit-motion 0.2.0 render/encode owner (CHANGELOG cites #476); duplicate of #463. |
| rightkit#477 | DONE | rightkit-crypto 0.1.0 `hmac_sha256` and `ct_eq`, published |
| rightkit#479 | DONE | rightkit-ffmpeg one-call resolver, `override_first`, `encoders` and `handle` modules; 0.2.13 published |
| rightkit#496 | DONE | rightkit-embed `ResidentIndex` (int8) and `reciprocal_rank_fusion` replace the sqlite-vec ask; embed 0.6.2 published |
| rightkit#499 | DONE | rightkit-credentials `os-auth` feature (Windows Hello UserConsentVerifier, macOS LAContext); 0.4.4 is published. |
| rightkit#503 | DONE | rightkit-identity 0.1.0 `build_info` / `BuildIdentity`, published |
| rightkit#506 | STALE | 2026-10-17 port-gap triage tracker and patch queue; every gap is now its own issue or already shipped |
| rightkit#508 | DONE | RightKitSwift is distributed from public Orthic-Labs/rightkit by tag; versions.json says the private mirror was deleted |
| rightkit#521 | DONE | RightKitSwift 0.9.0 `RightKitOverlayPanel.allowsKey` settable at runtime |
| rightkit#522 | DONE | RightKitSwift 0.9.0 `RightKitOverlay` styleMask option |
| rightkit#523 | DONE | RightKitOverlayFullScreen split is in RightKitSwift 0.9.0 (commit 03640c242) |
| rightkit#525 | DONE | rightkit-overlay `covering_window` (CHANGELOG #525); 0.2.0 published |
| rightkit#528 | DONE | rightkit-docs 0.1.1 active-content refusal (CHANGELOG cites #528); 0.1.3 is published. |
| rightkit#529 | DONE | rightkit-automation `policy::credential_label` and `classify_risk` follow Pulse word lists, published 0.6.4 |
| rightkit#530 | DONE | rightkit-spend 0.2.0 `Budget` scopes (`caller_budget`, `set_process_budget`) with reserve/report; published |
| rightkit#533 | DONE | rightkit-hooks `compose_reply` keeps a notice-only systemMessage (CHANGELOG); hooks 0.2.1 published |
| rightkit#534 | DONE | rightkit-hooks `ReceiptStore::membrane` shared store (0.2.0); 0.2.1 published |
| rightkit#537 | DONE | rightkit-send Entry::open_reader (CHANGELOG cites #537); 0.3.0 is published. |
| rightkit#543 | DONE | rightkit-hooks 0.2.0 `membrane` module with `HookModuleId` and the hook wire types |
| rightkit#545 | DONE | oauth 0.10.0, mcp 0.7.2, browser 0.3.7, http 0.3.9 and daemon 0.5.1 are on crates.io, so the publish gap is closed |
| rightkit#546 | DONE | rightkit-logs `stderr_layer` (CHANGELOG #546); 0.3.7 published |
| rightkit#547 | DONE | rightkit-file-watch session manager (CHANGELOG "Added (session manager, #547)"); 0.4.2 is published. |
| rightkit#548 | DONE | rightkit-overlay `stack::place` and `StackLayout`, published 0.2.0 |
| rightkit#549 | DONE | rightkit-mcp `LeasedPool<K,V>` (acquire, epoch, evict_idle, snapshot); mcp 0.7.2 published |
| rightkit#550 | DONE | rightkit-mcp `TrustStore::gate` and `gate_stdio` shipped (CHANGELOG #550); mcp 0.7.2 published |
| rightkit#551 | DONE | rightkit-hooks `agent_hooks` host runner (HookSpec, ChainReport in chain.rs); 0.2.1 published |
| rightkit#553 | DONE | RightKitAppleCredentials SyncedEnvelopeStore is in the Swift README and tagged 0.9.0. |
| rightkit#558 | DONE | rightkit-sandbox `confine_command` for Linux; sandbox 0.6.2 published |
| rightkit#559 | DONE | rightkit-sandbox WriteOnly mode and `exec_any` (CHANGELOG #559); 0.6.2 published |
| rightkit#560 | DONE | rightkit-sandbox 0.5.0 deny_read and secret-root planner (CHANGELOG cites #560); 0.6.2 is published. |
| rightkit#561 | DONE | rightkit-sandbox `denial_report.rs` `DenialReport` with `requested_grant` and `protected_secret`, published 0.6.2 |
| rightkit#562 | DONE | rightkit-sandbox `spawn_piped` (tokio async piped AppContainer spawn); sandbox 0.6.2 published |
| rightkit#564 | DONE | rightkit-sandbox `AppContainerProfile::for_workspace` with grant leases and loopback; published |
| rightkit#565 | DONE | rightkit-embed `bundle_files`; 0.6.2 published |
| rightkit#566 | DONE | rightkit-sandbox 0.5.0 pin_read_roots (CHANGELOG cites #566); 0.6.2 is published. |
| rightkit#568 | DONE | rightkit-agents claude.rs lossless parse (rate_limit_event, results, usage); agents 0.2.0 published |
| rightkit#569 | DONE | rightkit-agents `UnknownPolicy::FailClosed` is in the 0.2.0 section, published |
| rightkit#570 | DONE | rightkit-agents `claude::initialize_frame`/`set_model_frame`/`parse_control_response`, already in published 0.2.0 |
| rightkit#572 | DONE | rightkit-agents 0.2.0 `rate_limit::RateLimitState` plus `driver_host` serde projection |
| rightkit#573 | DONE | rightkit-agents codex.rs usage-limit event and `launch_env`; agents 0.2.0 published |
| rightkit#574 | DONE | rightkit-agents discovery has PATHEXT handling, bounded vendor probes and `vendor_clis_disabled` (0.2.0) |
| rightkit#575 | DONE | rightkit-browser observe `frame_id`/`stable_id`/`sensitive` (CHANGELOG #575); 0.3.7 published |
| rightkit#576 | DONE | rightkit-browser 0.3.4: BrowserPage::history waits at most 10 s (CHANGELOG cites #576); 0.3.7 is published. |
| rightkit#577 | DONE | rightkit-browser `accessibility_tree_with(depth)` and `frames()`, published 0.3.7 |
| rightkit#578 | DONE | rightkit-browser `new_page_with(url, PageSetup { interceptor, download })`; published |
| rightkit#579 | DONE | `BrowserPage::scroll_by(target, dx, dy)` shipped (browser CHANGELOG #579); browser 0.3.7 published |
| rightkit#581 | DONE | rightkit-ffmpeg command::extract_args (CHANGELOG cites #581); 0.2.13 is published. |
| rightkit#582 | DONE | rightkit-embed `installed_model_dir`, published 0.6.x |
| rightkit#583 | DONE | rightkit-llm `agent_providers` subscription auth (moved from CodeRight); llm 0.3.2 published |
| rightkit#584 | DONE | rightkit-llm `SchemaConstraint` and `ChatRequest::with_response_schema` (CHANGELOG #584), published |
| rightkit#585 | DONE | rightkit-llm agent_providers replay harness (`-replay-record`); 0.3.2 published |
| rightkit#586 | DONE | rightkit-oauth 0.8.0 with_token_param (RFC 8707 resource); 0.10.0 is published. |
| rightkit#587 | DONE | rightkit-oauth `CallbackBind::redirect_host` and `custom_bind_address`, published 0.10.0 |
| rightkit#588 | DONE | rightkit-oauth `CallbackListener::wait_post_json` (CORS preflight); oauth 0.10.0 published |
| rightkit#589 | DONE | rightkit-oauth `GatewayExchange::exchange_tokens` / `exchange_raw` (CHANGELOG #589), published |
| rightkit#594 | DONE | RightKitLiveActivity keyed host (Swift CHANGELOG cites 594), tagged 0.9.0. |
| rightkit#595 | DONE | RightKitSwift 0.9.0 speech lane, issues 363/364/595/597 |
| rightkit#597 | DONE | RightKitSwift 0.9.0 RightKitAudio session interruption handling (#597) |
| rightkit#598 | DONE | rightkit-search `Keyset::range_bound` and `cursor_expr` (CHANGELOG #598); search 0.4.1 published |
| rightkit#599 | DONE | RightKitSwift 0.9.0 (issue 599 in CHANGELOG) |
| rightkit#600 | DONE | rightkit-oauth MemoryTokenStore (CHANGELOG cites #600); 0.10.0 is published. |
| rightkit#603 | DONE | RightKitSwift 0.9.0 round-2 license lane (issues 601-605), `LicenseClient.platform()` |
| rightkit#604 | DONE | RightKitSwift RightKitLicense `ContinuityBridgeTransport`; swift 0.9.0 |
| rightkit#605 | DONE | AppleCredentialStore `accountValidator` is in RightKitAppleCredentials (tag 0.9.0) |
| rightkit#606 | DONE | RightKitFS round-2 readData/exists/remove in RightKitSwift 0.9.0 |
| rightkit#607 | DONE | RightKitFS.migrate(from:to:) in RightKitFS.swift, tagged 0.9.0. |
| rightkit#616 | DONE | @rightkit/i18n 0.2.0 locale registry, persistence and function messages |
| rightkit#619 | DONE | @rightkit/onboarding 0.6.x onContinue, onComplete, skippable and labels (#619); 0.6.3 published |
| rightkit#628 | DONE | RightKitTTS product in RightKitSwift 0.9.0 |
| rightkit#631 | DONE | @rightkit/doc-render MarkdownRenderer components and onText (CHANGELOG cites #631); npm 0.2.1 is published. |
| rightkit#632 | DONE | @rightkit/doc-render `parseToTree` options, published 0.2.1 |
| rightkit#633 | DONE | @rightkit/doc-render 0.2.1 `CodeBlock`; published |
| rightkit#635 | DONE | rightkit-models `probe_with_runner` (CHANGELOG #635); 0.5.3 published |
| rightkit#636 | DONE | rightkit-models tiered fit (CHANGELOG cites #635/#636/#637); 0.5.3 is published. |
| rightkit#637 | DONE | rightkit-models runner probes (#635-#637), published 0.5.x |
| rightkit#641 | DONE | rightkit-browser `design_mode` (CHANGELOG #641); 0.3.7 published |
| rightkit#645 | DONE | rightkit-overlay style_has_no_frame and WS_BORDER_BIT (CHANGELOG cites #645); 0.2.0 is published. |
| rightkit#656 | SUPERSEDED | async providers on `rightkit_http::AsyncClient` landed as rightkit-llm `agent_providers` (CodeRight engine moved in), published 0.3.2 |
| rightkit#657 | DONE | rightkit-llm agent_providers transport `StreamPolicy` and `IncompleteOutput`; llm 0.3.2 published |
| rightkit#659 | DONE | rightkit-llm `CircuitBreaker` (CHANGELOG #659); 0.3.2 published |
| rightkit#660 | DONE | rightkit-llm `health` module (RouteHealth, HealthState, snapshot; CHANGELOG cites #660); 0.3.2 is published. |
| rightkit#661 | SUPERSEDED | rightkit-llm `FailoverEvent.breaker_skipped` plus `agent_router` / `agent_providers`, published 0.3.2 |
| rightkit#662 | DONE | rightkit-llm agent_providers `model_metadata` (tier, efforts, compaction) moved from CodeRight; 0.3.2 published |
| rightkit#663 | DONE | rightkit-llm image generation client and image-route catalogue (CHANGELOG #663), published |
| rightkit#664 | DONE | rightkit-llm `ToolCallChunkReassembler` (CHANGELOG #664); 0.3.2 published |
| rightkit#665 | DONE | rightkit-llm agent_providers has the Responses and codex/responses transport (client/transport.rs); 0.3.2 is published. |
| rightkit#667 | DONE | RightKitSwift 0.9.0 `NetPolicy` loopback/private allow-set and policy-aware session |
| rightkit#670 | DONE | rightkit-tauri-shell `associations.rs` (file-association status and defaults); tauri-shell 0.7.x on registry |
| rightkit#672 | DONE | tauri-shell host `rightkit_settings_*`/`models_*`/`secrets_*` handlers (CHANGELOG 62-68); 0.7.0 published |
| rightkit#673 | DONE | @rightkit/legal 0.6.0 ships the rightkit-legal npm-licenses and rust-licenses CLI; npm has 0.6.3. |
| rightkit#675 | DONE | `right-release icons ico/icns` (release CHANGELOG, #675) |
| rightkit#678 | STALE | filed against rightkit-qa 0.6.1 on the Dell; qa is now 0.10.2 and QA moved out of build (GC-A07) |
| rightkit#679 | DONE | rightkit-asr top-k fix (d0a76d336); 0.4.0 published |
| rightkit#681 | STALE | one-off 2026-10-18 publication run report; the crates are on crates.io (rightkit-versions.json registry) and the publish route is now local. |
| rightkit#685 | DONE | rightkit-child-process `WindowsJob::contain_current_process_with` supports breakaway, published 0.5.7 |
| rightkit#688 | STALE | one-off qualification run report for record/pdf ignored asset tests |
| rightkit#693 | DONE | rightkit-ffmpeg export-ass catalog row and `ResolveOptions::override_first`; 0.2.13 published |
| rightkit#698 | DONE | rightkit-credentials `apple_helper::resolve_bundled_login_item`; 0.4.4 published |
| rightkit#699 | DONE | RightKitAppleCredentials opt-in broad synced read is in tag 0.9.0 (commit 03640c242) |
| rightkit#701 | DONE | RightKitLicense ContinuityBridgeClient (ContinuityBridge.swift), tagged 0.9.0. |
| rightkit#702 | DONE | RightKitSwift 0.9.0 round-2 license lane (702): `requireDeviceID` options |
| rightkit#713 | DONE | rightkit-logs `DetachedLog::stderr_file()` owner-only append handle for detached child stderr |
| rightkit#723 | DONE | release broker-wire.mjs REFUSAL_CLASSES handle RK_ACTIVATION_PENDING / RK_JOB_SUSPENDED; 0.6.36 published |
| rightkit#724 | DONE | @rightkit/release 0.4.1 fixed duplicate archive member names, and 0.6.7 stage-sidecar native-library stages DirectML with the ORT row. |
| rightkit#727 | DONE | rightkit-models install `download_url` rewrite and ordered mirrors; 0.5.3 published |
| rightkit#742 | DONE | rightkit-transcript `TimedWord::from_seconds`/`start_secs` keep sub-ms; 0.4.0 published |
| rightkit#745 | DONE | rightkit-onnx `developer_override_candidate` documented (README/CHANGELOG, dev-overrides); 0.2.9 published |
| rightkit#769 | DONE | rightkit-db `WalCheckpointJob` / `WalCheckpointPolicy` with PASSIVE then RESTART (CHANGELOG); db 0.5.3 published |
| rightkit#777 | DONE | build-control-policy write hook no longer prints top-level decision:allow (b6436353b) |
| rightkit#778 | STALE | one-off Muse M1–M23 remediation takeover (2026-10-18); work long since landed or superseded by ownership waves |
| rightkit#784 | DONE | Pulse registered as bogusyogi/pulse (bbd474cea, d552754c6) |
| rightkit#787 | DONE | rightkit-sandbox pins.rs `pinned_read_roots` now `#[cfg(any(test, not(windows)))]`, published 0.6.2 |
| rightkit#788 | DONE | rightkit-daemon fix for a released lock reading as held on Unix (daemon CHANGELOG, #788); 0.5.1 published |
| rightkit#791 | SUPERSEDED | rightkit#1471: npm publishes locally with provenance skipped by decision |
| rightkit#796 | DONE | rightkit-job-queue `SupervisorConfig::with_handshake` (CHANGELOG #796); job-queue 0.5.1 published |
| rightkit#797 | SUPERSEDED | by #346 disk hygiene (floors, 1-day holding, scratch and cargo-target sweep) |
| rightkit#801 | DONE | rightkit-device 0.3.1 raised the license floor (commit 9d3471ace); device 0.4.0 published |
| rightkit#802 | DONE | rightkit-job-queue serve reaps finished join handles (CHANGELOG #802); in published 0.5.1 |
| rightkit#804 | DONE | rightkit-logs `DetachedLog` with `Drainer` and `DrainReport` (GenRight #10), published |
| rightkit#807 | SUPERSEDED | `license-notices cargo`/`binaries` routed in release (CHANGELOG 471); lock binding continues as #1445 |
| rightkit#809 | DONE | tauri-shell single_instance_destroy test now uses settled claims (5adff94c8) |
| rightkit#810 | STALE | preliminary Windows idle-wakeup measurement at rightkit-qa 0.7.1; version long passed (qa 0.10.x) |
| rightkit#811 | DONE | @rightkit/qa-chrome 0.1.12 close() uses a single time budget and a fixed Windows scanner (CHANGELOG #811) |
| rightkit#813 | STALE | one-off 2026-10-19 Dell find / incident report; the rule is an agent-behaviour policy, not RightKit code. |
| rightkit#814 | DONE | rightkit-llm 0.3.x `cli` feature depends on rightkit-child-process 0.5.7 |
| rightkit#815 | STALE | @rightkit/tauri 0.2.3 / logs 0.1.4 mismatch; superseded by @rightkit/tauri 0.3.0 and @rightkit/logs 0.1.6 |
| rightkit#817 | DONE | rightkit-embed on rightkit-db 0.5.0 (3b955f5c7); 0.6.2 published |
| rightkit#818 | SUPERSEDED | same failed-open WAL loss fixed by #932 in rightkit-db 0.5.2 (tests/failed_open_keeps_wal.rs); published. |
| rightkit#822 | SUPERSEDED | locked M1–M23 packet plan for the CI publication route, replaced by local publish (rightkit#1471) |
| rightkit#824 | SUPERSEDED | right-release 0.5.0 PIPELINE entry contract replaced by the locked GATE-CONTRACT and pipeline-admission.mjs in 0.6.x; 0.6.36 is current. |
| rightkit#829 | STALE | hosted npm qualification prerequisites for a CI route that is no longer used |
| rightkit#830 | DONE | rightkit-fs `AtomicWriter::create_new_private` (CHANGELOG #830); 0.4.9 published |
| rightkit#831 | DONE | rightkit-credentials db-adapter CredentialsKeyStore returns an owned ProtectedKeySession; 0.4.4 is published. |
| rightkit#832 | SUPERSEDED | shared CI publish/qualify routes are retired; publishing is local from the Mac (rightkit#1471, docs/CI.md) |
| rightkit#834 | STALE | CI pipeline adversarial review and publication gate closure; CI publish routes are retired, publishing is local (#1471) |
| rightkit#835 | DONE | rightkit-child-process 0.5.5 sanitizes inherited descriptors (commit 191e3dc33); 0.5.7 published |
| rightkit#836 | DONE | detached logger idle drain fix (40dd170e1); rightkit-logs 0.3.7 published |
| rightkit#837 | DONE | rightkit-child-process 0.5.6/0.5.7 foreign sweep and signal no longer report an exiting process as failed; 0.5.7 is published. |
| rightkit#839 | SUPERSEDED | leftover-code register review replaced by the wave-2 port exception and ledger triage rows |
| rightkit#840 | STALE | Developer ID credentials qualification profile for the retired CI publication infrastructure (#778/#822) |
| rightkit#841 | DONE | rightkit-crash omits unsafe renderer fields before redaction (cbc619cd8); 0.3.6 published |
| rightkit#842 | STALE | provisioning a signer for frozen @rightkit/release 0.4.1 qualification; that release is long superseded (0.6.36) and CHANGELOG 0.4.1 corrected the signing routes. |
| rightkit#843 | STALE | qualification via CI publish workflows; CI routes are not in use and RightKitSwift 0.9.0 is already tagged |
| rightkit#844 | STALE | 2026-10-21 pending-package publication takeover; superseded by the local publish route (#1471) and this publish-all pass |
| rightkit#848 | DONE | RightKitSwift 0.9.0 `RightKitOverlayPanel.allowsKey` (issues 361/521/522/523) |
| rightkit#849 | DONE | rightkit-mesh crate (iroh, mailbox bin) on crates.io 0.1.1 |
| rightkit#850 | DONE | rightkit-credentials 0.4.x with `os-auth` is published (0.4.4) |
| rightkit#852 | DONE | RightKitSwift 0.9.0 tagged: RightKitAccessibility element(atPosition:) and RightKitEngineClient. |
| rightkit#853 | DONE | @rightkit/release 0.6.7 stage-sidecar `remote` extract with nested member path (#853, #973) |
| rightkit#855 | DONE | rightkit-sandbox PtySession output cursor and total written are in src (0.6.2) |
| rightkit#860 | DONE | rightkit-license 0.2.6, rightkit-tauri-shell 0.7.x and @rightkit/license 0.3.0 are published |
| rightkit#862 | DONE | rightkit-tauri-shell host/legal.rs registers rightkit_legal_receipt_read, create and reset (0.5.0); 0.7.0 is published. |
| rightkit#870 | DONE | rightkit-fs `ensure_private_dir_repairing` and `private_cached` handle an Administrators-owned folder; fs 0.4.9 published |
| rightkit#874 | DONE | rightkit-automation 0.6.4 adds UIA focus and element probes; (macOS parent/CFURL read not confirmed, reopen if Pulse still lacks it) |
| rightkit#875 | DONE | @rightkit/issues-tracker 0.1.0 published (`right-issues`, commit 81a445659) |
| rightkit#877 | SUPERSEDED | by #952 (customer-grade relay admission service) |
| rightkit#878 | DONE | docs/E2E-TESTING-STANDARD.md and the rightkit-qa journeys module (journeys.toml runner) are on main; qa 0.10.0 is published. |
| rightkit#879 | DONE | stage-sidecar `isDebugSymbolMember` skips the .dSYM DWARF copy (packages/release/stage-sidecar.mjs) |
| rightkit#880 | STALE | 2026-10-21 publish-order hold (app-update 0.4.1, settings-ui 0.7.0, credentials 0.4.0); versions long superseded |
| rightkit#883 | DONE | rightkit-license guard-retention tests made deterministic (eb98a9478) |
| rightkit#885 | STALE | red CI run at 8d8a6a63 on 2026-10-21; D6 says no GitHub Actions CI and these jobs are gone. |
| rightkit#886 | DONE | rightkit-credentials 0.4.2 `handoff` module and rightkit-license engine snapshot (fa69fef50) |
| rightkit#887 | DONE | rightkit-sandbox Windows `deny_read` fails closed (sandbox CHANGELOG, #887); 0.6.2 published |
| rightkit#889 | DONE | onnx 0.2.7 and ffmpeg 0.2.10 moved to ORT 1.30 with an SME2 leak guard (commit 7428140b8) |
| rightkit#890 | DONE | dependency and RustSec bump (063788a6b, 95457bb) |
| rightkit#892 | DONE | rightkit-automation `desktop-context` feature (frontmost and focus continuity, moved from heardright_platform), published 0.6.4 |
| rightkit#893 | DONE | rightkit-ownership rule `appkit.autorelease.thread-loop-no-pool`; ownership 0.4.4 |
| rightkit#895 | DONE | rightkit-qa runs are functional by default with no screenshots (CHANGELOG 164) |
| rightkit#896 | DONE | rightkit-overlay-win 0.1.0 (Win32 native overlay renderer) is on crates.io. |
| rightkit#897 | DONE | rightkit-models `parakeet_ultra_spec` (88c514e4f), published |
| rightkit#898 | DONE | rightkit-models `suite::refs::release` and retired-entry pruning; 0.5.3 published |
| rightkit#899 | DONE | rightkit-fsindex 0.5.0 write budget, rescan deferral and footprint cap (commit 48eebdc84); 0.6.0 published |
| rightkit#900 | SUPERSEDED | rule now lives in GATE-CONTRACT/decisions.md (aef607782) and GC-A11..A13 |
| rightkit#901 | DONE | rightkit-device 0.4.0 depends on rightkit-credentials 0.4.3; published. |
| rightkit#903 | DONE | rightkit-models runtime-artifact lane accepts `rightsites-*` buckets (#903); 0.5.3 published |
| rightkit#904 | DONE | rightkit-tauri-shell supports both webview2-com 0.38 and 0.39 generations (Cargo.toml); 0.7.x published |
| rightkit#905 | STALE | debug dev build under the broker target dir deleted by GC-A07 (e6982603d, 3b032533b) |
| rightkit#906 | DONE | desktop-app-template lock now carries rustls 0.23.46 and the fixtures are declared in ci/gate.config.json. |
| rightkit#907 | STALE | one-off harness repin against Pulse main of 2026-10-22; Pulse has moved on and `docs/bridge-vectors` is not in RightKit |
| rightkit#909 | STALE | pulse#321 asks for plan answers from the M3/M4 engine-split era; Pulse has since moved on |
| rightkit#910 | DONE | rightkit-fsindex RKFSIX03 compact store (48eebdc84); 0.6.0 published |
| rightkit#912 | STALE | @rightkit/release 0.4.1 versions-map mismatch for the HeardRight 0.1.230 seal; the release is long superseded. |
| rightkit#916 | STALE | hosted Windows CI flake in policy_e2e; CI routes are not in use and browser is 0.3.7 |
| rightkit#918 | DONE | release 0.6.1 ignores app path crates (fced9f07a); legal 0.6.3 |
| rightkit#922 | DONE | rightkit-db keeps `-wal` through quarantine (rightkit#932) and has `SidecarPolicy`; db 0.5.3 published |
| rightkit#923 | SUPERSEDED | GC-A11: Class A cargo metadata runs directly with the real cargo, not as a broker call |
| rightkit#925 | DONE | rightkit-browser 0.3.6: the network policy callback now decides browser-internal egress (CHANGELOG cites #925); 0.3.7 is published. |
| rightkit#927 | DONE | @rightkit/release 0.6.3 doctor scans the same iOS ownership scope as the build (rightkit#927) |
| rightkit#929 | DONE | commit 07fe52d0d: rk-pipeline watch never hangs silently; npm visibility wait 15 min |
| rightkit#930 | DONE | rightkit-automation 0.6.1 `win::bounded` lanes refuse while busy (commit e0901f618) |
| rightkit#931 | STALE | hosted Windows-CI Chrome stall; CI routes unused, rightkit-browser 0.3.7 published |
| rightkit#932 | DONE | rightkit-db 0.5.2 keeps -wal/-shm on failed open (CHANGELOG cites #932); 0.5.3 is published. |
| rightkit#933 | DONE | rightkit-child-process 0.5.7 Windows reap/sweep no longer reports an exiting process as failed (rightkit#1260) |
| rightkit#935 | STALE | hardcoded unit version in a hosted qualification consumer recipe; hosted qualification is retired |
| rightkit#940 | DONE | Right Suite API contract and `suite_api` projection in rightkit-license (commit 7129a65d5) |
| rightkit#946 | DONE | doctor and build share one admission contract (rightkit#982, commit 986c4da7a); release 0.6.36 |
| rightkit#962 | STALE | three-OS CI superseded by the local broker/dual-host gate (#900); CI publish routes not in use |
| rightkit#973 | DONE | release stage-sidecar `native-library` kind handles the DirectML nupkg (CHANGELOG 320); 0.6.36 published |
| rightkit#975 | DONE | rightkit-daemon reserved probe slots, handshake deadline and `TableFull` event (0e65b08e8), published 0.5.x |
| rightkit#977 | DONE | rightkit-db backup uses one total deadline, not busy_timeout (CHANGELOG, scripted-clock tests); published |
| rightkit#978 | DONE | Apple Speech removed in RightKitSwift 0.9.0 (521f20426) |
| rightkit#980 | STALE | one-off requalification of Oct-24 unmanaged compiler runs; superseded by the gate/broker contract (GC-A06, GC-A08) |
| rightkit#981 | DONE | stage-sidecar uses vswhere and the Visual Studio bundled generator tool (stage-sidecar.mjs `VSWHERE_PARTS`); release 0.6.36 |
| rightkit#982 | DONE | doctor equals build; deterministic check first (commit 986c4da7a, release 0.6.x) |
| rightkit#991 | DONE | rightkit-license `with_revoked_ids` on engine and lifecycle configs; 0.2.6 published |
| rightkit#1014 | DONE | rightkit-ffmpeg handle.rs accepts an ordered variants list (`variants(...)` builder); 0.2.13 is published. |
| rightkit#1018 | SUPERSEDED | `dialog-qa` removed before @rightkit/tauri 0.3.0; QA code is out of release builds (GC-A07) |
| rightkit#1029 | DONE | stage-sidecar ffmpeg `resourceDir` (CHANGELOG 319); 0.6.36 published |
| rightkit#1040 | DONE | ownership-dependency-allowlist.json carries tree-sitter-rust and xxhash-rust; @rightkit/release 0.6.36 is published. |
| rightkit#1044 | DONE | every release command loads right-release.config.json (CHANGELOG 356); assembled-app path exists; 0.6.36 published |
| rightkit#1055 | DONE | tauri-shell `LogsRuntime::clear` returns `ClearReport` and drains (0.7.0 CHANGELOG); 0.7.0 published |
| rightkit#1085 | DONE | @rightkit/onboarding `readAcceptance` validates eligibilityBasis; 0.6.3 published |
| rightkit#1096 | DONE | rightkit-license native engine entitlements plus rightkit-models `EntitlementError` / `ModelStore` gating (license 0.2.6, models 0.5.3) |
| rightkit#1127 | DONE | `SyncedCredentialPolicy` in RightKitAppleCredentials, RightKitSwift 0.9.0 tag |
| rightkit#1129 | SUPERSEDED | `CredentialTokenSource::from_record_store` deprecated in rightkit-license 0.2.6; the engine reads the owner snapshot (#886) |
| rightkit#1143 | DONE | RightKitSwift 0.9.0 is tagged (RightKitOAuth, ContinuityBridgeClient, SyncedCredentialPolicy on main). |
| rightkit#1165 | DONE | rightkit-onnx `bind_installed_runtime` (BindSpec/BoundRuntime); 0.2.9 published |
| rightkit#1168 | STALE | publish.yml `verify` actionlint; CI publish route unused and the @rightkit/ci tests are gone |
| rightkit#1170 | DONE | RightKitSwift 0.9.0 tagged with AppLockCoordinator, UserVerification and OwnerKey |
| rightkit#1180 | STALE | flaky Windows run on a hosted GitHub runner; D6 says no GitHub CI, and rightkit-db 0.5.3 has since moved on. |
| rightkit#1185 | STALE | hosted-CI `npm-qa-chrome` job red on old PRs; CI routes not in use |
| rightkit#1193 | DONE | @rightkit/qa ts-register uses `module.registerHooks()`; qa 0.6.2 tarball has it |
| rightkit#1218 | DONE | canary treats a baseline refused at the app-check guard as unevaluable (680139755) |
| rightkit#1219 | DONE | commit 680139755: canary treats read-only trees as removable and refused baselines as unevaluable (#1218, #1219) |
| rightkit#1220 | DONE | canary runs the doctor in place on each live checkout (commit 1a3bdd82e, rightkit#1220) |
| rightkit#1222 | DONE | qa `control` no longer enables daemon test-mode and models honours RIGHTKIT_SUITE_ROOT or real folders; qa 0.10.0 and models 0.5.3 are published. |
| rightkit#1224 | DONE | @rightkit/legal-ui removed the forced light theme (native radios and checkboxes follow the theme); 0.4.1 published |
| rightkit#1226 | STALE | refers to install-app test suites; @rightkit/release test suites were deleted in 0.6.25 |
| rightkit#1228 | DONE | ios-dev.mjs wakes a disconnected device via `devicectl device info details` and re-reads the list |
| rightkit#1232 | STALE | the release test suites (the 19 failing tests) were deleted in 0.6.25. |
| rightkit#1233 | DONE | packages/release/cargo-target.mjs resolves the managed target_directory from cargo metadata |
| rightkit#1236 | DONE | rightkit-qa scenario.rs fails an unknown requirement kind/unselected tier (not a pass); 0.10.0 published |
| rightkit#1240 | DONE | class A launch guard and build-bearing tests moved after class A (commit b13e26966) |
| rightkit#1241 | SUPERSEDED | npm units now publish from the Mac via publish-local (#1471, release 0.6.29/0.6.36); broker-pnpm path moot |
| rightkit#1246 | DONE | CSP present in the 3 fixture tauri.conf.json, M22 in template manifest, empty app-check baseline (1270 backfill) |
| rightkit#1249 | DONE | ci/gate.config.json `fixtures` lists rightkit-qa examples, tauri-shell example-app and spikes, so SC-R4 skips them |
| rightkit#1252 | STALE | `treeSha256` pin digest no longer exists in the release or gate code |
| rightkit#1255 | DONE | gate snapshot seeds its temporary index from the commit tree (aac073dd7, packages/release/snapshot.mjs) |
| rightkit#1256 | STALE | pre-push compile+test hook on a private repo; hooks are banned by contract and the local gate is the route |
| rightkit#1258 | STALE | 11 Windows failures in packages/release tests; that suite was deleted in 0.6.25 |
| rightkit#1260 | DONE | rightkit-child-process 0.5.7 (on crates.io): terminate_tree_bounded reports Exited only when the Job Object has ActiveProcesses==0. |
| rightkit#1262 | STALE | failures in @rightkit/release test suites, which were deleted in 0.6.25 |
| rightkit#1264 | STALE | right-suite-contract test against heardright scripts; the packages/release test suites were deleted in 0.6.25 |
| rightkit#1265 | DONE | gate cache bypass refused/retired (release CHANGELOG line 29); 0.6.36 published |
| rightkit#1269 | DONE | class A tsc reads siblings' committed sources (release CHANGELOG, rightkit#1269) |
| rightkit#1270 | DONE | backfill landed in release 0.6.10 (template M22 CLI contract, empty baseline) |
| rightkit#1271 | DONE | backfill record for gate-contract commits 0cda2ee00, 67ad3c5d4 and 12b1c1b19; the rules are in docs/GATE-CONTRACT.md. |
| rightkit#1272 | DONE | rightkit-child-process 0.5.7 is on crates.io; backfill issue only |
| rightkit#1273 | DONE | rightkit-asr `kws` Zipformer2 keyword spotter (#1273); asr 0.4.0 published |
| rightkit#1274 | DONE | npm units release through the gate in 0.6.10 (commit 603c4f47b) |
| rightkit#1275 | DONE | rightkit-send `Transfer.accepted` (CHANGELOG); 0.3.0 published |
| rightkit#1276 | DONE | rightkit-fsindex 0.6.0 re-list gate age pruning (CHANGELOG cites #1276); published. |
| rightkit#1284 | DONE | every dependency bumped to latest (commit 454e975a1 and follow-ups); a one-off run |
| rightkit#1285 | DONE | one Cargo workspace and lock per crate, components built in parallel (CHANGELOG 208-209) |
| rightkit#1286 | DONE | rightkit-license 0.2.6 imports the bare record so status and native read agree (CHANGELOG cites #1286); published. |
| rightkit#1291 | DONE | @rightkit/release 0.6.11 .rightkit/fixture-files.json declared-fixture skips (CHANGELOG cites #1291). |
| rightkit#1295 | DONE | commit b0fdd9d69: the broker trusts only a published, CI-qualified gate (GATE-CONTRACT) |
| rightkit#1297 | SUPERSEDED | direct publishes are now the sanctioned `publish-local` route (rightkit#1471, 0.6.29/0.6.34); route probe and canary shipped in 0.6.14 |
| rightkit#1304 | DONE | rightkit_qa::signing Evidence in rightkit-qa 0.10.0 (CHANGELOG cites #1304); 0.10.0 is on crates.io. |
| rightkit#1305 | DONE | `right-release deps --cargo-lock --manifest --pin` lock-update door (0.6.14) |
| rightkit#1312 | DONE | decision recorded as GATE-CONTRACT GC-A06 (real-broker witness before pinning) |
| rightkit#1316 | DONE | rightkit-models speech models HeardRight-owned, `retire_speech_models` (f7b52b036); 0.5.3 published |
| rightkit#1321 | SUPERSEDED | GATE-CONTRACT GC-A01 states containers run the gate without the broker; cloud route retired for local publish (#1471) |
| rightkit#1323 | STALE | CI red on main; CI routes are not in use (docs/CI.md, rightkit#1471) |
| rightkit#1325 | DONE | whole-workspace default and RightKit-only per-unit exceptions (`all-units-exceptions`, broker-exception.mjs); release 0.6.36 |
| rightkit#1329 | DONE | gate pnpm pin check with a typed version-mismatch failure (commit 0535ece5b, #1329) |
| rightkit#1330 | DONE | stage-sidecar lists and extracts with System32 `tar.exe` by absolute path (stage-sidecar.mjs); release 0.6.36 |
| rightkit#1331 | DONE | gate materializes declared compile inputs before class B (commit 0535ece5b, release 0.6.16) |
| rightkit#1348 | DONE | rightkit-file-watch reports delete after earlier batch (a19142a2b); 0.4.2 published |
| rightkit#1365 | DONE | `rightkit-host-gate` bin plus `rightkit-linux-setup` (6a5dd6226), release 0.6.17/0.6.18 |
| rightkit#1367 | DONE | commits 6c5a50203 and 6b448a3b6: build ownership per repository, gate stops counting builds; release 0.6.36 |
| rightkit#1368 | SUPERSEDED | broker/gate split redefined by GC-A11 to GC-A15 in 0.6.25 (Class A never in broker) |
| rightkit#1380 | DONE | GATE-CONTRACT rewritten to broker ownership (d3e2e0d45, b1b d) |
| rightkit#1401 | STALE | measured waste in CI qualify-direct and publish; CI is not the publish route since rightkit#1471 |
| rightkit#1409 | DONE | rightkit-asr 0.4.0 (on crates.io) has the Rust kws module; HeardRight deleting vendor/rightkit-asr is heardright's step. |
| rightkit#1413 | SUPERSEDED | CI.md now requires native tests (cf8719b5d); publish is local through the gate (rightkit#1471, GC-A06) |
| rightkit#1443 | DONE | packaging and signing path faults closed in @rightkit/release 0.6.25 (commit c77dfdd23), published |
| rightkit#1450 | SUPERSEDED | build-helper trace check removed from Class A in @rightkit/release 0.6.36 |
| rightkit#1452 | DONE | GC-A11 shipped in 0.6.25, completed in 0.6.34 (carried cargo non-compiling calls run directly) |
| rightkit#1453 | DONE | GC-A12: web-build runs directly and skips type-check-only tsc (CHANGELOG 90-92) |
| rightkit#1454 | DONE | GATE-CONTRACT GC-A08: Rust tests run outside the broker (@rightkit/release CHANGELOG 0.6.25); doctests stay an open decision. |
| rightkit#1455 | DONE | GC-A13: RA-03 no longer refuses unexplained cause verdicts (@rightkit/release changelog) |
| rightkit#1456 | DONE | release 0.6.32 reuses results by full input key (per-run ids normalised, broker compiles reused); published |
| rightkit#1457 | DONE | 24 h `settledLatest` freshness shipped in 0.6.25; publish-date reads fixed there too |
| rightkit#1459 | DONE | scan time limits always fire (0.6.31) and the early-client-exit race is waited for (0.6.25). |
| rightkit#1471 | DONE | `right-release publish-local cargo/npm` shipped in 0.6.29 and 0.6.34; decision recorded |
| rightkit#1472 | DONE | owned packages exempt from provenance (b8b890fa9, release 0.6.29) |
| rightkit#1473 | SUPERSEDED | the build-helper trace check was removed from Class A in 0.6.36 (#1450), so the trace key no longer exists. |
| rightkit#1474 | STALE | deferred blobless-clone idea; CI is no longer the publish route and the gain is small |
| rightkit#1478 | DONE | rightkit-onnx 0.2.7: a universal row serves macos-x64 and the catalog pins ORT 1.30.0 universal2; 0.2.9 is published. |
| rightkit#1486 | DONE | snapshot check accepts pnpm hard links of a committed `file:` package, @rightkit/release 0.6.35 (commit 717200b0b) |
| rightkit#1490 | STALE | one-off snapshot-lease timeout traced to build-helper trace load; that trace check was removed from Class A in 0.6.36 |
| rightkit#1492 | SUPERSEDED | 0.6.29+ lists only what each host builds per target and skips what cannot build (CHANGELOG line 50) |
| rightkit#1493 | DONE | @rightkit/release 0.6.36 (published): publish-local npm exports the whole tree, installs offline and builds workspace deps before packing. |
| rightkit#1494 | SUPERSEDED | eslogger trace removed with the build-helper trace check in @rightkit/release 0.6.36 |
