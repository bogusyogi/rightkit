# Decisions, mandates and relayed answers (addendum to DECISIONS.md)

Written 2026-11-03 so nothing lives only in chat. Source: the cloud session `session_015S2ZYoFiiRAYE5gG7hFQxD`.

## Standing mandates (Adrian)

- Goal: publish every RightKit crate and npm package; where publishing is blocked, port the apps onto what is already published. No qualifier like "only the mandatory ones": the asset crates are published without qualification if needed.
- Port commission (Adrian, 2026-11-01): every capability in every app moves into RightKit packages; each app keeps only a thin shell (<=2%), declared exceptions, recipes, and journey/e2e tests. Master plan = 86 new packages (64 crates, 16 npm, 6 Swift) in `plan-86-packages.txt`; per-app mapping tables in `per-app/`; the real document is the plan plus those tables (not PORT-MATRIX).
- App repos receive results on `main`. Per-app port work goes to the desktop agents (one per repo; HeardRight and Pulse already running; CodeRight can continue in cloud).
- Gate-change rules (rkn): only `[profile.release]` with `panic = "unwind"`; no dev/debug build selection; no test-mode features (tests isolate by injection); no `debug_assertions` as a dev/release switch (QA code sits behind `qa-native`); three-OS checks; OS code cfg-gated.
- Conduct: prose minimal; "&" for "and"; no ending caveats; the ask is the approval, never stop for extra approval. Adrian ended the overnight orchestrator delegation on 2026-11-03: every approval request goes to Adrian.
- Adrian's stop order (2026-11-03 ~08:40 UTC): stop, hand over to the desktop agent, push everything even red, no more CI.

## Answers given to the orchestrator (by topic; the orchestrator's own 1-21 numbering is not preserved here, and my reconstruction may be incomplete)

- `diagnostics.ts` has no configure function: use an interim Vite alias; ask for `configureDiagnostics` in document-core 0.1.x.
- Renames confirmed: `hostIsPreferred`, `isHostAppOrigin`, `HostError`/`parseHostError`, `HostDocxToolbar`.
- `wf_port` is not workflow/code-audit: it becomes the new crate `rightkit-dispatch-validator`.
- The evidence-contracts diffs are generalisations: `set_state_identity` is needed, `CanonicalAnchorParams` stays, serde renames are preserved.
- 17 bench moves happen after wave 1 (extract `rightkit-bench`, `pdf_bench`).
- onnx rc.15 is already in source (rightkit-onnx 0.2.9).
- MailRight: the lock file on origin is fine; add a minimatch override to `^11.0.1`; the ledger dependents are `rightkit-models 0.5.3`; `approvedBy` needs Adrian.
- The pre-commit hook is retired: use `right-release hooks install`; the allowlist is advisory unless protected trust.
- Credentials: the `MemoryBackend` was `cfg(any(test, debug_assertions))`; fixed with a non-default `memory-backend` feature (dev-dependencies only), not a separate crate. Released in credentials 0.4.4.
- No memory store in release; e2e needs a real store or a keyless fake; the child-process injection question is open with rkn (rightkit#1367).
- `rightkit-document-workspace` `markdown_index` fixed (0.1.2/0.1.3).
- qa-native `debug_assertions` defect: rightkit-qa 0.10.2 and tauri-shell 0.7.1+ fixed.
- Web QA define spec filed as rightkit#1480: `__RIGHTKIT_QA__` from `RIGHTKIT_BUILD_MODE=dev` via `@rightkit/config` `rightkitQaDefine()`, release forbid-scan of the marker, retire `VITE_*_QA` and `__RIGHTKIT_MOCK__`.
- Embed 0.6.2, asr 0.4.0 (KWS patch, rightkit#1273, Sherpa removed from RightKit) are out.


## Open decisions / items that need Adrian

- `approvedBy` for the MailRight ledger.
- Child-process injection for e2e (rightkit#1367, with rkn).
- rightkit#1493: `publish-local npm` prepack runs `tsc` without node_modules. Not needed for the four stuck packages now that CI publishes npm (qa 0.6.2 went out via publish-direct), but still needed for the Keychain route.
- Whether to keep the hourly "Fleet PR sweep" routine (trig_01SzCpWPvwRWfAca8iNXso7F); it is not part of this work.
- Cleanup that needs a human terminal: `/tmp/rk-bht-*`, `/tmp/rk-admission-*` in the cloud box, and `/home/user/rightkit-branch-cleanup.sh` (branch deletes the proxy blocked).

## Coordination with rkn (via the orchestrator)

- rkn froze pushes to rightkit main during qualify windows, and each push to main voids in-flight publish dispatches. With the orchestrator gone, Adrian arbitrates.
- rkn asked for: KWS patch applied (done), asr 0.4.0 (done), version report to the HRwin chat (done).

## App commits made without verification (never compiled; CI history is July)

heardright 3d183003..51cad537, genright db8446c..78cdafc, CutRight 9a54902..bf83904, pulse 0fe5beea/918878b9, mailright 7c28ce3 (fixes `.env.test.example`). First job per app on the desktop: install the published crates, build, fix.

## Known product-side asks logged for RightKit (tasks #13, #14; files in `handoff/`)

dictation-core `app_paths` injection and test-mode removal, hotkeys `Serialize`, overlay `PillEvent`, sync scheduler, fs `known_folder`, license gaps, mesh `KNOWN_CAPS` agent-files, app-manager home provider, bench extraction (`pdf_bench`), brief.rs/pipeline command layer, spend/job-queue/models/review/tts gaps, dictation-ui/library-ui/media-gen-ui export maps, `@rightkit/docx-editor-react` patched build (the patch is source-only and will not apply to the npm dist), about 24 crates still using `debug_assertions` as a switch.
