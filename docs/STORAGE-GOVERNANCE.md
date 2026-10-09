# RightKit storage governance

> Retired 2026-10-09: the Node build broker packages were removed; the Rust broker at `bogusyogi/rustright` (binary `rustright`, alias `rightkit`) owns this. Rows and commands below that name broker, client or core modules describe the removed implementation.

Implemented source changes only. No commit, publication, live config edit, cutover, real-data deletion or Trash move occurred. Existing unrelated checkout edits remain intact.

| Requested item | Changes |
|---|---|
| Managed QA roots | `packages/qa/src/storage.ts`, `workspace.ts`; `crates/rightkit-qa/src/storage.rs`, workspace, harness, suite, CLI & process registry sweep. Shared markers, PID/lease protection, serialized startup retention, managed captures/evidence/scratch, budget accounting & refusal when protected/unknown data prevents fitting budget. Orphan recovery precedes retention & skips live owners. |
| Installer retention | `packages/release/artifact-storage.mjs`, build/seal/upload/update paths & `mirror-root-artifact.mjs`. Outputs resolve under managed app/version directories. Keep newest completed versions plus currently published platform versions. Fresh verified publication updates pointer; dry-run/replayed receipts cannot replace it. Doctor fails repository installer-copy findings; ownership report includes storage findings. |
| Xcode | Removed Node core module (`xcode-storage`); now RustRight. Managed DerivedData/xcresult argument helper, connected-device discovery, recent-use history, version retention & same-volume Trash moves. Discovery/history failures stop pruning. |
| Broker | Volume discovery, host sizing, config migration, ThinState GC & live `STORAGE_GC` RPC. Budget triggers LRU above free floor; final transition rechecks generation, timestamps, activity & grace. Dry-run never queues applying maintenance. Compiler storage remains owned by native sccache LRU. |
| Storage CLI | Removed Node client CLI, dispatch & typed client RPC, and core storage modules; now `rustright storage`. JSON report, per-app/category totals, top offenders, doctor, dry-run/apply, script wrapper & explicit one-off Trash selection. |

Defaults ([QA storage.ts:5](../packages/qa/src/storage.ts#L5), [Rust storage.rs:12](../crates/rightkit-qa/src/storage.rs#L12); broker defaults live in RustRight):

| Policy | Default |
|---|---|
| Managed root | `RIGHTKIT_MANAGED_ROOT`; otherwise `/Volumes/D/.rightkit-managed` on Mac, `D:\.rightkit-managed` on Windows |
| Run path | `<managed>/runs/<app>/<run-id>`; scripts receive `RIGHTKIT_RUN_ROOT` |
| Run retention | Newest 5 OR younger than 24 h; 20 GB/app (20,000,000,000 bytes). Old inactive runs may lose count protection to fit byte cap. Live PID/future lease & young runs always survive. |
| Artifact path/retention | `<managed>/artifacts/<app>/<version>`; `RIGHT_RELEASE_ARTIFACT_DIR`; newest 3 completed versions plus current published versions |
| Publication state | `<managed>/artifacts/<app>/published.json`; unknown state preserves artifacts & CLI reports blocked retention. Establish current platform versions from verified distribution state before initial cleanup. |
| DeviceSupport | Newest 2 OS versions, connected versions & versions observed during managed Xcode preparation/pruning within 30 days |
| Target cache | At most 150 GB (150,000,000,000 bytes); smaller volumes can lower ceiling |
| Compiler cache | Separate native sccache ceiling: `min(150 GiB, 17% volume)` for newly generated config; migration preserves existing hand-tuned compiler fields |
| Broker volume/free floor | Rediscovered mounted capacity; usable capacity 90%, free floor 10%; default GC grace 30 min, existing configured grace preserved |
| Ownership gate | Repository subdirectories over 5 GB outside ignored managed paths; ordinary ignored `.cache`/`.audit` directories remain findings |

Managed run/artifact pruning requires valid ownership metadata. Unknown roots survive. One-off archives, research directories, workspace trash & installers move by rename into containing volume’s `.Trashes/<uid>`; no recursive deletion of those items. Legacy audit/cache/temp trees require app adoption or explicit reviewed relocation; new run retention does not claim ownership over existing unmarked data.

Storage migration, bootstrap and cutover commands for the removed Node broker no longer exist; use the RustRight installer and CLI documented in `bogusyogi/rustright`.

```sh
rightkit storage report --workspace /Volumes/D/claude \
  --external /Volumes/D/.build-home/adrdsouza/tmp \
  --external /Volumes/D/.build-home/adrdsouza/xcode-ios-device-support
rightkit storage doctor --workspace /Volumes/D/claude
rightkit storage prune --dry-run \
  --device-support /Volumes/D/.build-home/adrdsouza/xcode-ios-device-support
rightkit storage prune --apply \
  --device-support /Volumes/D/.build-home/adrdsouza/xcode-ios-device-support

# Optional reviewed one-off item: repeat --trash for exact selected paths.
rightkit storage prune --dry-run --trash /exact/path/old-installer.dmg
rightkit storage prune --apply --trash /exact/path/old-installer.dmg

# Scripts inherit managed output root & temporary-directory variables.
rightkit storage run --app heardright -- bash ios-native/scripts/device-smoke.sh
# Called inside that managed run; emits JSON array for xcodebuild argv.
rightkit storage xcode-args --run-root "$RIGHTKIT_RUN_ROOT"
```

Prune uses live broker RPC for broker caches. `--broker skip` explicitly excludes broker work, useful for temp-only fixtures. Busy/unavailable broker, unresolved run overage or unknown artifact publication state returns failure with JSON diagnostics. Device discovery uses locally verified `xcrun devicectl list devices --json-output -`; a captured snapshot can be supplied via `--devices-json FILE`.

App chats must adopt published QA/release versions containing these APIs after separate publication; consumer dependencies remain registry pinned. Shared Rust/JS entrypoints now choose managed roots; standalone scripts need these edits:

| App | Scripts/output changes |
|---|---|
| HeardRight | `ios-native/scripts/device-smoke.sh`, `device-screenshots.sh`, `device-dogfood-trace.sh`, `device-deeplink-recording-probe.sh`, `ios-dogfood-proof.sh`, `release-preflight.sh`, `appstore-archive.sh`: replace `.audit`/`.cache` output defaults with run subdirectories; consume managed DerivedData/xcresult argv. Route `tauri-app-next/scripts/qa-hub-e2e.mjs`, `qa-mini-clipboard-geometry.mjs` & evaluation-run outputs to run evidence/captures; keep model inputs separate from disposable run output. |
| ScrapeRight | `ios-native/scripts/run-e2e.sh`: set `IOS_E2E_DERIVED_DATA=$RIGHTKIT_RUN_ROOT/DerivedData`, `IOS_E2E_EVIDENCE=$RIGHTKIT_RUN_ROOT/evidence/ios`. `scripts/qa-browser.mjs`, `qa-capture.{mjs,ts}`, `qa-visual-diff.ts`, `dogfood-real.mjs`, `sidecar-smoke/args.mjs`, `benchmark-receipts.mjs`: run-root output. `package.sh:41` writes root DMG; `:100` & `:456` mirror into repo. Change installer path to `$RIGHT_RELEASE_ARTIFACT_DIR`, remove root mirrors; notarization scratch belongs under managed scratch. Doctor reports these exact mirror calls; no app edit made. |
| CodeRight | iOS Xcode callers: replace `.cache/DerivedData` with managed helper paths. `apps/coderight-ios/scripts/build-runtime.sh`: staging output under run scratch, leaving broker-owned compiler targets intact. `apps/coderight-tauri/scripts/mac-dmg.mjs`: write installer to `RIGHT_RELEASE_ARTIFACT_DIR`, remove repo mirrors. Old root DMGs require explicit Trash selection. |
| GenRight | `scripts/qa-browser.ps1`, `qa-browser-stop.ps1`, `qa-shot.mjs`, `qa-functional.mjs`, `qualify-local-image-backend.mjs` & completion-run callers: resolve browser state, screenshots, logs & qualification outputs under same managed run. `.cache/devin-completion-20261006` remains existing data. Preserve downloaded model inputs separately. |
| CutRight | `skills/qa/scripts/qa.mjs`, `qa-shot.mjs`, `qa-functional.mjs` & Rust QA test callers: use managed QA workspace root, child env & scratch API; finish marker only after owned processes stop. Replace arbitrary temp-root allocation with managed scratch. |
| ViewRight/MailRight & other consumers | Upgrade published QA/release pins; standalone QA scripts use `createQaWorkspace`/`createManagedRun` or storage wrapper, captures below run root, installers below `RIGHT_RELEASE_ARTIFACT_DIR`; remove root-copy hooks. |

Validation uses disposable temp directories. No claimed disk-space recovery.

| Check | Total | Passed | Failed | Skipped |
|---|---:|---:|---:|---:|
| Node core + clients, complete packages | 104 | 103 | 1 | 0 |
| Node broker, complete package | 364 | 363 | 1 | 0 |
| Node release, complete package | 767 | 709 | 41 | 17 |
| JS QA Vitest, complete package | 57 | 52 | 5 | 0 |
| Node QA public contract | 1 | 1 | 0 | 0 |
| RightKit Cargo QA, unit/integration/doc tests | 64 | 64 | 0 | 0 |

Focused core/CLI/Xcode + public QA policies: 25/25 passed. Focused release/doctor/CLI/launcher policies: 40 passed, 1 platform skip, 0 failed. Focused broker migration/bootstrap policies: 38/38 passed. Focused QA Vitest: 11/11 passed; QA TypeScript build passed. Full Node failures include Node26 async assertion in client socket tests, denied Unix/TCP binds, pnpm store SQLite access, existing app contract drift & unrelated dirty-source contract failures. Required `pnpm run test:release` & `pnpm run test:qa` wrappers were attempted separately; both fail before test execution because pnpm cannot open its store SQLite database. Full Node suites are not green.

`rightkit cargo test -p rightkit-qa` completed successfully on native Mac through managed broker: 48 unit, 15 integration & 1 doc test; all six new Rust storage policy tests passed. Terminal request `df8fb341-e026-4213-aed0-36852cb703aa`, exit 0, confirmed by `rightkit attach`. Optional browser-feature tests were not enabled. First attempt exposed an outdated workspace-path assertion; fixed before successful rerun.

Lead verification commands:

```sh
TMPDIR=/private/tmp node --test --test-concurrency=1 --test-force-exit \
  packages/release/*.test.mjs packages/qa/public-storage.test.mjs
pnpm --dir packages/qa test
rightkit cargo test -p rightkit-qa
```

Primary check: `git diff --check` passed. Read-only independent safety review informed fixes for owner locks, publication replay, active-run sweeps, count retention & broker grace races.
