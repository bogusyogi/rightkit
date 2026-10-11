# Publication review — 2026-10-11

Snapshot: `f60f116514467e83952f5e5c12a71b34e3341389`; working tree had 526 changed/untracked entries. No network, build, test, pack, install, publication, credential read, or product change ran. One pure read-only `runtimeBudgetCheck` invocation read manifests & READMEs; it did not build, test, qualify, or mutate state. `registry_recorded` means local ledger only; remote availability remains unverified.

## Lifecycle inventory

Implemented means source manifest/VERSION exists. Staged, registry-recorded, & adopted come only from `packages/release/rightkit-versions.json`. Swift `swift.rightkit-swift` URL/version is a local registry-pinned tag record, not an adoption record. `—` means no local entry. Receipt status evaluates each public unit’s expected gate file against current tree identity; stale means former evidence fails current exact-commit/version/path-clean gate, not that former publication was false.

| Ecosystem | Unit | Implemented | Staged | Registry-recorded | Adopted | Receipt | Declared closure |
| --- | --- | --- | --- | --- | --- | --- | --- |
| npm | @rightkit/agent-room-mcp | 0.1.0 | — | — | — | not-required-private | files:0; exports:no; bin:no |
| npm | @rightkit/app-shell | 0.6.1 | 0.6.0 | 0.6.0 | — | stale | files:10; exports:yes; bin:no |
| npm | @rightkit/ax | 0.2.4 | 0.2.4 | 0.2.4 | — | missing | files:7; exports:yes; bin:yes |
| npm | @rightkit/build-control-policy | 0.1.0-phase1 | — | — | — | not-required-private | files:0; exports:no; bin:no |
| npm | @rightkit/doc-render | 0.1.6 | 0.1.6 | 0.1.6 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/git | 0.2.39 | 0.2.39 | 0.2.39 | 0.2.0 | stale | files:5; exports:no; bin:yes |
| npm | @rightkit/hooks | 0.1.2 | 0.1.2 | 0.1.2 | — | stale | files:3; exports:yes; bin:yes |
| npm | @rightkit/i18n | 0.1.0 | 0.1.0 | 0.1.0 | — | stale | files:4; exports:yes; bin:no |
| npm | @rightkit/legal | 0.3.1 | 0.3.1 | 0.3.1 | 0.2.0 | missing | files:2; exports:yes; bin:no |
| npm | @rightkit/legal-ui | 0.2.2 | 0.2.2 | 0.2.2 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/license | 0.2.0 | 0.1.8 | 0.1.8 | 0.1.5 | missing | files:1; exports:yes; bin:no |
| npm | @rightkit/logs | 0.1.4 | 0.1.4 | 0.1.4 | 0.1.3 | missing | files:2; exports:yes; bin:no |
| npm | @rightkit/media-ui | 0.3.2 | 0.3.2 | 0.3.2 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/onboarding | 0.3.2 | 0.3.2 | 0.3.2 | — | stale | files:5; exports:yes; bin:no |
| npm | @rightkit/pdf-ui | 0.1.5 | 0.1.5 | 0.1.5 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/platform-ui | 0.2.2 | 0.2.1 | 0.2.1 | 0.1.0 | stale | files:5; exports:yes; bin:no |
| npm | @rightkit/qa | 0.4.1 | 0.4.1 | 0.4.1 | 0.1.0 | stale | files:4; exports:yes; bin:no |
| npm | @rightkit/qa-chrome | 0.1.3 | 0.1.3 | 0.1.3 | — | stale | files:4; exports:yes; bin:yes |
| npm | @rightkit/release | 0.2.153 | 0.2.153 | 0.2.151 | 0.2.47 | stale | files:7; exports:no; bin:yes |
| npm | @rightkit/settings-ui | 0.3.4 | 0.3.4 | 0.3.4 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/shell | 0.1.0 | 0.1.0 | 0.1.0 | — | stale | files:2; exports:yes; bin:no |
| npm | @rightkit/tauri | 0.2.0 | 0.2.0 | 0.2.0 | 0.1.0 | stale | files:3; exports:yes; bin:no |
| npm | @rightkit/theme | 0.2.3 | 0.2.3 | 0.2.3 | — | stale | files:6; exports:yes; bin:yes |
| npm | @rightkit/update | 0.2.0 | — | — | — | missing | files:6; exports:yes; bin:yes |
| npm | @rightkit/updates | 0.2.5 | 0.2.5 | 0.2.5 | 0.2.3 | stale | files:2; exports:yes; bin:no |
| cargo | agent-room-core | 0.3.1 | 0.3.0 | — | — | not-required-private | include:no |
| cargo | rightkit-agent-tools | 0.1.2 | — | — | — | not-required-private | include:no |
| cargo | rightkit-align | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-asr | 0.2.0 | 0.1.0 | 0.0.2 | — | stale | include:yes |
| cargo | rightkit-audio | 0.2.1 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-automation | 0.3.0 | 0.1.0 | — | — | not-required-private | include:no |
| cargo | rightkit-browser | 0.2.4 | 0.2.4 | 0.2.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-cache | 0.2.0 | 0.1.0 | 0.1.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-channel-memory | 0.1.0 | — | — | — | not-required-private | include:no |
| cargo | rightkit-compose | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-contracts | 0.1.4 | 0.1.2 | — | — | not-required-private | include:no |
| cargo | rightkit-control | 0.2.0 | 0.2.0 | 0.1.11 | — | stale | include:yes |
| cargo | rightkit-crash | 0.2.1 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-daemon | 0.1.3 | — | — | — | not-required-private | include:no |
| cargo | rightkit-db | 0.4.0 | 0.3.2 | 0.3.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-embed | 0.3.2 | 0.3.2 | 0.3.1 | — | stale | include:yes |
| cargo | rightkit-framed-sidecar | 0.2.0 | 0.1.0 | 0.1.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-fs | 0.3.4 | 0.3.3 | 0.3.3 | 0.1.0 | stale | include:yes |
| cargo | rightkit-fsindex | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-hotkeys | 0.1.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-http | 0.3.0 | 0.2.5 | 0.2.4 | — | stale | include:yes |
| cargo | rightkit-jobs | 0.2.8 | 0.2.7 | 0.2.6 | — | stale | include:yes |
| cargo | rightkit-license | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.2 | missing | include:yes |
| cargo | rightkit-llm | 0.2.0 | 0.2.0 | — | — | missing | include:yes |
| cargo | rightkit-logs | 0.2.2 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-mcp | 0.5.3 | 0.5.3 | 0.5.2 | — | stale | include:yes |
| cargo | rightkit-media | 0.2.6 | 0.2.5 | 0.2.4 | 0.1.0 | stale | include:yes |
| cargo | rightkit-models | 0.2.9 | 0.2.8 | 0.2.7 | — | stale | include:yes |
| cargo | rightkit-oauth | 0.3.1 | 0.3.1 | 0.3.0 | — | stale | include:yes |
| cargo | rightkit-ocr | 0.2.1 | 0.2.0 | 0.1.2 | — | stale | include:yes |
| cargo | rightkit-ort | 0.2.3 | 0.2.2 | 0.2.2 | — | stale | include:yes |
| cargo | rightkit-overlay | 0.1.1 | 0.1.1 | — | — | missing | include:yes |
| cargo | rightkit-ownership | 0.2.1 | 0.2.1 | 0.2.1 | — | stale | include:yes |
| cargo | rightkit-pdf | 0.2.1 | 0.2.0 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-playback | 0.2.0 | 0.1.2 | 0.1.1 | 0.1.0 | stale | include:yes |
| cargo | rightkit-process | 0.3.4 | 0.3.3 | 0.3.3 | 0.1.0 | stale | include:yes |
| cargo | rightkit-qa | 0.2.16 | 0.2.15 | 0.2.14 | — | stale | include:yes |
| cargo | rightkit-qts | 0.2.6 | 0.2.5 | 0.2.4 | — | stale | include:yes |
| cargo | rightkit-qts-ggml | 0.1.2 | 0.1.2 | 0.1.1 | — | stale | include:yes |
| cargo | rightkit-qts-ggml-sys | 0.1.2 | 0.1.2 | 0.1.1 | — | stale | include:yes |
| cargo | rightkit-record | 0.2.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-review | 0.2.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-sandbox | 0.3.4 | 0.3.4 | 0.3.3 | — | stale | include:yes |
| cargo | rightkit-scenario | 0.1.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-search | 0.2.0 | 0.2.0 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-secrets | 0.1.3 | 0.1.3 | 0.1.2 | — | stale | include:yes |
| cargo | rightkit-send | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-service | 0.1.5 | 0.1.4 | 0.1.3 | — | stale | include:yes |
| cargo | rightkit-settings | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-shell | 0.3.0 | 0.2.0 | 0.1.4 | — | stale | include:yes |
| cargo | rightkit-speech-client | 0.3.3 | 0.3.3 | 0.3.2 | — | stale | include:yes |
| cargo | rightkit-sync | 0.1.1 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-tauri | 0.1.3 | 0.1.2 | 0.1.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-transcript | 0.3.0 | 0.2.0 | 0.2.0 | — | stale | include:yes |
| cargo | rightkit-tts | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-vad | 0.1.0 | 0.0.3 | 0.0.2 | — | stale | include:yes |
| cargo | rightkit-watch | 0.3.3 | 0.3.2 | 0.3.1 | — | stale | include:yes |
| swift | RightKitSwift | 0.6.0 | — | 0.4.0 | — | stale | libraries:15 |
| swift | RightKitCloudKitHelper | — | — | — | — | not-applicable | executables:1 |

Detailed machine-readable rows, receipt identity reasons, closure declarations, & source paths: [publication-inventory.json](publication-inventory.json).

## Receipts, identity & portability

Gate schema is `1` in `packages/release/publication-gate.mjs`. A public npm/Cargo/Swift release needs evidence for exact current 40-hex `HEAD`, clean unit path, same name/version, successful host receipt(s), & a 64-hex artifact hash. Of 84 units, 71 are public release units, 12 private units, & one internal Swift helper. Current snapshot has **0 current public-unit gate receipts**: 59 stale (18 npm + 40 Cargo + Swift) & 12 missing (5 npm + 7 Cargo). Current path dirtiness independently blocks app-shell & license; exact-HEAD mismatch blocks each existing receipt.

Declared npm `files`/exports/bin & Cargo `include` describe intended archive closure, but no package archive or fresh install was made here, so standalone-install portability is **unverified for every release unit**. Internal `workspace:*` npm & Cargo `path` edges are source-package wiring; they are not app-consumer violations by themselves. RightKitCloudKitHelper’s `.package(path: "../RightKitSwift")` is an internal helper edge, recorded separately; it is neither a tagged release nor an app adoption. App violations require an app manifest with a local/file/link/path shared dependency; this review did not infer one from internal package wiring.

## Runtime-budget enforcement

`docs/RUNTIME-BUDGETS.md` requires declaration of resident memory ceiling, CPU, disk operations, measured run; repeatable bench/test; runtime exposure; publish gate; & scale-consumer record. `runtime-budgets.mjs` only inspects README text for exact heading plus resident/CPU figures & one measured line; it does not execute/verify bench, disk-op or ceiling figure, runtime `status()`, or installed scale evidence. `publish-cargo.mjs` invokes this static check before cargo publish, so README failure blocks real cargo publication.

Static invocation of `runtimeBudgetCheck` returned:

| Class crate | Result | Reason |
| --- | --- | --- |
| rightkit-fsindex | fail | no `## Runtime budget` section |
| rightkit-watch | fail | no `## Runtime budget` section |
| rightkit-jobs | fail | no `## Runtime budget` section |
| rightkit-service | fail | no `## Runtime budget` section |
| rightkit-daemon | fail | no `## Runtime budget` section |
| rightkit-cache | fail | no `## Runtime budget` section |
| rightkit-sync | fail | no `## Runtime budget` section |
| rightkit-logs | fail | no `## Runtime budget` section |
| rightkit-shell | fail | no `## Runtime budget` section |
| rightkit-qa | fail | no `## Runtime budget` section |
| rightkit-send | fail | no `## Runtime budget` section |

`rightkit-fsindex` has measured resident-memory prose & `status()` metrics, but no required heading; remaining ten lack a Runtime budget section. Thus docs’ broader rule is not enforced or demonstrated by current source; no claim of runtime compliance follows from README comments or bench references.


