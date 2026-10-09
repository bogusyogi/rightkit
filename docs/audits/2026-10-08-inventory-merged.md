# RightKit merged inventory (Haiku set + Luna set), verified

Date: 2026-10-08. Scope: `/Volumes/D/claude/rightkit` (50 crates, 29 npm packages, Swift package + helper, ownership-approval, contracts, scripts, templates). Read-only: the only file written by this merge is this one. No commits.

Inputs merged: Haiku set = `2026-10-08-deps-batch1..4.md` (47 crates, A-F with measured dependency trees), `-deps-npm.md` (22 public npm packages), `-inventory-haiku-crosscutting.md` (7 private packages, Swift, ownership-approval, contracts, scripts, cross-cutting G-K). Luna set = `-inventory-luna-crates.md` (50 crates, manifest-level, no counts) and `-inventory-luna-npm-swift-crosscutting.md` (28 npm packages, 14 Swift targets, self-declared incomplete). Spec: `scratchpad/inventory-spec.md`.

## 0. How to read this document

**Evidence labels.** Class (who found it): AGREED / HAIKU-ONLY / LUNA-ONLY / CONFLICT. Verification (my own checking against code, manifests, `rightkit cargo tree`, the live npm registry and crates.io): VERIFIED / REFUTED / UNVERIFIABLE, each with file:line. "(Windows lead)" = facts supplied by the Windows lead from Cargo.toml/Cargo.lock and marked VERIFIED (Windows lead) as instructed.

**What I measured myself (2026-10-08).** `rightkit cargo tree -p <crate> -e normal,build --prefix depth --no-dedupe --offline` for all 50 crates, default features and `--no-default-features`, host target (macOS). "excl" = registry crates reachable only through that direct dependency (computed from the depth tree; Haiku's "crates added" uses the same idea, small differences come from my dedupe of shared proc-macro crates). Windows-only dependencies cannot be measured (broker refuses `x86_64-pc-windows-msvc`); they are labelled "estimated" or "not measured". The workspace now loads (align/compose/contracts have manifests), which Haiku's batches could not rely on; Haiku used a scratch copy with the same lock, and its numbers reproduce within 0-4 crates (vad 47/10 and qts 6 exactly).

**Other checks.** Systematic unused-dependency scan of every `[dependencies]`, `[build-dependencies]` and `[dev-dependencies]` entry of all 50 crates (identifier search in src/, build.rs, tests/, examples/, benches/), `Cargo.lock` parse (815 packages, 77 multi-version names, 176 entries: reproduces Haiku b4), `pnpm-lock.yaml` parse (539 entries, 471 names, 43 multi-version; doc-render closure 236 reproduced), manifest scan of the 14 app repos for `rightkit-*` and `@rightkit/*` consumers, `md5`/`diff` for claimed copies, and live reads of crates.io (50 crate names) and the npm registry (6 packages) for publication state.

**Known limits.** No Windows build, no `cargo build/test`, no Swift build, no RustSec/advisory data (not available, so unmaintained-crate status is NOT assessed anywhere), no Node tests run. Feature-trim findings are verified statically; a build is needed before removal.

## 1. Coverage check

Legend: **both** = in both inventories; **Haiku only**; **Luna only**; **neither**. Depth differs: Haiku has use sites, verdicts and measured trees per dependency; Luna has manifest identity/API/consumers and flags a handful of candidate-unused dependencies, and states it could not measure counts (its `--locked` run refused to update the lock).

### 1.1 Crates (50 directories under `crates/`)

Summary: both = 47, Luna only = 3 (`rightkit-align`, `rightkit-compose`, `rightkit-contracts`: created during the audit, after Haiku's batches ran against a scratch workspace), Haiku only = 0, neither = 0. Luna counted 50 manifests; Haiku's batches cover 47.

| # | Crate | Haiku | Luna | Status |
|---|---|---|---|---|
| 1 | `agent-room-core` | batch 1 | rows A-F, no counts | both |
| 2 | `rightkit-agent-tools` | batch 1 | rows A-F, no counts | both |
| 3 | `rightkit-align` | no (new crate) | rows A-F, no counts | Luna only -> inventoried here (section 2.A entry 3) |
| 4 | `rightkit-asr` | batch 1 | rows A-F, no counts | both |
| 5 | `rightkit-audio` | batch 1 | rows A-F, no counts | both |
| 6 | `rightkit-automation` | batch 1 | rows A-F, no counts | both |
| 7 | `rightkit-browser` | batch 1 | rows A-F, no counts | both |
| 8 | `rightkit-cache` | batch 1 | rows A-F, no counts | both |
| 9 | `rightkit-compose` | no (new crate) | rows A-F, no counts | Luna only -> inventoried here (section 2.A entry 9) |
| 10 | `rightkit-computer` | batch 1 | rows A-F, no counts | both |
| 11 | `rightkit-contracts` | no (new crate) | rows A-F, no counts | Luna only -> inventoried here (section 2.A entry 11) |
| 12 | `rightkit-control` | batch 1 | rows A-F, no counts | both |
| 13 | `rightkit-crash` | batch 1 | rows A-F, no counts | both |
| 14 | `rightkit-db` | batch 1 | rows A-F, no counts | both |
| 15 | `rightkit-embed` | batch 1 | rows A-F, no counts | both |
| 16 | `rightkit-framed-sidecar` | batch 2 | rows A-F, no counts | both |
| 17 | `rightkit-fs` | batch 2 | rows A-F, no counts | both |
| 18 | `rightkit-hotkeys` | batch 2 | rows A-F, no counts | both |
| 19 | `rightkit-http` | batch 2 | rows A-F, no counts | both |
| 20 | `rightkit-jobs` | batch 2 | rows A-F, no counts | both |
| 21 | `rightkit-license` | batch 2 | rows A-F, no counts | both |
| 22 | `rightkit-llm` | batch 2 | rows A-F, no counts | both |
| 23 | `rightkit-logs` | batch 2 | rows A-F, no counts | both |
| 24 | `rightkit-mcp` | batch 2 | rows A-F, no counts | both |
| 25 | `rightkit-media` | batch 2 | rows A-F, no counts | both |
| 26 | `rightkit-models` | batch 2 | rows A-F, no counts | both |
| 27 | `rightkit-oauth` | batch 2 | rows A-F, no counts | both |
| 28 | `rightkit-ocr` | batch 3 | rows A-F, no counts | both |
| 29 | `rightkit-ort` | batch 3 | rows A-F, no counts | both |
| 30 | `rightkit-overlay` | batch 3 | rows A-F, no counts | both |
| 31 | `rightkit-pdf` | batch 3 | rows A-F, no counts | both |
| 32 | `rightkit-playback` | batch 3 | rows A-F, no counts | both |
| 33 | `rightkit-process` | batch 3 | rows A-F, no counts | both |
| 34 | `rightkit-qa` | batch 3 | rows A-F, no counts | both |
| 35 | `rightkit-qts-ggml-sys` | batch 3 | rows A-F, no counts | both |
| 36 | `rightkit-qts-ggml` | batch 3 | rows A-F, no counts | both |
| 37 | `rightkit-qts` | batch 3 | rows A-F, no counts | both |
| 38 | `rightkit-sandbox` | batch 3 | rows A-F, no counts | both |
| 39 | `rightkit-search` | batch 3 | rows A-F, no counts | both |
| 40 | `rightkit-secrets` | batch 4 | rows A-F, no counts | both |
| 41 | `rightkit-service` | batch 4 | rows A-F, no counts | both |
| 42 | `rightkit-settings` | batch 4 | rows A-F, no counts | both |
| 43 | `rightkit-shell` | batch 4 | rows A-F, no counts | both |
| 44 | `rightkit-speech-client` | batch 4 | rows A-F, no counts | both |
| 45 | `rightkit-sync` | batch 4 | rows A-F, no counts | both |
| 46 | `rightkit-tauri` | batch 4 | rows A-F, no counts | both |
| 47 | `rightkit-transcript` | batch 4 | rows A-F, no counts | both |
| 48 | `rightkit-tts` | batch 4 | rows A-F, no counts | both |
| 49 | `rightkit-vad` | batch 4 | rows A-F, no counts | both |
| 50 | `rightkit-watch` | batch 4 | rows A-F, no counts | both |

### 1.2 npm packages (29 directories under `packages/`)

Summary: both = 28, Haiku only = 1 (`updates`), Luna only = 0, neither = 0. Haiku's 22-package public audit plus its crosscut for 7 private packages = 29; Luna's table has 28 rows (no `updates`) while its text says 28 npm packages.

| # | Package | Haiku | Luna | Status |
|---|---|---|---|---|
| 1 | `@rightkit/agent-room-mcp` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 2 | `@rightkit/app-shell` | deps-npm | yes (name/version/exports/consumers only) | both |
| 3 | `@rightkit/ax` | deps-npm | yes (name/version/exports/consumers only) | both |
| 4 | `@rightkit/build-control-broker` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 5 | `@rightkit/build-control-clients` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 6 | `@rightkit/build-control-core` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 7 | `@rightkit/build-control-phase0` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 8 | `@rightkit/build-control-policy` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 9 | `@rightkit/build-control-staging` | crosscut section 1 | yes (name/version/exports/consumers only) | both |
| 10 | `@rightkit/doc-render` | deps-npm | yes (name/version/exports/consumers only) | both |
| 11 | `@rightkit/git` | deps-npm | yes (name/version/exports/consumers only) | both |
| 12 | `@rightkit/hooks` | deps-npm | yes (name/version/exports/consumers only) | both |
| 13 | `@rightkit/i18n` | deps-npm | yes (name/version/exports/consumers only) | both |
| 14 | `@rightkit/legal` | deps-npm | yes (name/version/exports/consumers only) | both |
| 15 | `@rightkit/legal-ui` | deps-npm | yes (name/version/exports/consumers only) | both |
| 16 | `@rightkit/license` | deps-npm | yes (name/version/exports/consumers only) | both |
| 17 | `@rightkit/logs` | deps-npm | yes (name/version/exports/consumers only) | both |
| 18 | `@rightkit/media-ui` | deps-npm | yes (name/version/exports/consumers only) | both |
| 19 | `@rightkit/onboarding` | deps-npm | yes (name/version/exports/consumers only) | both |
| 20 | `@rightkit/pdf-ui` | deps-npm | yes (name/version/exports/consumers only) | both |
| 21 | `@rightkit/platform-ui` | deps-npm | yes (name/version/exports/consumers only) | both |
| 22 | `@rightkit/qa` | deps-npm | yes (name/version/exports/consumers only) | both |
| 23 | `@rightkit/qa-chrome` | deps-npm | yes (name/version/exports/consumers only) | both |
| 24 | `@rightkit/release` | deps-npm | yes (name/version/exports/consumers only) | both |
| 25 | `@rightkit/settings-ui` | deps-npm | yes (name/version/exports/consumers only) | both |
| 26 | `@rightkit/shell` | deps-npm | yes (name/version/exports/consumers only) | both |
| 27 | `@rightkit/tauri` | deps-npm | yes (name/version/exports/consumers only) | both |
| 28 | `@rightkit/theme` | deps-npm | yes (name/version/exports/consumers only) | both |
| 29 | `@rightkit/updates` | deps-npm | no (table omits it) | Haiku only |

### 1.3 Everything else

| Item | Haiku | Luna | Status |
|---|---|---|---|
| `swift/RightKitSwift` 14 library targets | crosscut 2.1 (LOC, imports, verdicts) | table (responsibility, internal deps, app evidence) | both |
| `swift/RightKitCloudKitHelper` | crosscut 2.2 | no | Haiku only |
| `swift/ios-app-template`, `swift/scripts`, `swift/contracts`, `swift/scraperight-ios-spike` | crosscut 2.3 (listed) | no | Haiku only |
| `ownership-approval/` | crosscut 3 (file table, findings) | summary bullets | both |
| `contracts/` (6 schemas) | crosscut 4 | summary bullet | both |
| `scripts/` (generate-index.mjs, install-right-ctl.sh, run-ui-tests.sh) | crosscut 5 | bullet | both |
| `desktop-app-template/`, `spikes/`, `characterization/`, `infrastructure/`, `release-evidence/`, `legal/`, `docs/`, `tasks/` | no | no | **neither** -> inventoried in 2.D |
| `crates/rightkit-shell/example-app`, `crates/rightkit-control/examples/control-test-app` | mentioned as consumers only | no | **neither** -> 2.D |
| Workspace manifests (Cargo.toml, Cargo.lock, pnpm-workspace.yaml, pnpm-lock.yaml, package.json) | b4 section J | section J (partial) | both |

Items covered by neither inventory were inventoried by me in 2.D (desktop-app-template, spikes, characterization, infrastructure, nested example apps); items only one inventory covered were re-checked against code (2.C Swift helper, 2.B `updates`); the three new crates use Luna's rows plus my own tree measurements and source reads (their entries in 2.A carry full dependency tables).

## 2. Merged per-crate and per-package tables

### 2.A Per-crate (50)

Dependency-table legend. `kind`: normal / build / dev, `/cfg(...)` target gate, `/opt` optional. `use sites`: first two `file:line` hits of the dependency identifier in `src/` (build.rs for build deps; dev deps show a test hit). `measured`: `sub N / excl M` = registry crates in the dependency's subtree / reachable only through it, host default-feature tree; "off by default" = optional and not enabled; "not measured (non-host target)" = Windows-only. Verdict values: NEEDED, OPTIONAL (feature-gate it, named), REPLACEABLE (by named alternative), TRIM (features or defaults), UNUSED. Tag `[class.verification]`: AG/HO/LO/CF (see section 3) then V/R/U; **ND** = not disputed by either inventory and not flagged by my scan (NEEDED by default; the use sites shown are the evidence). Where the two inventories differ on a fact (consumers, publish state, counts) the entry says which one I took and why.

### 2.1 `agent-room-core`  (coverage: both)

- **A. Identity**: source `0.3.0`; publish=false; versions-json registry `-` / staged `0.3.0`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS, Windows, Linux (shell-outs: open, xdg-open, explorer.exe, taskkill.exe, powershell.exe, icacls.exe).
- **B. Responsibility / API**: Shared agent-room state machine (append-only SQLite store with SHA-256 receipts, protocol turns, auth records) plus a local axum HTTP/SSE host and `room` CLI. API: `auth`, `protocol`, `runtime`, `server::serve`, `store` (lib.rs:3-7).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 80; `--no-default-features` 80.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `axum` 0.8 | normal | src/server.rs:3, src/server.rs:4 | OPTIONAL (feature http) | sub 49 / excl 29 | server.rs:3-8; 29 exclusive crates [HO·V] |
| `rand` 0.9 | normal | src/auth.rs:2, src/auth.rs:205 | REPLACEABLE (getrandom) | sub 8 / excl 6 | auth.rs:205, bin/room.rs:718 only `fill_bytes`; 6 exclusive; rand 0.9 still pulled by tokenizers/tungstenite [HO·V] |
| `rusqlite` 0.40.1 +bundled | normal | src/store.rs:2, src/store.rs:31 | NEEDED | sub 14 / excl 13 |  [ND] |
| `serde` 1 +derive | normal | src/auth.rs:3, src/auth.rs:13 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1 | normal | src/bin/room.rs:8, src/bin/room.rs:177 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/auth.rs:4, src/bin/room.rs:9 | NEEDED | sub 10 / excl 8 | 0.10; receipts/auth [AG] |
| `tokio` 1 no-default +rt-multi-thread,macros,net,sync,time | normal | src/bin/room.rs:69, src/bin/room.rs:305 | TRIM (rt-multi-thread bin-only) | sub 10 / excl 0 | `#[tokio::main]` in bin/room.rs:69 is the only multi-thread need [HO·V(partial)] |
| `tokio-stream` 0.1 +sync | normal | src/server.rs:18, src/server.rs:19 | OPTIONAL (with http) | sub 15 / excl 3 | server.rs:18-19; 3 exclusive [HO·V] |
| `tempfile` 3 | dev | src/bin/room.rs:1436 | dev only | n/a (dev) |   |
| `tower` 0.5 +util | dev | tests/p0_server.rs:9 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F22 [HO, VERIFIED, measured]: library and HTTP host in one crate; axum + tokio-stream (32 exclusive crates) reach every consumer.
  - F22 / F35: Windows ACL via `icacls.exe` (runtime.rs:57,82) duplicates rightkit-fs owner-only DACL (fs/private.rs:5,180); atomic writes are bare `fs::rename` (store.rs:234,316,386).
  - F56: `publish = false` (Cargo.toml:7) yet listed in `stagedCargo`; not on crates.io. Zero crate consumers (its client is the npm package agent-room-mcp over HTTP).

### 2.2 `rightkit-agent-tools`  (coverage: both)

- **A. Identity**: source `0.1.2`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: Desktop backend macOS+Windows only; web/control backends portable.
- **B. Responsibility / API**: One typed tool interface for agents over automation, browser, speech and each app engine (via rightkit-service) plus the `right-ctl <app> <command>` CLI. API: `backends::{control,desktop,speech,web}`, `exposure`, `mcp` (feature), `control_toolbox`, `shared`, `CTL_CALLER` (lib.rs).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 124; `--no-default-features` 12.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-service` 0.1.0 | normal | src/backends/control.rs:9, src/backends/speech.rs:7 | NEEDED | sub 12 / excl 0 |  [ND] |
| `rightkit-automation` 0.1.0 | normal/opt | src/backends/desktop.rs:9, src/backends/desktop.rs:281 | NEEDED | sub 12 / excl 0 |  [ND] |
| `rightkit-browser` 0.2.2 | normal/opt | src/backends/web.rs:11, src/bin/right-ctl.rs:105 | OPTIONAL (default-on feature) | sub 124 / excl 104 | 104 exclusive; right-ctl needs it (install-right-ctl.sh:10); no lib consumer [HO·V] |
| `rightkit-speech-client` 0.3.0 | normal/opt | src/backends/speech.rs:8 | NEEDED | sub 12 / excl 0 |  [ND] |
| `rightkit-mcp` 0.4.0 | normal/opt | src/mcp.rs:7 | OPTIONAL (feature mcp) | off by default | already gated [AG] |
| `serde` workspace | normal | src/spec.rs:4, src/spec.rs:11 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` workspace | normal | src/backends/control.rs:10, src/backends/desktop.rs:13 | NEEDED | sub 5 / excl 0 |  [ND] |
| `tokio` 1 +rt-multi-thread,time | normal/opt | src/backends/web.rs:16, src/backends/web.rs:30 | NEEDED | sub 16 / excl 0 |  [ND] |
| `tempfile` 3 | dev | tests/e2e.rs:91 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F23: `browser` is a default feature (Cargo.toml:12) = 104 exclusive crates, but scripts/install-right-ctl.sh:10 builds the bin with defaults and right-ctl needs the web tools; there is no library consumer. The bin IS cfg-gated (right-ctl.rs:65,78,89,103): Haiku b1 "bin fails to compile without browser" is REFUTED.
  - Description names `rightkit-control` (Cargo.toml:6, lib.rs:8) but there is no such dependency (doc drift, VERIFIED).

### 2.3 `rightkit-align`  (coverage: Luna only (re-inventoried here))

- **A. Identity**: source `0.0.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable.
- **B. Responsibility / API**: Product-neutral script-to-ASR word alignment (pronunciation normalisation, edit-distance alignment with lexical/timing confidence, ambiguity rejection, control-name coverage). New content-pipeline crate (plan RK4).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 14; `--no-default-features` 14.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-transcript` path | normal | src/lib.rs:28, src/lib.rs:142 | NEEDED | sub 14 / excl 7 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/lib.rs:26, src/lib.rs:37 | NEEDED | sub 7 / excl 0 |  [ND] |

- **F. Problems / findings**:
  - Plan-owned (RK4): no consumer yet by design. Haiku crosscut H6 "merge into rightkit-transcript" NOT adopted (F63).

### 2.4 `rightkit-asr`  (coverage: both)

- **A. Identity**: source `0.0.2`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS (CoreML via objc2), Windows/Linux via ONNX Runtime + dynamic whisper.cpp.
- **B. Responsibility / API**: Reconciled Parakeet ONNX (TDT, EOU, Nemotron, unified, Cohere) and Whisper (CoreML / whisper.cpp dynamic) decoding runtime. API: `Parakeet*`, `Whisper*`, `CoreMlParakeet`, `select_engine`, `transcribe_spans`, `longform` (lib.rs:87-118).
- **C. Consumers**: RightKit crates: rightkit-transcript(dev). Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 137; `--no-default-features` 137.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `eyre` 0.6 | normal | - | UNUSED | sub 3 / excl 2 | zero hits src/tests/build.rs/examples; 2 exclusive [AG·V] |
| `half` 2 | normal | src/model_transducer.rs:275, src/model_transducer.rs:295 | REPLACEABLE (rightkit-ort `half` feature) | sub 8 / excl 0 | model_transducer.rs:275,295; 0 crates [HO] |
| `hound` 3.5 | normal | src/audio.rs:3, src/audio.rs:13 | NEEDED | sub 1 / excl 1 | audio.rs:3; overlaps rightkit-audio wav.rs (low) [HO] |
| `ndarray` 0.17 | normal | src/audio.rs:4, src/cohere.rs:32 | NEEDED | sub 7 / excl 0 |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,ndarray,api-24,half | normal | src/error.rs:8, src/error.rs:39 | NEEDED but route via rightkit-ort | sub 54 / excl 0 | Cargo.toml:32; ort-defaults feature adds download-binaries/tls-native/copy-dylibs (:56) [CF·V(b1,b4)] |
| `realfft` 3 | normal | src/audio.rs:58, src/sortformer.rs:26 | NEEDED | sub 9 / excl 5 |  [ND] |
| `serde` 1.0 +derive | normal | src/config.rs:1, src/config.rs:3 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0 | normal | src/error.rs:48, src/error.rs:49 | NEEDED | sub 5 / excl 0 |  [ND] |
| `libloading` 0.9 | normal | src/whisper/cpp.rs:128, src/whisper/cpp.rs:168 | NEEDED | sub 2 / excl 1 |  [ND] |
| `rightkit-vad` path | normal | src/chunked.rs:9 | TRIM (default-features=false) | sub 54 / excl 0 | vad default enables download-binaries; only `Span` used (chunked.rs:9) [HO·V] |
| `tokenizers` 0.22.2 no-default +onig | normal | src/cohere.rs:35, src/decoder.rs:22 | OPTIONAL (Parakeet feature) | sub 76 / excl 54 | 54 exclusive incl. onig_sys C; parakeet_eou.rs:11, decoder.rs:22 [HO·V(measured)] |
| `rightkit-media` 0.2.3 | dev | tests/e2e_whisper.rs:25 | dev only | n/a (dev) |   |
| `rightkit-ort` 0.2.0 | dev | tests/e2e_whisper.rs:26 | dev only | n/a (dev) |   |
| `rightkit-speech-client` 0.3.0 | dev | tests/certify_heardright.rs:18 | dev only | n/a (dev) |   |
| `objc2` 0.6 +exception | normal/cfg(target_os = "macos") | src/parakeet_coreml/decode.rs:18, src/whisper/coreml/bridge.rs:13 | NEEDED | sub 6 / excl 0 |  [ND] |
| `objc2-core-ml` 0.3 | normal/cfg(target_os = "macos") | src/whisper/coreml/bridge.rs:16, src/whisper/coreml/mod.rs:24 | NEEDED | sub 16 / excl 5 |  [ND] |
| `objc2-foundation` 0.3 | normal/cfg(target_os = "macos") | src/whisper/coreml/bridge.rs:20 | NEEDED | sub 11 / excl 0 |  [ND] |

- **F. Problems / findings**:
  - F01 [AG, VERIFIED]: `eyre` declared (Cargo.toml:28), zero references.
  - F07 [CF, Haiku b1+b4 VERIFIED; b3 REFUTED]: default feature `ort-defaults` (Cargo.toml:51,56) enables `ort/download-binaries`, `tls-native`, `copy-dylibs`; `rightkit-vad` is taken with default features (Cargo.toml:37) so the download chain stays even with `--no-default-features --features cpu` (163 lines both ways).
  - F34: direct `ort =2.0.0-rc.13` (Cargo.toml:32); rightkit-ort is only a dev-dependency (Cargo.toml:47-49).
  - Feature candidate [HO, measured]: tokenizers (54 exclusive, vendored onig_sys C) only for Parakeet paths; split design unverified.
  - Consumers: none in apps; only rightkit-transcript as dev-dependency. Plan-owned pipeline crate (RK4 local route). Vendored derivative of parakeet-rs 0.3.5 (UPSTREAM.toml, PATCHES.md).

### 2.5 `rightkit-audio`  (coverage: both)

- **A. Identity**: source `0.1.1`; publishable; versions-json registry `0.1.1` / staged `0.1.1`; crates.io live `0.1.1`; platforms: macOS (CoreAudio), Windows (WASAPI/COM), Linux (cpal).
- **B. Responsibility / API**: Shared audio for Right Suite apps: typed FFmpeg filter graphs, loudness gates, room tone, ducking, WAV I/O, cpal capture with resample/downmix/recovery. API: bus, capture, chain, ducking, filter, lifecycle, measure, preset, profile, room_tone, wav (lib.rs:15-37).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 64; `--no-default-features` 41.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-media` 0.2.3 | normal | src/error.rs:3, src/measure.rs:13 | NEEDED | sub 24 / excl 11 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/chain.rs:7, src/chain.rs:12 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/error.rs:22, src/preset.rs:62 | NEEDED | sub 5 / excl 0 |  [ND] |
| `thiserror` 2 | normal | src/error.rs:5, src/error.rs:7 | NEEDED | sub 6 / excl 3 |  [ND] |
| `hound` 3 | normal | src/wav.rs:8, src/wav.rs:21 | NEEDED | sub 1 / excl 1 |  [ND] |
| `cpal` 0.18.2 | normal/opt | src/capture/device.rs:9, src/capture/device.rs:10 | OPTIONAL (feature capture, default-on) | sub 15 / excl 5 | 5 exclusive [HO·V] |
| `rubato` 5.0.0 | normal/opt | src/capture/resample.rs:9, src/capture/resample.rs:61 | OPTIONAL (feature capture) | sub 20 / excl 16 | 16 exclusive [HO·V] |
| `ringbuf` 0.5 | normal/opt | src/capture/downmix.rs:8, src/capture/downmix.rs:113 | OPTIONAL (feature capture) | sub 2 / excl 2 | 2 exclusive [HO·V] |
| `tracing` 0.1 | normal | src/capture/session.rs:193, src/lifecycle.rs:378 | NEEDED | sub 9 / excl 5 |  [ND] |
| `tempfile` 3 | dev | tests/audio_e2e.rs:63 | dev only | n/a (dev) |   |
| `objc2-core-audio` 0.3.2 | normal/cfg(target_os = "macos") | src/capture/device.rs:72 | NEEDED | sub 10 / excl 0 |  [ND] |
| `objc2-core-foundation` 0.3.2 | normal/cfg(target_os = "macos") | src/capture/device.rs:82 | NEEDED | sub 7 / excl 0 |  [ND] |
| `windows` 0.62.2 +Win32_Foundation,Win32_Media_Audio,Win32_System_Com,Win32_System_Com_StructuredStorage,Win32_System_Power,Win32_UI_WindowsAndMessaging | normal/cfg(windows) | src/lifecycle.rs:90, src/lifecycle.rs:92 | NEEDED (Windows) | not measured (non-host target) | 0.62 vs 0.61 elsewhere (F39) [AG] |
| `windows-core` 0.62.2 | normal/cfg(windows) | - | NEEDED | not measured (non-host target) | required by #[implement] expansion (lifecycle.rs:198); Luna candidate-unused REFUTED [CF·R(Luna)] |
| `windows-implement` 0.60 | normal/cfg(windows) | - | UNUSED (direct) | not measured (non-host target) | macro expansion references ::windows_core only; Windows build not run [AG·V(static)] |

- **F. Problems / findings**:
  - F24 [HO, VERIFIED, measured]: `capture` default (Cargo.toml:15) -> 64 vs 41 crates without it. No consumer exists today.
  - F02 [AG, VERIFIED static]: `windows-implement` direct dep (Cargo.toml:44) unreferenced; `windows-core` (Cargo.toml:43) IS required by the `#[implement]` expansion (lifecycle.rs:198; windows-implement-0.60.2/src/gen.rs:72,85,145,183 emit `::windows_core::`). Luna listed windows-core as candidate: REFUTED.
  - F39: windows 0.62 here vs 0.61 in seven other crates [VERIFIED (Windows lead)].

### 2.6 `rightkit-automation`  (coverage: both)

- **A. Identity**: source `0.1.0`; publish=false; versions-json registry `-` / staged `0.1.0`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS (AX), Windows (UIA); no backend elsewhere.
- **B. Responsibility / API**: Brand-neutral desktop automation: observe/act on any app (macOS AX, Windows UIA) with a four-tier input ladder, focus guard, lease fencing and evidence receipts. API: `Automation`, `Policy`, `Lease`, `Journal`, `Redactor`, `native(policy)` (lib.rs:31-64).
- **C. Consumers**: RightKit crates: rightkit-agent-tools(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 12; `--no-default-features` 12.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-service` 0.1.0 | normal | src/in_app.rs:11, src/in_app.rs:121 | NEEDED | sub 12 / excl 7 |  [ND] |
| `serde_json` 1.0.150 | normal | src/in_app.rs:12 | REPLACEABLE (src/json.rs) - low value | sub 5 / excl 0 | single use in_app.rs:12; shared with rightkit-service, 0 crates saved [HO·V] |
| `tempfile` 3 | dev | tests/e2e_mac.rs:110 | dev only | n/a (dev) |   |
| `windows` 0.61 +Win32_Foundation,Win32_Graphics_Gdi,Win32_Storage_FileSystem,Win32_Storage_Xps,Win32_System_JobObjects,Win32_System_Com... | normal/cfg(windows) | src/engine.rs:301, src/engine.rs:302 | NEEDED (Windows); move to 0.62 | not measured (non-host target) | 0.61 here [Windows lead] |

- **F. Problems / findings**:
  - F56: `publish = false` but in `stagedCargo`; not on crates.io.
  - F38 / F39: Windows job object at win/sys.rs:579-601 (third copy) and windows 0.61.
  - F37 / F35: raw Command (macos/appkit.rs, macos/capture.rs) and fs::rename (redact.rs:100) outside process/fs.
  - Cargo.toml comment "Zero third-party dependencies on macOS" is misleading: rightkit-service + serde_json unconditional; serde_json has one use (in_app.rs:12) while src/json.rs hand writer exists (0 crates saved).

### 2.7 `rightkit-browser`  (coverage: both)

- **A. Identity**: source `0.2.3`; publishable; versions-json registry `0.2.2` / staged `0.2.3`; crates.io live `0.2.2`; platforms: Any OS with Chrome; Unix /bin/sh watchdog (proc.rs:125, fixed script).
- **B. Responsibility / API**: Shared CDP browser runtime: multi-page sessions, named profiles, real CDP input, stale-ref observations, egress proxy with URL policy. API: `BrowserSession`, `BrowserPage`, `Observation`, `PinnedConnection`, `policy` (lib.rs:26-39).
- **C. Consumers**: RightKit crates: rightkit-agent-tools(dep/opt), rightkit-qa(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.0, workspace (opt); genright =0.2.2 dev.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 124; `--no-default-features` 124.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `chromiumoxide` 0.9 | normal | src/page.rs:4, src/page.rs:5 | NEEDED | sub 118 / excl 54 | 54 exclusive; proxy-only consumers still pay [HO] |
| `futures` 0.3 | normal | src/page.rs:20, src/policy.rs:45 | NEEDED | sub 16 / excl 0 |  [ND] |
| `serde` 1 +derive | normal | src/page.rs:21, src/page.rs:33 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1 | normal | src/page.rs:22, src/page.rs:1057 | NEEDED | sub 5 / excl 0 |  [ND] |
| `tempfile` 3 | normal | src/config.rs:589, src/config.rs:635 | NEEDED | sub 9 / excl 4 | runtime use session.rs:32,59 (temp Chrome profile) [HO·V] |
| `thiserror` 2 | normal | src/error.rs:1, src/error.rs:5 | NEEDED | sub 6 / excl 0 |  [ND] |
| `tokio` 1 +rt-multi-thread,macros,sync,time,fs,net... | normal | src/page.rs:27, src/page.rs:28 | TRIM (fs unused) | sub 16 / excl 0 | no tokio::fs in src; 0 crates [HO·V(static)] |
| `url` 2 | normal | src/page.rs:64, src/page.rs:235 | NEEDED | sub 32 / excl 0 |  [ND] |
| `rightkit-process` 0.3.0 | normal | src/proc.rs:7 | NEEDED | sub 5 / excl 2 |  [ND] |

- **F. Problems / findings**:
  - [HO, VERIFIED static]: tokio `fs` feature has no `tokio::fs` use in src; 0 crates saved. tempfile is a legitimate runtime dep (session.rs:59).
  - Chrome resolution overlaps @rightkit/qa-chrome (judgement, G15).
  - Consumers (corrected): CodeRight =0.2.0 (workspace dep), GenRight genright-e2e dev =0.2.2, rightkit-qa (optional), rightkit-agent-tools (optional). Ownership drafts show CodeRight/ScrapeRight still use chromiumoxide directly.

### 2.8 `rightkit-cache`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: portable.
- **B. Responsibility / API**: Disk cache with hard size cap, restart-surviving LRU, atomic writes, group invalidation and stable-prefix prompt-cache keys. API: `DiskCache`, `Config`, `Key`, `CacheTracker`, `ConductorStableKey` (lib.rs:12-14).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 10; `--no-default-features` 10.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `sha2` 0.11 | normal | src/key.rs:5 | NEEDED; align 0.11->0.10 | sub 10 / excl 10 | 0.11 pulls digest 0.11 stack (10 crates) [AG·V] |
| `tempfile` 3 | dev | tests/disk_e2e.rs:5 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F50 [HO, VERIFIED]: product leakage `ConductorStableKey` / `CONDUCTOR_CACHE_KEY_PREFIX` (prefix.rs:61,66).
  - F20: sha2 0.11 (Cargo.toml:13) vs workspace 0.10.
  - F35: own temp+rename (disk.rs:252). Published 0.1.0 (crates.io live) with zero consumers.

### 2.9 `rightkit-compose`  (coverage: Luna only (re-inventoried here))

- **A. Identity**: source `0.0.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable.
- **B. Responsibility / API**: Pure composition model/evaluator: layered video, stills, cursor sprites, backgrounds, clipping, shadows, motion blur, overlays, keyframed tracks, one per-frame evaluation shared by preview and export. New content-pipeline crate (plan RK9).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 10; `--no-default-features` 10.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/eval.rs:5, src/eval.rs:19 | NEEDED | sub 7 / excl 4 |  [ND] |
| `thiserror` 2 | normal | src/error.rs:1, src/error.rs:5 | NEEDED | sub 6 / excl 3 |  [ND] |
| `serde_json` 1.0.150 | dev | tests/composition.rs:820 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - Plan-owned (RK9): no consumer yet by design. Haiku crosscut I "confirm owner, then delete" NOT adopted (F63).
  - Uses thiserror (Cargo.toml:18); same hand-impl option as F13 once the contract settles.

### 2.10 `rightkit-computer`  (coverage: both)

- **A. Identity**: source `0.1.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS only (raw FFI).
- **B. Responsibility / API**: Deprecated macOS prototype for background computer control (window list, observe tree, press, type, screenshot). README: folded into rightkit-automation (2026-10-06).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 0; `--no-default-features` 0.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|

- **F. Problems / findings**:
  - F61 [AG, VERIFIED]: zero consumers anywhere; README says "Do not start new work here". Remove after confirming no path references (none found).

### 2.11 `rightkit-contracts`  (coverage: Luna only (re-inventoried here))

- **A. Identity**: source `0.1.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable.
- **B. Responsibility / API**: Versioned content-pipeline contracts: content brief, delivery envelope, record session/event, demo plan, daemon API. JSON Schema validation (schemas embedded via include_str!, lib.rs:98-108), serde types, major-version negotiation; bin `rightkit-contracts`. New content-pipeline crate (plan RK0).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 65; `--no-default-features` 65.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/lib.rs:30, src/lib.rs:144 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/lib.rs:31, src/lib.rs:136 | NEEDED | sub 5 / excl 0 |  [ND] |
| `jsonschema` 0.58.6 no-default | normal | src/schema.rs:3, src/schema.rs:19 | NEEDED | sub 65 / excl 54 |  [ND] |

- **F. Problems / findings**:
  - Plan-owned (RK0): consumers (CutRight, GenRight, PostRight daemons) do not exist yet by design.
  - `jsonschema` with default features off = 54 exclusive crates; deliberate (Cargo.toml comment).

### 2.12 `rightkit-control`  (coverage: both)

- **A. Identity**: source `0.1.9`; publishable; versions-json registry `0.1.9` / staged `0.1.9`; crates.io live `0.1.9`; platforms: macOS (mac.rs, embedded/mac_view.rs), Windows (embedded/win_view.rs, WebView2), stub elsewhere.
- **B. Responsibility / API**: Background native input, accessibility reads and a WebDriver bridge for driving real desktop apps without stealing focus; optional in-app Tauri plugin. API: admission, cli, embedded (tauri-plugin), events, json, keys, lease, lease_store, mac, webdriver; bin `rightkit-control` (lib.rs:26-37).
- **C. Consumers**: RightKit crates: rightkit-qa(dep). Apps (manifest pins; .cache/worktree copies excluded): cockpit =0.1.6 (opt); cutright =0.1.9; genright =0.1.9.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 0; `--no-default-features` 0.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `tauri` 2 | normal/opt | src/embedded/mac_view.rs:18, src/embedded/mod.rs:53 | OPTIONAL (feature tauri-plugin) | off by default | 216 exclusive when on [AG] |
| `rightkit-service` 0.1.0 | normal/opt | src/embedded/mod.rs:45 | NEEDED | off by default |  [ND] |
| `serde_json` 1.0.150 | normal/opt | src/embedded/mod.rs:48, src/embedded/mod.rs:536 | NEEDED | off by default |  [ND] |
| `windows_061` 0.61 | normal/cfg(windows)/opt | src/embedded/win_view.rs:18 | NEEDED (wv2-0_38 default) | not measured (non-host target) | intentional dual generation [Windows lead] |
| `webview2_com_038` 0.38 | normal/cfg(windows)/opt | src/embedded/win_view.rs:18 | NEEDED | not measured (non-host target) |  [ND] |
| `windows_062` 0.62 | normal/cfg(windows)/opt | src/embedded/win_view.rs:20 | OPTIONAL (wv2-0_39) | not measured (non-host target) | intentional dual generation [Windows lead] |
| `webview2_com_039` 0.39 | normal/cfg(windows)/opt | src/embedded/win_view.rs:20 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - windows 0.61/webview2-com 0.38 and 0.62/0.39 are two selectable generations on purpose (features wv2-0_38 default / wv2-0_39): NOT duplication to remove until apps drop webview2-com 0.38 [VERIFIED (Windows lead)]; Haiku b1 "make a single choice" is re-scoped.
  - F54 [HO, VERIFIED]: stray tracked registration.patch at crate root. Default build has 0 external crates; tauri-plugin adds ~228 (tauri 216).
  - Consumers (corrected vs Haiku b1 which listed only Cockpit): CutRight =0.1.9, GenRight =0.1.9, Cockpit =0.1.6 (optional, stale), rightkit-qa (path).
  - Lease/fencing concept duplicated with rightkit-automation [HO]; raw Command in mac.rs (F37).

### 2.13 `rightkit-crash`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: macOS, Windows, Linux (reports only).
- **B. Responsibility / API**: Privacy-first panic capture, redacted crash reports, native dumps (macOS sample, Windows MiniDump helper bin) and support bundles. API: `install_panic_hook`, `record_report`, `build_support_bundle`, `create_zip`, `NativeCapture`, `enqueue_pending` (lib.rs:23-29).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 39; `--no-default-features` 39.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-logs` 0.2.0 no-default | normal | src/bundle.rs:5, src/native.rs:89 | NEEDED | sub 23 / excl 11 | default-features=false already [AG] |
| `serde` 1 +derive | normal | src/bundle.rs:8, src/bundle.rs:28 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1 | normal | src/bundle.rs:9, src/bundle.rs:72 | NEEDED | sub 5 / excl 0 |  [ND] |
| `zip` 2 no-default +deflate | normal | src/archive.rs:14, src/archive.rs:15 | OPTIONAL (feature support-bundle) | sub 21 / excl 16 | 16 exclusive; archive.rs:14-31 only [HO·V] |
| `rightkit-logs` 0.2.0 +testing | dev | src/bundle.rs:5 | NEEDED | n/a (dev) | default-features=false already [AG] |
| `windows-sys` 0.61.2 +Win32_Foundation,Win32_Storage_FileSystem,Win32_System_Diagnostics_Debug,Win32_System_Kernel,Win32_System_Memory,Win32_System_Threading | normal/cfg(target_os = "windows") | src/bin/rightkit-dump-helper.rs:18, src/bin/rightkit-dump-helper.rs:19 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - F25 [HO, VERIFIED, measured]: `zip` non-optional (16 exclusive) though only archive.rs uses it. Zero consumers.
  - macOS /usr/bin/sample spawn passes the process's own pid (native.rs:52-56): low risk; bypasses rightkit-process.
  - Fold-into-logs (Haiku H10) is a design judgement; dependency on logs verified (native.rs:89, pending.rs:2).

### 2.14 `rightkit-db`  (coverage: both)

- **A. Identity**: source `0.3.1`; publishable; versions-json registry `0.3.1` / staged `0.3.1`; crates.io live `0.3.1`; platforms: portable (Unix libc::gethostname).
- **B. Responsibility / API**: Correct-by-default SQLite/SQLCipher bootstrap: WAL, busy timeout, durability profiles, migrations, online backup/restore, quarantine/recovery, rekey, per-host path. API: `DbOptions`, `DurabilityProfile`, `ReadPool`, `write_txn`, `online_backup_to`, `rekey` (lib.rs:18-34).
- **C. Consumers**: RightKit crates: rightkit-jobs(dep). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.1, =0.3.0, workspace; genright =0.3.0; mailright =0.2.1; scraperight =0.2.1; viewright =0.2.1.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 30; `--no-default-features` 27.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rusqlite` 0.40.1 no-default +backup | normal | src/backup.rs:11, src/backup.rs:12 | NEEDED | sub 11 / excl 10 | bundled (default feature) vendors SQLite C [AG] |
| `rightkit-fs` 0.2.0 | normal | src/backup.rs:67, src/backup.rs:77 | TRIM (default-features=false after fs `json` feature) | sub 19 / excl 11 | 7-8 crates (serde, serde_core, serde_derive, syn2, serde_json, itoa, memchr, zmij) [HO·V] |
| `thiserror` 2 | normal | src/lib.rs:41, src/lib.rs:43 | NEEDED | sub 6 / excl 0 |  [ND] |
| `zeroize` 1.8 | normal | src/lib.rs:13, src/recover.rs:15 | NEEDED | sub 1 / excl 1 |  [ND] |
| `tempfile` 3 | dev | tests/contract.rs:15 | dev only | n/a (dev) |   |
| `libc` 0.2 | normal/cfg(unix) | src/host.rs:60 | NEEDED | sub 1 / excl 0 |  [ND] |

- **F. Problems / findings**:
  - F12 [HO (b1+b2), VERIFIED]: db takes rightkit-fs with default features, dragging serde + serde_json (7-8 crates) it never uses in its own source.
  - CORRECTION: Haiku b1 says no app manifest depends on rightkit-db; manifests show GenRight =0.3.0, CodeRight =0.2.1/0.3.0, MailRight/ViewRight/ScrapeRight =0.2.1.
  - `hooks` feature (Cargo.toml:25) only forwards to rusqlite/hooks; Haiku "one cfg gate" REFUTED (no cfg(feature="hooks") in db/src).
  - `bundled` default vendors SQLite C into every consumer.

### 2.15 `rightkit-embed`  (coverage: both)

- **A. Identity**: source `0.2.2`; publishable; versions-json registry `0.2.2` / staged `0.2.2`; crates.io live `0.2.2`; platforms: portable (CoreML/DirectML via rightkit-ort).
- **B. Responsibility / API**: Local text embeddings (EmbeddingGemma via ONNX Runtime, load-dynamic) and a persistent cosine vector store on SQLite. API: `Embedder`, `EmbedderConfig`, `VectorStore`, `cosine_similarity` (lib.rs:6-10).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.2; mailright =0.2.1.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 90; `--no-default-features` 90.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic | normal | src/embedder.rs:4, src/embedder.rs:470 | NEEDED but route via rightkit-ort | sub 5 / excl 0 | direct pin :13 [AG·V] |
| `rightkit-ort` 0.2.0 | normal | src/embedder.rs:5, src/embedder.rs:74 | NEEDED | sub 5 / excl 0 |  [ND] |
| `rusqlite` 0.40.1 no-default | normal | src/vector.rs:1, src/vector.rs:16 | NEEDED; route via rightkit-db / add `bundled` passthrough | sub 8 / excl 5 | no bundled; vector.rs:1 [HO·V] |
| `serde_json` 1.0.150 | normal | src/embedder.rs:718 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10.9 | normal | src/embedder.rs:9 | NEEDED | sub 10 / excl 7 |  [ND] |
| `thiserror` 2 | normal | src/embedder.rs:10, src/embedder.rs:44 | NEEDED | sub 6 / excl 0 |  [ND] |
| `tokenizers` 0.22.2 no-default +onig | normal | src/embedder.rs:11 | OPTIONAL (embedder feature) | sub 75 / excl 58 | 58 exclusive [HO·V(measured)] |
| `rusqlite` 0.40.1 +bundled | dev | src/vector.rs:1 | NEEDED; route via rightkit-db / add `bundled` passthrough | n/a (dev) | no bundled; vector.rs:1 [HO·V] |
| `tempfile` 3 | dev | src/embedder.rs:932 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F34 [AG]: direct ort =2.0.0-rc.13 (Cargo.toml:13) beside rightkit-ort (:14); embedder.rs:4-5 imports ort::session::Session.
  - F49 [HO, VERIFIED]: rusqlite default-features off with no `bundled` passthrough (Cargo.toml:15; dev has bundled :21) so consumers must supply SQLite; vector store opens its own connection (vector.rs:1) bypassing rightkit-db WAL/durability.
  - Feature split embedder vs vector-store: tokenizers 58 exclusive [HO, measured, split design unverified].
  - Consumers (corrected vs Haiku b1 "none"): MailRight =0.2.1, CodeRight context =0.2.2.

### 2.16 `rightkit-framed-sidecar`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: portable.
- **B. Responsibility / API**: Bounded JSON-lines and length-prefixed transport for sidecar processes. API: `read_json_line(_with)`, `write_json_line`, `ReadLimits`, `Incoming`, `length_prefixed`, `wire` (lib.rs).
- **C. Consumers**: RightKit crates: rightkit-jobs(dep), rightkit-qa(dep), rightkit-service(dep), rightkit-speech-client(dep). Apps (manifest pins; .cache/worktree copies excluded): template =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 11; `--no-default-features` 11.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/length_prefixed.rs:19, src/length_prefixed.rs:25 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1.0.150 | normal | src/length_prefixed.rs:30, src/length_prefixed.rs:98 | NEEDED | sub 5 / excl 4 |  [ND] |

- **F. Problems / findings**:
  - Lean (11 crates). Unbounded hand-rolled line framing elsewhere: rightkit-tts client.rs:161 + worker.rs:82, rightkit-sync helper/process.rs:125, rightkit-mcp client.rs (F36b). Haiku G16 also listed rightkit-media: REFUTED (probe.rs:121,154 parse ffmpeg stderr).

### 2.17 `rightkit-fs`  (coverage: both)

- **A. Identity**: source `0.2.0`; publishable; versions-json registry `0.2.0` / staged `0.2.0`; crates.io live `0.2.0`; platforms: Unix (libc, nix); Windows paths present (state/windows_tests.rs).
- **B. Responsibility / API**: Safe state directories, atomic writes, cross-process locks (POSIX record locks), owner-only files, stable file identity, network-FS detection, macOS clonefile; bin `rightkit-clonefile`.
- **C. Consumers**: RightKit crates: rightkit-db(dep). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.0, workspace.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 19; `--no-default-features` 19.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1 +derive | normal | src/identity.rs:22, src/identity.rs:37 | OPTIONAL (feature json) | sub 7 / excl 3 | identity.rs:22, lock.rs:18 [HO·V] |
| `serde_json` 1 | normal | src/bin/rightkit-clonefile.rs:18, src/bin/rightkit-clonefile.rs:27 | OPTIONAL (feature json) | sub 5 / excl 4 | identity.rs:247,260; lock.rs:137,379; bin clonefile.rs:18,27 [HO·V] |
| `thiserror` 2 | normal | src/clone.rs:12, src/clone.rs:15 | REPLACEABLE (hand impl) | sub 6 / excl 3 | error.rs:6, clone.rs:12; 3 exclusive [HO·V] |
| `tempfile` 3 | dev | src/state/windows_tests.rs:91 | dev only | n/a (dev) |   |
| `libc` 0.2 | normal/cfg(unix) | src/atomic.rs:122, src/clone.rs:100 | NEEDED | sub 1 / excl 0 |  [ND] |
| `nix` =0.30.1 no-default +fs | normal/cfg(unix) | src/lock.rs:254, src/lock.rs:255 | REPLACEABLE (libc::fcntl) | sub 5 / excl 4 | lock.rs:254-256 only; 4 exclusive [HO·V] |

- **F. Problems / findings**:
  - F12 [HO, VERIFIED]: serde/serde_json unconditional (Cargo.toml:19-20); used only by identity.rs:22,247,260, lock.rs:18,137,379, bin clonefile.rs:18,27.
  - F11 [HO, VERIFIED]: nix =0.30.1 only for fcntl + Errno (lock.rs:254-256) -> libc (4 exclusive crates).
  - F13: thiserror for two enums (3 crates).
  - F35 / F36: intended owner of atomic write and locks, but only rightkit-db depends on it in the workspace; making it cheap (F12, F11) is the precondition for migrating the other copies.

### 2.18 `rightkit-hotkeys`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS (hand FFI), Windows (windows 0.61).
- **B. Responsibility / API**: Global shortcuts and low-level input hooks: chord parsing, conflict detection, push-to-talk state, macOS CGEventTap (manual FFI), Windows low-level keyboard hook + RegisterHotKey. API: chord, conflict, decision, ptt, macos, windows.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 0; `--no-default-features` 0.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `windows` 0.61 +Win32_Foundation,Win32_System_LibraryLoader,Win32_System_Threading,Win32_UI_Input_KeyboardAndMouse,Win32_UI_WindowsAndMessaging | normal/cfg(windows) | src/chord.rs:127, src/chord.rs:188 | REPLACEABLE (windows-sys 0.61) | not measured (non-host target) | plain Win32 only; windows/mod.rs:12-19 [HO·V(Windows lead)] |

- **F. Problems / findings**:
  - F40 [HO, VERIFIED (Windows lead)]: Windows code uses plain Win32 only (windows/mod.rs:12-19) so windows-sys 0.61 is a drop-in; the module "has not been built on the macOS authoring host" (windows/mod.rs:3-5).
  - F56: NOT on crates.io (live 404), not in rightkit-versions.json; INDEX.md says "crates.io" and Luna said "published": REFUTED. Zero consumers.

### 2.19 `rightkit-http`  (coverage: both)

- **A. Identity**: source `0.2.0`; publishable; versions-json registry `0.2.0` / staged `0.2.0`; crates.io live `0.2.0`; platforms: portable.
- **B. Responsibility / API**: Brand-neutral blocking HTTP client (ureq wrapper): timeouts, retry/backoff with Retry-After, SSE, redaction, untrusted-web-text wrapping. API: `client::*`, `HttpError`, `RetryPolicy`, `Secret`, `sse`, `untrusted`, `testing` (feature).
- **C. Consumers**: RightKit crates: rightkit-http(dev), rightkit-llm(dep+dev), rightkit-oauth(dep+dev). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.0, workspace; mailright =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 49; `--no-default-features` 44.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/client.rs:5, src/client.rs:6 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1.0.150 | normal | src/client.rs:139, src/client.rs:228 | NEEDED | sub 5 / excl 3 |  [ND] |
| `ureq` 3.4 no-default +rustls | normal | src/client.rs:261, src/client.rs:268 | NEEDED | sub 30 / excl 27 | 27 exclusive; gzip via feature [AG] |
| `sha2` 0.10.9 | normal | src/untrusted.rs:5 | NEEDED | sub 10 / excl 8 |  [ND] |
| `getrandom` 0.3 | normal | src/retry.rs:70 | REPLACEABLE (std RandomState) - low value | sub 3 / excl 1 | retry.rs:70 jitter; 0 workspace crates [HO] |
| `rightkit-http` path no-default +testing | dev | tests/e2e.rs:8 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F16 [HO, VERIFIED, measured]: `gzip` default feature (Cargo.toml:15-17): 49 -> 44 crates without; llm/oauth use defaults.
  - Plan RK10 adds a server-side HTTPS listener on this crate; jitter getrandom could be std RandomState (0 workspace crates).
  - `testing` module compiled into the library behind a feature.

### 2.20 `rightkit-jobs`  (coverage: both)

- **A. Identity**: source `0.2.4`; publishable; versions-json registry `0.2.4` / staged `0.2.4`; crates.io live `0.2.4`; platforms: Unix (nix) + Windows via rightkit-process.
- **B. Responsibility / API**: Persistent job store (SQLite via rightkit-db), runner, stdio/CLI engine contract. API: `EngineClient`, `StoreConfig`, `StatusModel`, `JobError`, modules client/config/contract/runner/service/store/supervisor.
- **C. Consumers**: RightKit crates: rightkit-shell(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): cutright =0.2.1; genright =0.2.2.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 46; `--no-default-features` 46.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-db` 0.3.0 | normal | src/config.rs:9, src/error.rs:85 | NEEDED | sub 30 / excl 14 | bundled SQLite C [AG] |
| `rightkit-framed-sidecar` 0.1.0 | normal | src/client.rs:8, src/contract.rs:23 | NEEDED | sub 11 / excl 0 |  [ND] |
| `rightkit-process` 0.3.0 | normal | src/client.rs:11, src/supervisor.rs:44 | NEEDED | sub 5 / excl 0 |  [ND] |
| `rightkit-service` 0.1.0 | normal | src/service.rs:7 | NEEDED | sub 12 / excl 0 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/supervisor.rs:48, src/supervisor.rs:63 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/client.rs:12, src/config.rs:10 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/store.rs:8 | NEEDED | sub 10 / excl 8 |  [ND] |
| `time` 0.3 +formatting,macros | normal | src/client.rs:6, src/contract.rs:21 | NEEDED | sub 6 / excl 6 | only RFC3339 impl in jobs (store.rs:9-10); 6 exclusive [HO] |
| `uuid` 1 +v4 | normal | src/store.rs:438, src/store.rs:1191 | REPLACEABLE (getrandom + hex) | sub 4 / excl 2 | store.rs:438,1191,1205; 2 exclusive [HO·V] |
| `tempfile` 3 | dev | tests/e2e.rs:91 | dev only | n/a (dev) |   |
| `nix` =0.30.1 no-default +fs | normal/cfg(unix) | src/runner.rs:60, src/runner.rs:61 | REPLACEABLE (libc) | sub 5 / excl 0 | runner.rs:60-62 (POSIX record lock by design); 0 alone [HO·V] |

- **F. Problems / findings**:
  - F14 [HO, VERIFIED]: uuid (store.rs:438,1191,1205) -> getrandom + hex (2 exclusive crates). F11: nix for one fcntl (runner.rs:60-62; deliberate POSIX record lock, comment runner.rs:63-66).
  - bundled SQLite C via rightkit-db default; `time` is the only RFC3339 implementation here (logs/models use chrono).
  - Apps pin older: CutRight =0.2.1, GenRight =0.2.2.

### 2.21 `rightkit-license`  (coverage: both)

- **A. Identity**: source `0.1.3`; publishable; versions-json registry `0.1.3` / staged `0.1.3`; crates.io live `0.1.3`; platforms: portable.
- **B. Responsibility / API**: Offline Ed25519 verification of server-signed entitlement tokens (Rust half of @rightkit/license). API: `verify_token`, `Entitlement`, `LicenseKind`, `VerifyError`.
- **C. Consumers**: RightKit crates: rightkit-tauri(dep). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.3; heardright =0.1.3; mailright =0.1.3; scraperight =0.1.3, =0.1.3 (opt); viewright =0.1.3.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 30; `--no-default-features` 30.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `ed25519-dalek` 2 | normal | src/lib.rs:18, src/lib.rs:365 | NEEDED (TRIM unmeasured) | sub 18 / excl 18 | 18 exclusive; pins sha2 0.10.9 [HO] |
| `base64` 0.22 | normal | src/lib.rs:17, src/lib.rs:317 | REPLACEABLE (hand decoder) | sub 1 / excl 1 | lib.rs:17,317; 1 exclusive [HO·V] |
| `serde` 1 +derive | normal | src/lib.rs:19, src/lib.rs:21 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1 | normal | src/lib.rs:331 | NEEDED | sub 5 / excl 4 |  [ND] |

- **F. Problems / findings**:
  - F17 [HO, VERIFIED]: base64 0.22 -> small decoder (1 crate). F54: stray tracked crates/rightkit-license/Cargo.lock (41 entries). F42: golden vector license-v2.json in 4 byte-identical copies (md5 085249124998683b7739e5b20fb43aba).
  - ed25519-dalek 2 default features untrimmed (18 exclusive crates; trim not measured); ed25519-dalek 2.2.0 pins sha2 0.10.9 (relevant to F20).

### 2.22 `rightkit-llm`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable.
- **B. Responsibility / API**: BYOK LLM clients (Anthropic Messages, OpenAI-compatible), streaming, tool use, failover chain with circuit breaker, pricing table. API: `FailoverChain`, `Route`, `PriceTable`, `CredentialSource` (lib.rs).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 49; `--no-default-features` 49.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-http` 0.2.0 | normal | src/anthropic.rs:6, src/credentials.rs:4 | TRIM (default-features=false) | sub 49 / excl 38 | 5 crates (gzip) [HO·V] |
| `serde` 1.0.228 +derive | normal | src/pricing.rs:6, src/pricing.rs:12 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/anthropic.rs:7, src/anthropic.rs:244 | NEEDED | sub 5 / excl 0 |  [ND] |
| `rightkit-http` 0.2.0 +testing | dev | src/anthropic.rs:6 | TRIM (default-features=false) | n/a (dev) | 5 crates (gzip) [HO·V] |

- **F. Problems / findings**:
  - F16: take rightkit-http with default-features off (5 crates). F56: unpublished (absent from stagedCargo; Luna "staged" REFUTED; crates.io live 404). Zero consumers.

### 2.23 `rightkit-logs`  (coverage: both)

- **A. Identity**: source `0.2.0`; publishable; versions-json registry `0.2.0` / staged `0.2.0`; crates.io live `0.2.0`; platforms: macOS (objc2/libc), Windows (windows-sys), other.
- **B. Responsibility / API**: Privacy-safe local JSONL logging and diagnostics: events, redaction, consented outbox upload, system identity/metrics. API: `collect::*`, `Consent`, `EventRecord`, `Outbox`, `upload` (http feature).
- **C. Consumers**: RightKit crates: rightkit-crash(dep+dev), rightkit-logs(dev). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.0; genright =0.2.0; heardright =0.1.0; mailright =0.2.0; scraperight =0.1.0; template =0.2.0; viewright =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 45; `--no-default-features` 23.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `chrono` 0.4 no-default +std | normal | src/collect.rs:3 | REPLACEABLE (time) | sub 3 / excl 3 | collect.rs:3,261; 3 exclusive [HO·V] |
| `serde` 1 +derive | normal | src/collect.rs:4, src/collect.rs:12 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1 | normal | src/collect.rs:5, src/collect.rs:213 | NEEDED | sub 5 / excl 3 |  [ND] |
| `ureq` 3.4 no-default +rustls | normal/opt | src/upload.rs:81, src/upload.rs:88 | OPTIONAL (feature http, default-on) | sub 25 / excl 22 | 22 exclusive; bypasses rightkit-http [AG] |
| `getrandom` 0.4 | normal | src/identity.rs:11 | TRIM (0.4 -> 0.3) | sub 3 / excl 1 | identity.rs:11; 1 exclusive [HO·V] |
| `rightkit-logs` path +testing | dev | tests/contract.rs:1 | dev only | n/a (dev) |   |
| `objc2-foundation` 0.3 | normal/cfg(target_os = "macos") | src/system/metrics.rs:201 | OPTIONAL (feature thermal) | sub 7 / excl 6 | metrics.rs:201; 6 exclusive (macOS) [HO·V] |
| `libc` 0.2 | normal/cfg(target_os = "macos") | src/system/identity.rs:187, src/system/identity.rs:195 | NEEDED | sub 1 / excl 0 |  [ND] |
| `windows-sys` 0.61.2 +Win32_Foundation,Win32_System_ProcessStatus,Win32_System_Registry,Win32_System_SystemInformation,Win32_System_Threading | normal/cfg(target_os = "windows") | src/system/identity.rs:28, src/system/identity.rs:31 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - F15 [HO, VERIFIED, measured]: chrono (3) only parses RFC3339 (collect.rs:3,261); objc2-foundation (6, macOS) only for thermal state (metrics.rs:201); getrandom 0.4 (identity.rs:11) vs workspace 0.3.
  - F41: own ureq agent (upload.rs:81,88) bypasses rightkit-http (feature `http` default). Minimal local logging: 23 crates with `http` off.
  - F35: private atomic.rs:14.

### 2.24 `rightkit-mcp`  (coverage: both)

- **A. Identity**: source `0.4.0`; publishable; versions-json registry `0.4.0` / staged `0.4.0`; crates.io live `0.4.0`; platforms: portable.
- **B. Responsibility / API**: MCP client (stdio, streamable HTTP, trust gate, pool, OAuth hook) and server host (stdio, loopback HTTP, per-tool admission). API: `McpClient`, `ServerSpec`, server and trust modules; example example_server.
- **C. Consumers**: RightKit crates: rightkit-agent-tools(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.3.1, workspace.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 107; `--no-default-features` 107.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `getrandom` 0.3 | normal | src/server.rs:251 | NEEDED - but error ignored (bug) | sub 3 / excl 1 | server.rs:251 `let _ =` [HO·V(security)] |
| `serde` 1 +derive | normal | src/client.rs:1287, src/trust.rs:5 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1 | normal | src/advanced_http.rs:9, src/advanced_http.rs:507 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/trust.rs:6 | NEEDED | sub 10 / excl 8 |  [ND] |
| `ureq` 3.4 no-default +rustls | normal | src/client.rs:251, src/client.rs:774 | NEEDED (keep one stack) | sub 25 / excl 4 | client.rs:251,774 [AG] |
| `reqwest` 0.12 no-default +rustls-tls,stream,json | normal | src/advanced_http.rs:8, src/advanced_http.rs:53 | REPLACEABLE (ureq / rightkit-http) | sub 94 / excl 25 | advanced_http.rs:8,53,295; 25 exclusive (Haiku 30) [AG·V] |
| `tokio` 1 +rt-multi-thread,net,time,sync,macros | normal | src/advanced_http.rs:15, src/advanced_http.rs:36 | TRIM (net unused by own code) | sub 11 / excl 0 | no saving while reqwest present [HO·V] |
| `futures-util` 0.3 | normal | src/advanced_http.rs:7 | REPLACEABLE with reqwest | sub 10 / excl 0 | advanced_http.rs:7 [HO] |
| `url` 2 | normal | src/advanced_http.rs:52, src/advanced_http.rs:288 | NEEDED | sub 32 / excl 0 |  [ND] |
| `tempfile` 3 | dev | tests/client_features.rs:282 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F28 [AG, VERIFIED]: two HTTP stacks (ureq client.rs:251,774; reqwest advanced_http.rs:8,53,295). reqwest exclusive 25 crates measured (Haiku 30).
  - F29 [HO, VERIFIED, SECURITY]: `random_hex` discards the RNG error (server.rs:249-253) and supplies the default loopback bearer value (server.rs:934): all-zero on RNG failure.
  - tokio `net` feature: no tokio::net in own code (std::net only: server.rs:6); reqwest enables it anyway (0 saving).
  - Consumer: CodeRight only (=0.3.1, workspace), older than 0.4.0.

### 2.25 `rightkit-media`  (coverage: both)

- **A. Identity**: source `0.2.3`; publishable; versions-json registry `0.2.3` / staged `0.2.3`; crates.io live `0.2.3`; platforms: portable (drives FFmpeg binaries).
- **B. Responsibility / API**: Verified FFmpeg artifact and lifecycle primitives: approved catalog, ffmpeg/ffprobe verification, command building, runner, staging, lifecycle policy, cancellation. API: `resolve_ffmpeg`, `verify_ffmpeg`, `sha256_file`, `CancellationToken`, `LifecyclePolicy`.
- **C. Consumers**: RightKit crates: rightkit-asr(dev), rightkit-audio(dep), rightkit-ort(dev), rightkit-playback(dep), rightkit-vad(dev). Apps (manifest pins; .cache/worktree copies excluded): genright =0.2.3.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 24; `--no-default-features` 24.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-process` 0.3.0 | normal | src/lib.rs:15, src/lib.rs:951 | NEEDED | sub 5 / excl 3 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/lib.rs:16, src/lib.rs:92 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1.0.150 | normal | src/lib.rs:138, src/lib.rs:443 | NEEDED | sub 5 / excl 4 |  [ND] |
| `sha2` =0.10.9 | normal | src/lib.rs:17 | TRIM (exact pin =0.10.9 -> "0.10") | sub 10 / excl 8 | Cargo.toml:18 [AG·V] |
| `tempfile` 3 | dev | src/staging.rs:280 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F21 [AG, VERIFIED]: sha2 =0.10.9 exact pin (Cargo.toml:18), no recorded reason.
  - Package include ships catalog/, recipes/, third-party/ (not audited by either inventory). Consumer: GenRight studio =0.2.3.

### 2.26 `rightkit-models`  (coverage: both)

- **A. Identity**: source `0.2.3`; publishable; versions-json registry `0.2.3` / staged `0.2.3`; crates.io live `0.2.3`; platforms: portable.
- **B. Responsibility / API**: Local-model manifest/installer: revisions, resumable verified downloads, tar.gz/zip extraction, atomic activation, rollback, hardware fit, suite (RightApps) artifact client (feature `rightapps`). 9,267 source lines.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): cutright =0.2.1; genright =0.2.2.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 68; `--no-default-features` 68.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `ed25519-dalek` 2 | normal/opt | src/rightapps.rs:6 | NEEDED | off by default |  [ND] |
| `base64` 0.22 | normal/opt | src/rightapps.rs:5 | REPLACEABLE (hand decoder) [rightapps] | off by default | 1 crate [HO] |
| `url` 2 | normal/opt | src/download.rs:26, src/download.rs:51 | NEEDED | off by default |  [ND] |
| `chrono` 0.4 no-default +std | normal/opt | src/rightapps.rs:495, src/rightapps.rs:498 | REPLACEABLE (time) [rightapps] | off by default | rightapps.rs:495,498 [HO·V] |
| `serde` 1 +derive | normal | src/download.rs:11, src/download.rs:49 | NEEDED | sub 7 / excl 3 |  [ND] |
| `serde_json` 1 | normal | src/download.rs:454, src/download.rs:480 | NEEDED | sub 5 / excl 2 |  [ND] |
| `sha2` 0.11 | normal | src/hash.rs:1, src/suite/manifest.rs:6 | REPLACEABLE (0.10) | sub 10 / excl 8 | 0.11; 8 exclusive; ed25519-dalek keeps 0.10 in rightapps graphs [HO·V] |
| `ureq` 3.4 no-default +rustls | normal | src/download.rs:184, src/download.rs:194 | NEEDED; duplicates rightkit-http | sub 25 / excl 21 | 21 exclusive [AG] |
| `tar` 0.4 | normal | src/archive.rs:17, src/archive.rs:28 | OPTIONAL (feature) | sub 8 / excl 6 | 6 exclusive [HO·V] |
| `flate2` 1 | normal | src/archive.rs:24, src/suite/archive.rs:127 | OPTIONAL with tar | sub 6 / excl 0 | shared with ureq gzip [HO] |
| `dirs` 7 | normal | src/suite/discover.rs:72, src/suite/discover.rs:76 | REPLACEABLE (std env) | sub 4 / excl 3 | 3 call sites (suite/*.rs); 3 exclusive [HO·V] |
| `getrandom` 0.3 | normal | src/suite/install.rs:121 | NEEDED | sub 3 / excl 1 |  [ND] |
| `zip` 2 no-default +deflate | normal | src/archive.rs:52, src/archive.rs:53 | OPTIONAL (feature) | sub 21 / excl 10 | 10 exclusive; archive.rs:52 [HO·V] |
| `tempfile` 3 | dev | tests/adoption.rs:220 | dev only | n/a (dev) |   |
| `rightkit-qa` 0.2.8 | dev | tests/e2e_resume.rs:8 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F19 [HO, VERIFIED, measured]: zip (10 exclusive), tar (6), flate2 are not optional; sha2 0.11 (8, F20); dirs 7 (3) for three call sites; chrono under rightapps (3).
  - F41: own ureq (download.rs:184, rightapps.rs:184) bypasses rightkit-http.
  - F35 / F36 / G30: two parallel stacks inside the crate (archive.rs:128,171 vs suite/archive.rs:181,224; store.rs vs suite/store.rs; lock.rs vs suite/lease.rs).
  - Dev-dep on rightkit-qa 0.2.8 (Cargo.toml:42) is a publish-order note, far behind 0.2.13.
  - Consumers: CutRight =0.2.1, GenRight =0.2.2.

### 2.27 `rightkit-oauth`  (coverage: both)

- **A. Identity**: source `0.3.0`; publishable; versions-json registry `0.3.0` / staged `0.3.0`; crates.io live `0.3.0`; platforms: Unix for `server` (geteuid, O_NOFOLLOW); defaults cross-platform.
- **B. Responsibility / API**: OAuth 2.0 sign-in: loopback PKCE, RFC 8628 device flow, provider presets, refreshing credentials, encrypted daemon persistence (`server` feature). API: `OAuthClient`, `TokenSet`, `device::*`, `server`.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.0, workspace; mailright =0.2.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 56; `--no-default-features` 49.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-http` 0.2.0 | normal | src/credentials.rs:17, src/device.rs:14 | TRIM (default-features=false) | sub 49 / excl 26 | 5 crates [HO·V] |
| `rightkit-secrets` 0.1.0 | normal/opt | src/credentials.rs:26, src/credentials.rs:30 | NEEDED | sub 19 / excl 4 |  [ND] |
| `rightkit-process` 0.3.0 | normal/opt | src/system.rs:11 | NEEDED | sub 5 / excl 3 |  [ND] |
| `getrandom` 0.3 | normal | src/lib.rs:744, src/server/mod.rs:633 | NEEDED | sub 3 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/device.rs:139, src/device.rs:202 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10.9 | normal | src/lib.rs:39, src/server/mod.rs:13 | NEEDED | sub 10 / excl 0 |  [ND] |
| `base64` 0.22 | normal/opt | src/server/mod.rs:10, src/server/mod.rs:555 | REPLACEABLE inline [server] | off by default | server/mod.rs:10,555 [HO] |
| `ring` 0.17 | normal/opt | src/server/store.rs:5 | NEEDED [server] | off by default | pulls getrandom 0.2, shlex 2 [HO] |
| `subtle` 2.6 | normal/opt | src/server/mod.rs:18 | REPLACEABLE inline [server] | off by default | server/mod.rs:18 [HO] |
| `url` 2.5 | normal/opt | src/device.rs:122, src/device.rs:135 | NEEDED [server] | off by default | 27 crates [HO] |
| `zeroize` 1.8 | normal/opt | src/server/mod.rs:19, src/server/mod.rs:109 | NEEDED | off by default |  [ND] |
| `serde` 1.0.228 +derive | normal/opt | src/server/mod.rs:96, src/server/mod.rs:194 | NEEDED | off by default |  [ND] |
| `libc` 0.2 | normal/opt | src/server/store.rs:238, src/server/store.rs:270 | NEEDED | off by default |  [ND] |
| `axum` 0.8 | normal/opt | - | UNUSED in lib (example only) | off by default | move to dev-dependencies [HO·V] |
| `tokio` 1 +rt-multi-thread,macros,net | normal/opt | - | UNUSED in lib (example only) | off by default | move to dev-dependencies [HO·V] |
| `rightkit-http` 0.2.0 +testing | dev | src/credentials.rs:17 | TRIM (default-features=false) | n/a (dev) | 5 crates [HO·V] |
| `tempfile` 3 | dev | tests/server.rs:394 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F05 [HO, VERIFIED]: axum/tokio are optional normal deps used only by examples/server_axum.rs (feature server-example, Cargo.toml:24,53-54); not used by src.
  - F16: take rightkit-http with default-features off. F17: base64/subtle in `server` replaceable inline (0 crates).
  - Plan RK1 extends `server` mode and adds a Linux secrets backend: do not treat `server` as dead. Consumers: CodeRight, MailRight =0.2.0.

### 2.28 `rightkit-ocr`  (coverage: both)

- **A. Identity**: source `0.1.2`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS (Vision/objc2), Windows (WinRT + ort DirectML).
- **B. Responsibility / API**: Product-neutral OCR: Apple Vision / Windows.Media.Ocr native backends with per-line geometry, isolated worker process, verified two-stage ONNX fallback via rightkit-ort. API: `OcrEngine`, `ocr_image_with_manifest`, `OcrArtifactManifest`, `WorkerCommand`; bin `rightkit-ocr`.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 51; `--no-default-features` 51.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `image` 0.25 no-default +png,jpeg | normal | src/bin/rightkit-ocr.rs:29, src/bin/rightkit-ocr.rs:31 | NEEDED | sub 18 / excl 16 |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic | normal | src/bin/rightkit-ocr.rs:29, src/bin/rightkit-ocr.rs:47 | REPLACEABLE (rightkit-ort) | sub 5 / excl 0 | Cargo.toml:23,41 [AG·V] |
| `rightkit-ort` 0.2.0 | normal | src/lib.rs:34, src/router.rs:12 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/lib.rs:35 | NEEDED | sub 10 / excl 8 |  [ND] |
| `serde` 1 +derive | normal | src/router.rs:13, src/router.rs:94 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1 | normal | src/bin/rightkit-ocr.rs:73, src/worker.rs:140 | NEEDED | sub 5 / excl 4 |  [ND] |
| `base64` 0.22 | dev | tests/artifact_contract.rs:66 | dev only | n/a (dev) |   |
| `objc2` 0.6 | normal/cfg(target_os = "macos") | src/native/vision.rs:26, src/native/vision.rs:27 | NEEDED | sub 2 / excl 0 |  [ND] |
| `objc2-foundation` 0.3 +NSString,NSArray,NSError,NSDictionary,NSValue,NSGeometry | normal/cfg(target_os = "macos") | src/native/vision.rs:33 | TRIM (NSValue, NSGeometry unreferenced) | sub 8 / excl 0 | removal safety unverified without a build [HO·U] |
| `objc2-core-graphics` 0.3 +CGImage,CGColorSpace,CGDataProvider | normal/cfg(target_os = "macos") | src/native/vision.rs:30 | NEEDED | sub 10 / excl 0 |  [ND] |
| `objc2-vision` 0.3 no-default +std,objc2-core-foundation,objc2-core-graphics,VNRequest,VNRequestHandler,VNRecognizeTextRequest... | normal/cfg(target_os = "macos") | src/native/vision.rs:34 | NEEDED | sub 11 / excl 1 |  [ND] |
| `objc2-core-foundation` 0.3 +CFCGTypes | normal/cfg(target_os = "macos") | src/native/vision.rs:29 | NEEDED | sub 7 / excl 0 |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic,directml | normal/cfg(windows) | src/bin/rightkit-ocr.rs:29, src/bin/rightkit-ocr.rs:47 | REPLACEABLE (rightkit-ort) | not measured (non-host target) | Cargo.toml:23,41 [AG·V] |
| `windows` 0.62 +Foundation,Foundation_Collections,Globalization,Graphics_Imaging,Media_Ocr,Security_Cryptography... | normal/cfg(windows) | src/bin/rightkit-ocr.rs:53, src/lib.rs:22 | TRIM (Storage_Streams, Foundation_Collections?) | not measured (non-host target) | needs Windows build; 0.62 [HO·U] |

- **F. Problems / findings**:
  - F34 [AG]: direct `ort` on two targets (Cargo.toml:23,41) beside rightkit-ort (:24).
  - F09 [HO, UNVERIFIABLE removal safety]: objc2-foundation features NSValue/NSGeometry never named in src (vision.rs:33 imports NSArray/NSDictionary/NSString); Windows features Storage_Streams/Foundation_Collections not named (win_native.rs:26-30).
  - CORRECTION: Luna says consumers include ScrapeRight/Coderight-adjacent crates; no manifest depends on rightkit-ocr (ScrapeRight crates/ocr uses rightkit-ort). ViewRight keeps its own crates/ocr (755 lines).
  - publish = false; source 0.1.2 vs INDEX 0.1.1 (stale).

### 2.29 `rightkit-ort`  (coverage: both)

- **A. Identity**: source `0.2.1`; publishable; versions-json registry `0.2.1` / staged `0.2.1`; crates.io live `0.2.1`; platforms: portable (CoreML on Apple targets, DirectML on Windows).
- **B. Responsibility / API**: Single pinned ONNX Runtime suite: dynamic-library resolution, provider probing, session setup; re-exports ort, half, ndarray. API: `configure_runtime`, `init_environment`, `probe_providers`, `SessionOptions` (lib.rs:26-52).
- **C. Consumers**: RightKit crates: rightkit-asr(dev), rightkit-embed(dep), rightkit-ocr(dep), rightkit-vad(dev). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.0, workspace; cutright =0.1.4; genright =0.2.0, =0.2.0 dev; scraperight =0.2.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 5; `--no-default-features` 0.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `half` 2 | normal/opt | src/lib.rs:29, src/lib.rs:30 | NEEDED | off by default |  [ND] |
| `ndarray` 0.17 | normal/opt | src/lib.rs:33, src/lib.rs:34 | NEEDED | off by default |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic | normal/opt | src/environment.rs:264, src/environment.rs:293 | NEEDED | sub 5 / excl 5 |  [ND] |
| `rightkit-media` 0.2.3 | dev | tests/e2e_runtime.rs:9 | dev only | n/a (dev) |   |
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic,coreml | normal/cfg(target_vendor = "apple")/opt | src/environment.rs:264, src/environment.rs:293 | NEEDED | not measured (non-host target) |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,api-24,load-dynamic,directml | normal/cfg(target_os = "windows")/opt | src/environment.rs:264, src/environment.rs:293 | NEEDED | not measured (non-host target) |  [ND] |
| `windows-sys` 0.61.2 +Win32_System_SystemInformation | normal/cfg(target_os = "windows") | src/environment.rs:125 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - Intended single owner of the ORT pin (F34); load-dynamic only, no download path. App pins: CutRight 0.1.4, others 0.2.0, source 0.2.1.

### 2.30 `rightkit-overlay`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS policy + Windows via windows-sys; no-op stubs elsewhere.
- **B. Responsibility / API**: Overlay state, hit testing and placement policy for HeardRight's pill and Cockpit's notch (macOS status-level policy, Windows topmost reassert). API: `OverlayModel`, `WorkArea`, `HoverGate`, `windows::make_noactivate` (lib.rs).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 7; `--no-default-features` 7.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` workspace | normal | src/lib.rs:7, src/lib.rs:15 | NEEDED | sub 7 / excl 7 |  [ND] |
| `serde_json` workspace | dev | tests | UNUSED (dev-dependency) | n/a (dev) | Cargo.toml:19; tests/ empty [AG·V] |
| `windows-sys` 0.61.2 +Win32_Foundation,Win32_UI_WindowsAndMessaging | normal/cfg(windows) | src/lib.rs:83, src/lib.rs:84 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - F04 [AG, VERIFIED]: dev serde_json unused (Cargo.toml:19).
  - F56: not on crates.io (live 404), not in versions JSON; INDEX.md "crates.io" is the publishable flag, not publication.
  - F62: zero consumers; Swift RightKitOverlay duplicates placement (Overlay.swift:97-124). Adopt-or-archive is an owner decision.

### 2.31 `rightkit-pdf`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: portable.
- **B. Responsibility / API**: PDF engine extracted from ViewRight crates/pdf: inspect, compose, font subsetting, staged atomic save, optional merge and text extraction. API: `compose_candidate`, `commit_pdf_save`, `extract_text`, `FontCatalog`, `PdfEditCommand` (lib.rs:20-29).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 85; `--no-default-features` 85.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `lopdf` 0.44.0 no-default +rayon | normal | src/compose.rs:79, src/compose.rs:80 | NEEDED | sub 60 / excl 32 |  [ND] |
| `thiserror` 2.0.20 | normal | src/commit.rs:13, src/commit.rs:15 | NEEDED | sub 6 / excl 0 |  [ND] |
| `fontdb` 0.24.0 | normal | src/font.rs:57, src/font.rs:65 | OPTIONAL (feature system-fonts) | sub 7 / excl 4 | 4 exclusive; font.rs:15,64 [HO·V] |
| `ttf-parser` 0.25.1 | normal | src/compose.rs:490, src/compose.rs:763 | NEEDED | sub 1 / excl 1 |  [ND] |
| `subsetter` 0.2.6 | normal | src/font.rs:431, src/font.rs:442 | NEEDED | sub 20 / excl 12 |  [ND] |
| `wuff` 0.2.8 | normal | src/font.rs:322 | NEEDED | sub 10 / excl 4 | pure Rust, no build.rs (source in registry) [HO·V] |
| `serde` workspace | normal | src/compose.rs:81, src/compose.rs:1237 | NEEDED | sub 7 / excl 4 |  [ND] |
| `sha2` 0.11.0 | normal | src/model.rs:32 | NEEDED (0.11) | sub 10 / excl 0 | cannot fully leave 0.11 because lopdf 0.44 needs it [CF·R(b3 direction)] |
| `getrandom` 0.4.3 | normal | src/commit.rs:33, src/save.rs:236 | NEEDED | sub 4 / excl 0 |  [ND] |
| `pdf-extract` 0.12 | normal/opt | src/extract.rs:15, src/extract.rs:25 | OPTIONAL (already feature) | off by default | pulls lopdf 0.42 + RustCrypto (24) [HO·V] |
| `serde_json` workspace | dev | src/model.rs:1057 | dev only | n/a (dev) |   |
| `image` 0.25 no-default +png | dev | tests/e2e.rs:413 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F44 [HO, VERIFIED]: fontdb always compiled (4 exclusive; font.rs:64); pdf-extract (feature `extract`) pulls lopdf 0.42 + old RustCrypto (24 crates; lock holds lopdf 0.42 and 0.44).
  - F20: sha2 0.11 (Cargo.toml:35), but lopdf 0.44 itself pulls sha2 0.11 and ed25519-dalek/tauri need 0.10, so pdf cannot fully leave 0.11.
  - wuff 0.2.9 source is in the cargo registry with no build.rs (pure Rust): resolves Haiku b3 "not inspected".
  - Zero consumers; ViewRight still ships its own src-tauri/crates/pdf (4,586 lines; path member, Cargo.toml:2): adoption is the open item (F64).

### 2.32 `rightkit-playback`  (coverage: both)

- **A. Identity**: source `0.1.1`; publishable; versions-json registry `0.1.1` / staged `0.1.1`; crates.io live `0.1.1`; platforms: portable.
- **B. Responsibility / API**: Source-preserving FFmpeg compatibility playback preparation. API: `compatibility_copy`, `PlaybackRegistry`, `FfmpegCandidates`, optional Tauri command module (feature `tauri`).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 29; `--no-default-features` 29.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-media` 0.2.3 | normal | src/lib.rs:24 | NEEDED | sub 24 / excl 0 |  [ND] |
| `rightkit-process` 0.3.0 | normal | src/lib.rs:25 | NEEDED | sub 5 / excl 0 |  [ND] |
| `parking_lot` =0.12.5 | normal | src/lib.rs:21 | REPLACEABLE (std Mutex), exact pin | sub 7 / excl 5 | exact =0.12.5 [HO] |
| `serde` 1.0.228 +derive | normal | - | UNUSED | sub 7 / excl 0 | Cargo.toml:22 [AG·V] |
| `serde_json` 1.0.150 | normal | - | UNUSED | sub 5 / excl 0 | Cargo.toml:23 [AG·V] |
| `sha2` =0.10.9 | normal | src/lib.rs:22 | NEEDED; exact pin =0.10.9 | sub 10 / excl 0 | Cargo.toml:24 [AG] |
| `tauri` 2 no-default | normal/opt | src/lib.rs:432, src/lib.rs:468 | NEEDED | off by default |  [ND] |

- **F. Problems / findings**:
  - F03 [AG, VERIFIED]: serde, serde_json declared (Cargo.toml:22-23), zero references in src/tests.
  - F21: exact pins parking_lot =0.12.5, sha2 =0.10.9.
  - Published on crates.io 0.1.1 (live). CutRight comment mediaAdapters.ts:8 "not published yet" is stale. Zero crate consumers (CutRight uses a TS adapter).

### 2.33 `rightkit-process`  (coverage: both)

- **A. Identity**: source `0.3.1`; publishable; versions-json registry `0.3.1` / staged `0.3.1`; crates.io live `0.3.1`; platforms: Unix (nix) and Windows (job objects, windows 0.61).
- **B. Responsibility / API**: Ownership-safe child lifecycle, restart, stall watchdog and health primitives. API: `OwnedCommand`, `OwnedChild`, `ProcessHandle`, `HealthPolicy`, `StallWatchdog`, `WindowsChild`.
- **C. Consumers**: RightKit crates: rightkit-browser(dep), rightkit-jobs(dep), rightkit-media(dep), rightkit-oauth(dep/opt), rightkit-playback(dep), rightkit-qa(dep), rightkit-sandbox(dep), rightkit-shell(dep/opt), rightkit-tts(dep). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.3.1; cutright =0.1.0; genright =0.3.0, =0.3.0 dev; template =0.2.1.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 5; `--no-default-features` 5.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `trybuild` =1.0.116 | dev | tests/api.rs:3 | dev only | n/a (dev) |   |
| `nix` =0.30.1 no-default +signal | normal/cfg(unix) | src/owned.rs:253, src/owned.rs:255 | REPLACEABLE (libc) - signal feature IS used | sub 5 / excl 5 | owned.rs:692-693 uses sys::signal::kill + unistd::Pid + errno [HO·R (Haiku claim) / new finding F11] |
| `windows` 0.61 +Win32_Foundation,Win32_Security,Win32_System_Diagnostics_ToolHelp,Win32_System_JobObjects,Win32_System_Threading | normal/cfg(windows) | src/lib.rs:8, src/lib.rs:9 | NEEDED (Windows); move to 0.62 | not measured (non-host target) | 0.61 [Windows lead] |

- **F. Problems / findings**:
  - F10 [HO, REFUTED]: Haiku b3 "nix signal feature unused" is wrong: owned.rs:692-693 uses nix::sys::signal::kill, nix::unistd::Pid, nix::errno::Errno.
  - F11 (new, mine): everything else is nix::libc (process_unix.rs:1, process_handle.rs:130-148, owned.rs:253-255,556), so nix (5 exclusive crates) is replaceable by libc; tests still use nix::process / nix::fs.
  - F38: owner of Windows job objects (owned.rs:800, process_windows.rs:68); copies in sandbox, automation.
  - App pins: CutRight =0.1.0, template =0.2.1, GenRight =0.3.0, CodeRight =0.3.1 (four generations).

### 2.34 `rightkit-qa`  (coverage: both)

- **A. Identity**: source `0.2.13`; publishable; versions-json registry `0.2.12` / staged `0.2.13`; crates.io live `0.2.12`; platforms: macOS, Windows, Unix.
- **B. Responsibility / API**: Rust QA harness for Right Suite apps: engine black-box scenarios, hidden native UI driving through rightkit-control, mock providers, loudness/WAV/PNG validators, run lock, orphan sweep, hashed evidence; bin `rightkit-qa`. API: modules audio..workspace (lib.rs:22-44).
- **C. Consumers**: RightKit crates: rightkit-models(dev). Apps (manifest pins; .cache/worktree copies excluded): cockpit =0.2.12 dev; cutright =0.2.12; genright =0.2.12, =0.2.12 dev.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 46; `--no-default-features` 46.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-control` 0.1.8 | normal | src/control.rs:162, src/control.rs:221 | NEEDED | sub 0 / excl 0 |  [ND] |
| `rightkit-framed-sidecar` 0.1.0 | normal | src/engine.rs:6 | NEEDED | sub 11 / excl 0 | engine.rs:6 (Haiku b3 UNUSED refuted) [CF·R] |
| `rightkit-service` 0.1.0 | normal | src/control.rs:15, src/control.rs:154 | NEEDED | sub 12 / excl 0 |  [ND] |
| `rightkit-process` 0.3.0 | normal | src/control.rs:12, src/control.rs:14 | NEEDED | sub 5 / excl 2 |  [ND] |
| `rightkit-browser` 0.2.2 | normal/opt | src/browser.rs:12, src/browser.rs:239 | NEEDED | off by default |  [ND] |
| `tokio` 1 +rt,time | normal/opt | src/browser.rs:108, src/browser.rs:145 | NEEDED | off by default |  [ND] |
| `png` 0.18 | normal | src/browser.rs:280, src/browser.rs:284 | NEEDED | sub 9 / excl 7 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/check.rs:5, src/check.rs:81 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/bin/rightkit-qa.rs:200, src/browser.rs:16 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/util.rs:2 | NEEDED | sub 10 / excl 8 |  [ND] |
| `libc` 0.2 | normal | src/storage.rs:668, src/storage.rs:669 | NEEDED | sub 1 / excl 0 |  [ND] |
| `time` 0.3 +formatting | normal | src/control.rs:23, src/engine.rs:14 | NEEDED | sub 5 / excl 5 |  [ND] |
| `toml` 0.8 | normal | src/bin/rightkit-qa.rs:62, src/bin/rightkit-qa.rs:151 | TRIM (0.8 vs 1.x) | sub 15 / excl 8 | lock carries 0.8.2, 0.9, 1.1.2 [HO] |
| `uuid` 1 +v4 | normal | src/util.rs:56 | REPLACEABLE (getrandom) - same as jobs | sub 4 / excl 2 | util.rs:56; 2 exclusive [HO] |
| `windows` 0.61 +Win32_Foundation,Win32_UI_WindowsAndMessaging | normal/cfg(windows) | src/config.rs:35, src/config.rs:42 | NEEDED (Windows); move to 0.62 | not measured (non-host target) | 0.61 [Windows lead] |

- **F. Problems / findings**:
  - F06 [CF, REFUTED]: Haiku b3 "rightkit-framed-sidecar UNUSED" is wrong (engine.rs:6 uses read_json_line/write_json_line/FrameError; Cargo.toml:19). Luna correctly lists it as a dependency.
  - F37: 20 raw Command::new sites despite depending on rightkit-process. toml 0.8 (Cargo.toml:30) is the odd toml generation (lock has 0.8.2/0.9/1.1.2). F39: windows 0.61 (Cargo.toml:42).
  - Consumers pin =0.2.12 (CutRight, GenRight, Cockpit); crates.io live 0.2.12, source 0.2.13 staged.

### 2.35 `rightkit-qts-ggml-sys`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: macOS (Metal), Windows (Vulkan, pregenerated bindings), Linux.
- **B. Responsibility / API**: Low-level vendored GGML FFI (ggml 0.9.8, 7.6 MB) built with cmake; bindings via bindgen unless a pregenerated file matches. `links = "rightkit_ggml"`.
- **C. Consumers**: RightKit crates: rightkit-qts-ggml(dep). Apps (manifest pins; .cache/worktree copies excluded): genright =0.1.0 (opt).
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 28; `--no-default-features` 28.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `bindgen` 0.72 | build | build.rs:244, build.rs:247 | REPLACEABLE (pregenerated bindings) | sub 24 / excl 24 | 24 exclusive [HO·V] |
| `cmake` 0.1 | build | build.rs:33, build.rs:82 | NEEDED | sub 4 / excl 4 | 4 exclusive [AG] |

- **F. Problems / findings**:
  - F30 [HO, VERIFIED, measured]: bindgen (24 exclusive build crates; libclang requirement) runs for every target except x86_64-pc-windows-msvc (build.rs:226-256; only bindings-x86_64-pc-windows-msvc.rs exists).
  - F31 [HO, VERIFIED]: build.rs:281-309 probes Homebrew, ~/.local/lib/*/site-packages/cmake, %LOCALAPPDATA% Python and Program Files for cmake and sets CMAKE when unset: build-time execution from home-directory locations.

### 2.36 `rightkit-qts-ggml`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: as sys crate.
- **B. Responsibility / API**: Thin safe wrapper over the ggml sys crate (`pub use ggml_sys as sys`; owned `Context`). 59 lines.
- **C. Consumers**: RightKit crates: rightkit-qts(dep). Apps (manifest pins; .cache/worktree copies excluded): genright =0.1.0 (opt).
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 28; `--no-default-features` 28.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-qts-ggml-sys` 0.1.0 no-default | normal | - | NEEDED | sub 28 / excl 28 |  [ND] |

- **F. Problems / findings**:
  - F33 [AG(soft), VERIFIED]: rightkit-qts only imports qts_ggml::sys (e.g. backend.rs:7); `Context` is used only by this crate's own test (lib.rs:50ff). GenRight declares it only as a forwarding dependency (genright-qts/Cargo.toml:14,35). Merge into -sys or qts.

### 2.37 `rightkit-qts`  (coverage: both)

- **A. Identity**: source `0.2.1`; publishable; versions-json registry `0.2.1` / staged `0.2.1`; crates.io live `0.2.1`; platforms: macOS (Metal/Accelerate), Windows (Vulkan, DirectML via ort), Linux CPU.
- **B. Responsibility / API**: Qwen3 TTS inference (GGUF + GGML), vendored from yet-another-ai/qts v0.2.0. API: `Qwen3TtsEngine`, `SynthesizeRequest`, `StreamingSynthesis`, `SpeakerEncoder`, `VocoderConfig`.
- **C. Consumers**: RightKit crates: rightkit-tts(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): genright =0.2.1 (opt).
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 84; `--no-default-features` 84.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-qts-ggml` 0.1.0 no-default | normal | - | NEEDED (merge candidate) | sub 28 / excl 15 | F33 [AG] |
| `thiserror` 2 | normal | src/error.rs:3, src/error.rs:5 | NEEDED | sub 6 / excl 3 |  [ND] |
| `rand` 0.10 | normal | src/pipeline/tts_transformer.rs:8, src/pipeline/tts_transformer.rs:5259 | NEEDED | sub 6 / excl 3 |  [ND] |
| `hound` 3.5.1 | normal | src/pipeline/speaker_encoder.rs:7, src/pipeline/speaker_encoder.rs:449 | NEEDED | sub 1 / excl 1 |  [ND] |
| `rustfft` 6.4.1 | normal | src/pipeline/speaker_encoder.rs:8, src/pipeline/speaker_encoder.rs:9 | NEEDED | sub 8 / excl 4 |  [ND] |
| `prost` 0.14 | normal | src/voice_clone_prompt.rs:2 | NEEDED | sub 10 / excl 2 |  [ND] |
| `ort` =2.0.0-rc.13 no-default +std,ndarray,tracing,load-dynamic,api-24 | normal | src/pipeline/vocoder.rs:9, src/pipeline/vocoder.rs:10 | REPLACEABLE (rightkit-ort); TRIM ndarray + tracing features | sub 16 / excl 10 | 6 crates [HO·V] |
| `prost-build` 0.13 | build | build.rs:11 | TRIM (align to 0.14) | sub 33 / excl 15 | 0.13 build vs 0.14 runtime [HO·V] |
| `protoc-bin-vendored` 3 | build | build.rs:10 | NEEDED (build-time exec) | sub 9 / excl 9 | 9 exclusive; executes vendored protoc [HO] |

- **F. Problems / findings**:
  - F08 [HO, VERIFIED static, measured]: ort `ndarray` + `tracing` features unused in src; removal = 6 crates (matrixmultiply, ndarray, pin-project-lite, rawpointer, tracing, tracing-core), measured by edge-pruning the tree.
  - F34: direct ort (Cargo.toml:47).
  - F32 [HO, VERIFIED]: prost-build 0.13 (build) vs prost 0.14 (runtime) -> two prost stacks in the lock; PINNED.md:8 says ort rc.12 (stale; Cargo.toml rc.13).
  - build.rs:10-12 executes a vendored protoc on every build (protoc-bin-vendored, 9 exclusive).
  - Consumer: GenRight genright-qts =0.2.1 (optional). 23 ort feature forwards.

### 2.38 `rightkit-sandbox`  (coverage: both)

- **A. Identity**: source `0.3.1`; publishable; versions-json registry `0.3.1` / staged `0.3.1`; crates.io live `0.3.1`; platforms: macOS seatbelt (deprecated sandbox_init FFI, seatbelt.rs:16-20,127), Windows job/AppContainer, Unix PTY.
- **B. Responsibility / API**: OS sandbox (seatbelt, AppContainer), PTY lease, process-tree and reserve-before-spawn resource authority. API: `Sandbox`, `SandboxPolicy`, `PtySession`, `ResourceProfile`, `process_tree`.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.2.4.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 20; `--no-default-features` 20.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-process` 0.3.0 | normal | src/spawn.rs:7, src/spawn.rs:40 | NEEDED | sub 5 / excl 2 |  [ND] |
| `portable-pty` 0.9 | normal | src/pty.rs:13, src/pty.rs:147 | NEEDED; brings nix 0.28 | sub 18 / excl 15 | 15 exclusive [HO·V] |
| `libc` 0.2 | normal/cfg(unix) | src/linux.rs:25, src/linux.rs:26 | NEEDED | sub 1 / excl 0 |  [ND] |
| `windows` 0.61 +Win32_Foundation,Win32_System_Threading,Win32_System_JobObjects,Win32_Security | normal/cfg(windows) | src/bin/rightkit-sandbox-exec.rs:15, src/bin/rightkit-sandbox-exec.rs:17 | REPLACEABLE (rightkit-process job API); move to 0.62 | not measured (non-host target) | windows_job.rs:7-34 [HO·V(Windows lead)] |

- **F. Problems / findings**:
  - F38 [HO, VERIFIED]: windows_job.rs:7-34 duplicates the process job-object API. F43: portable-pty 0.9 carries nix 0.28 / cfg_aliases 0.1 / thiserror 1 / bitflags 1 (15 exclusive crates).
  - Source 0.3.1 (live crates.io 0.3.1; versions JSON registry still 0.3.0). Only consumer: CodeRight tools =0.2.4 (two minors behind).

### 2.39 `rightkit-search`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: portable.
- **B. Responsibility / API**: Search query language: lexer, AST, SQL/FTS5 predicate compiler, snippets, filename index (feature `filename-index`). API: `ast`, `sql::Compiled`, `terms::Query`.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 8; `--no-default-features` 0.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rusqlite` 0.40.1 no-default | normal/opt | src/sql.rs:18, src/sql.rs:19 | OPTIONAL (default-on feature sqlite) | sub 8 / excl 8 | 8 crates [HO·V] |
| `rusqlite` 0.40.1 +bundled | dev | src/sql.rs:18 | OPTIONAL (default-on feature sqlite) | n/a (dev) | 8 crates [HO·V] |
| `tempfile` 3 | dev | tests/filename_index.rs:32 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F27 [HO, VERIFIED, measured]: default = ["sqlite"] (Cargo.toml:13) -> 8 crates (0 with it off). Zero consumers; published 0.1.0 (live).
  - Haiku "fold into rightkit-db" REFUTED: no code overlap (db is connection bootstrap; search is a lexer/compiler with one ToSql impl, sql.rs:18).

### 2.40 `rightkit-secrets`  (coverage: both)

- **A. Identity**: source `0.1.1`; publishable; versions-json registry `0.1.1` / staged `0.1.1`; crates.io live `0.1.1`; platforms: macOS Keychain, Windows Credential Manager; UnavailableBackend elsewhere (Linux backend planned in RK1).
- **B. Responsibility / API**: Per-app OS credential storage (Keychain / Credential Manager) with presence checks, non-interactive guard, DB-key provisioning and migration from older namespaces; bin `rightkit-secrets`. API: `Store`, `Backend`, `migrate_*`, `apple_helper` (lib.rs:18-32).
- **C. Consumers**: RightKit crates: rightkit-oauth(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.0, workspace; cutright =0.1.0; genright =0.1.0; mailright =0.1.0; scraperight =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 19; `--no-default-features` 19.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/migrate.rs:6, src/migrate.rs:9 | NEEDED | sub 7 / excl 3 |  [ND] |
| `serde_json` 1.0.150 | normal | src/apple_helper.rs:9, src/apple_helper.rs:108 | NEEDED | sub 5 / excl 4 |  [ND] |
| `thiserror` 2 | normal | src/error.rs:5, src/error.rs:8 | REPLACEABLE (hand impl) | sub 6 / excl 3 | error.rs:5-14; 3 exclusive [HO·V] |
| `getrandom` 0.3 | normal | src/macos.rs:327, src/store.rs:293 | NEEDED | sub 3 / excl 3 |  [ND] |
| `base64` 0.22 | normal | src/store.rs:5, src/store.rs:294 | REPLACEABLE (inline) | sub 1 / excl 1 | store.rs:5,294; 1 exclusive [HO·V] |
| `zeroize` 1.8 | normal | src/apple_helper.rs:15, src/secret.rs:2 | NEEDED | sub 1 / excl 1 |  [ND] |
| `rightkit-settings` 0.1.0 | normal/opt | src/store.rs:309, src/store.rs:310 | NEEDED | off by default |  [ND] |
| `tempfile` 3 | dev | tests/e2e.rs:42 | dev only | n/a (dev) |   |
| `windows-sys` 0.61 +Win32_Foundation,Win32_Security_Credentials,Win32_Security_Cryptography,Win32_System_Threading | normal/cfg(windows) | src/windows.rs:10, src/windows.rs:14 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - F13 / F17 [HO, VERIFIED, measured]: thiserror (3) and base64 (1) replaceable (19 -> 15).
  - F51 [HO, VERIFIED]: product knowledge in a shared crate (migrate.rs:104 ~/.coderight/<provider>.key; guard variable names store.rs:15-44).
  - `apple_helper` has no cfg (lib.rs:18) so Apple wire code compiles on Windows.
  - Every app pins =0.1.0; current 0.1.1.

### 2.41 `rightkit-service`  (coverage: both)

- **A. Identity**: source `0.1.1`; publishable; versions-json registry `0.1.1` / staged `0.1.1`; crates.io live `0.1.1`; platforms: Unix sockets + flock; Windows named pipes with current-user DACL (transport.rs:191-205).
- **B. Responsibility / API**: Single-instance service hosting and discovery for Right Suite apps (records, takeover, allowlist, JSON-line RPC over a local endpoint). API: `Client`, `Hosted`, `ServiceConfig`, `discover_apps`, `Suite`, `write_atomic`.
- **C. Consumers**: RightKit crates: rightkit-agent-tools(dep), rightkit-automation(dep), rightkit-control(dep/opt), rightkit-jobs(dep), rightkit-qa(dep), rightkit-shell(dep), rightkit-speech-client(dep). Apps (manifest pins; .cache/worktree copies excluded): heardright =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 12; `--no-default-features` 12.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-framed-sidecar` 0.1.0 | normal | src/client.rs:5, src/server.rs:6 | NEEDED | sub 11 / excl 0 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/discovery.rs:11, src/discovery.rs:17 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/client.rs:6, src/error.rs:1 | NEEDED | sub 5 / excl 0 |  [ND] |
| `tempfile` 3 | dev | src/discovery.rs:506 | dev only | n/a (dev) |   |
| `libc` 0.2 | normal/cfg(unix) | src/discovery.rs:288, src/discovery.rs:293 | NEEDED | sub 1 / excl 1 |  [ND] |
| `windows` 0.61 +Win32_Foundation,Win32_Security,Win32_Security_Authorization,Win32_Storage_FileSystem,Win32_System_IO,Win32_System_Pipes... | normal/cfg(windows) | src/discovery.rs:44, src/discovery.rs:46 | NEEDED (Windows); move to 0.62 | not measured (non-host target) | 0.61 [Windows lead] |

- **F. Problems / findings**:
  - F51 [HO, VERIFIED]: KNOWN_APPS hard-codes four product engines (discovery.rs:28-33) though documented as supplied by the product.
  - F35: write_atomic (record.rs:122). F36: raw libc::flock (discovery.rs:288).
  - F39: windows 0.61 (Cargo.toml:23). Consumer: HeardRight runtime =0.1.0.

### 2.42 `rightkit-settings`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `0.1.0` / staged `0.1.0`; crates.io live `0.1.0`; platforms: portable.
- **B. Responsibility / API**: Versioned JSON settings store: migrations, patch API with allowlist, subscriptions, atomic writes, presence-only secret reference. API: `Store<T>`, `Schema`, `atomic_write`, `SecretProbe`.
- **C. Consumers**: RightKit crates: rightkit-secrets(dep/opt). Apps (manifest pins; .cache/worktree copies excluded): genright =0.1.0.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 14; `--no-default-features` 14.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/secret.rs:1, src/secret.rs:5 | NEEDED | sub 7 / excl 3 |  [ND] |
| `serde_json` 1.0.150 | normal | src/store.rs:3, src/store.rs:22 | NEEDED | sub 5 / excl 4 |  [ND] |
| `thiserror` 2 | normal | src/store.rs:17, src/store.rs:19 | REPLACEABLE (hand impl) | sub 6 / excl 3 | store.rs:17-23; 3 exclusive [HO·V] |
| `tempfile` 3 | dev | tests/settings.rs:39 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F13: thiserror (3 crates) for three variants. F35: atomic.rs:9-20 duplicates fs atomic_write. Consumer: GenRight src-tauri =0.1.0; optional probe in rightkit-secrets.

### 2.43 `rightkit-shell`  (coverage: both)

- **A. Identity**: source `0.1.4`; publishable; versions-json registry `0.1.4` / staged `0.1.4`; crates.io live `0.1.4`; platforms: macOS (objc2), Windows (WinRT toast; wv2-0_38 / wv2-0_39), iOS stubs.
- **B. Responsibility / API**: Shared Tauri desktop shell: webview hardening, window state, menu, tray, single instance, deep links, autostart, rich clipboard, watchdog, engine wiring. API: modules autostart..windows (lib.rs:17-63).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.2; mailright =0.1.2 (opt); viewright =0.1.3.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 253; `--no-default-features` 244.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `tauri` 2 +tray-icon,image-png | normal | src/commands.rs:9, src/commands.rs:16 | NEEDED | sub 242 / excl 191 |  [ND] |
| `rightkit-service` 0.1.0 | normal | src/instance.rs:13, src/lib.rs:113 | NEEDED | sub 12 / excl 0 |  [ND] |
| `rightkit-jobs` 0.2.2 | normal/opt | src/engine.rs:12 | OPTIONAL (feature engine, default-on) | sub 46 / excl 7 | engine.rs:12; 7 exclusive (+2) [HO·V] |
| `rightkit-process` 0.3.0 | normal/opt | src/engine.rs:13 | NEEDED | sub 6 / excl 0 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/autostart.rs:8, src/autostart.rs:14 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/commands.rs:8, src/engine.rs:14 | NEEDED | sub 5 / excl 0 |  [ND] |
| `tempfile` 3 | dev | tests/policy.rs:274 | dev only | n/a (dev) |   |
| `objc2` 0.6 | normal/cfg(target_os = "macos") | src/clipboard.rs:43, src/clipboard.rs:44 | NEEDED | sub 6 / excl 0 |  [ND] |
| `objc2-foundation` 0.3 +NSArray,NSData,NSString,NSURL,NSValue,NSGeometry... | normal/cfg(target_os = "macos") | src/clipboard.rs:46, src/frameless.rs:70 | TRIM (feature list wider than names) | sub 12 / excl 0 | unmeasured [HO·U] |
| `objc2-user-notifications` 0.3 | normal/cfg(target_os = "macos") | src/notify.rs:301 | NEEDED | sub 14 / excl 2 |  [ND] |
| `block2` 0.6 | normal/cfg(target_os = "macos") | src/notify.rs:297 | NEEDED | sub 7 / excl 0 |  [ND] |
| `objc2-app-kit` 0.3 +NSWindow,NSButton,NSControl,NSView,NSResponder,NSPasteboard... | normal/cfg(target_os = "macos") | src/clipboard.rs:45, src/frameless.rs:69 | TRIM (feature list) | sub 20 / excl 0 | unmeasured [HO·U] |
| `tauri-winrt-notification` 0.7 | normal/cfg(windows) | src/notify.rs:472 | NEEDED | not measured (non-host target) |  [ND] |
| `windows_061` 0.61 +Win32_Foundation,Win32_System_Com,Win32_System_DataExchange,Win32_System_Memory,Win32_System_Ole,Win32_UI_Shell | normal/cfg(windows)/opt | src/lib.rs:48 | NEEDED (wv2-0_38 default) | not measured (non-host target) | intentional [Windows lead] |
| `webview2_com_038` 0.38 | normal/cfg(windows)/opt | src/lib.rs:48 | NEEDED | not measured (non-host target) |  [ND] |
| `windows_062` 0.62 +Win32_Foundation,Win32_System_Com,Win32_System_DataExchange,Win32_System_Memory,Win32_System_Ole,Win32_UI_Shell | normal/cfg(windows)/opt | src/lib.rs:50 | OPTIONAL (wv2-0_39) | not measured (non-host target) | intentional [Windows lead] |
| `webview2_com_039` 0.39 | normal/cfg(windows)/opt | src/lib.rs:50 | NEEDED | not measured (non-host target) |  [ND] |

- **F. Problems / findings**:
  - F26 [HO, VERIFIED, measured]: default = ["engine","wv2-0_38"] (Cargo.toml:13) pulls rightkit-jobs/db/bundled SQLite: 253 vs 244 crates (9; Haiku 13). All three app consumers use default-features=false (viewright Cargo.toml:63, coderight apps :51, mailright :111).
  - objc2-foundation / objc2-app-kit feature lists wider than names used (TRIM candidate, unmeasured). Windows dual generation intentional [VERIFIED (Windows lead)].
  - Nested example-app/ has its own Cargo.lock (not a workspace member).
  - Apps pin =0.1.2/0.1.3; source 0.1.4 (live 0.1.4).

### 2.44 `rightkit-speech-client`  (coverage: both)

- **A. Identity**: source `0.3.0`; publishable; versions-json registry `0.3.0` / staged `0.3.0`; crates.io live `0.3.0`; platforms: Unix + Windows.
- **B. Responsibility / API**: Client for the resident speech service: connect, transcribe via service, host planning and takeover. API: `SpeechClient`, `transcribe_via_service`, `plan_host`, `request_takeover`.
- **C. Consumers**: RightKit crates: rightkit-agent-tools(dep/opt), rightkit-asr(dev). Apps (manifest pins; .cache/worktree copies excluded): cutright =0.3.0; heardright =0.3.0, =0.3.0 dev.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 12; `--no-default-features` 12.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `rightkit-service` 0.1.0 | normal | src/lib.rs:25, src/lib.rs:119 | NEEDED | sub 12 / excl 1 |  [ND] |
| `rightkit-framed-sidecar` 0.1.0 | normal | src/lib.rs:24 | NEEDED | sub 11 / excl 0 |  [ND] |
| `serde` 1.0.228 +derive | normal | src/protocol.rs:9, src/protocol.rs:81 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/lib.rs:137, src/lib.rs:186 | NEEDED | sub 5 / excl 0 |  [ND] |
| `tempfile` 3 | dev | tests/contract.rs:72 | dev only | n/a (dev) |   |
| `windows` 0.61 +Win32_Foundation,Win32_Security,Win32_Storage_FileSystem,Win32_System_Pipes | dev/cfg(windows) | src/lib.rs:239 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F51 [HO, VERIFIED]: start_installed_heardright (lib.rs:226) is HeardRight-specific; F37: spawns with raw Command (lib.rs:232-250) instead of OwnedCommand.
  - Consumers: CutRight, HeardRight =0.3.0.

### 2.45 `rightkit-sync`  (coverage: both)

- **A. Identity**: source `0.1.0`; publishable; versions-json registry `-` / staged `0.1.0`; crates.io live `NOT PUBLISHED (404)`; platforms: portable; Windows spawn flags in helper/process.rs.
- **B. Responsibility / API**: Brand-neutral cloud-sync engine (change tokens, tombstones, last-writer-wins, resumable batches, offline queue) plus JSON-lines helper-process backend for the Swift CloudKit helper.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 11; `--no-default-features` 11.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/bin/rightkit-sync-fake-helper.rs:17, src/bin/rightkit-sync-fake-helper.rs:20 | NEEDED | sub 7 / excl 6 |  [ND] |
| `serde_json` 1.0.150 | normal | src/bin/rightkit-sync-fake-helper.rs:18, src/bin/rightkit-sync-fake-helper.rs:85 | NEEDED | sub 5 / excl 4 |  [ND] |

- **F. Problems / findings**:
  - F53 [HO, VERIFIED]: src/bin/rightkit-sync-fake-helper.rs test fixture is an auto-discovered bin target (include lists src/**); raw Command::new in helper/process.rs. Staged 0.1.0, not on crates.io; zero crate consumers but its Swift peer exists (swift/RightKitCloudKitHelper; helper/mod.rs:36).

### 2.46 `rightkit-tauri`  (coverage: both)

- **A. Identity**: source `0.1.1`; publishable; versions-json registry `0.1.1` / staged `0.1.1`; crates.io live `0.1.1`; platforms: portable.
- **B. Responsibility / API**: Framework-agnostic native host services for Right Suite Tauri apps: storage ordering, device identity, signed-entitlement authorization, paid-command guards. NOT dependent on Tauri.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.1; heardright =0.1.1; mailright =0.1.1; scraperight =0.1.1; template =0.1.1; viewright =0.1.1.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 31; `--no-default-features` 31.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `getrandom` =0.2.17 | normal | src/lib.rs:141 | REPLACEABLE (0.3; drop exact pin) | sub 3 / excl 1 | =0.2.17; lib.rs:141 fn value; 1 exclusive [HO·V] |
| `rightkit-license` 0.1.3 | normal | src/lib.rs:8, src/lib.rs:9 | NEEDED | sub 30 / excl 28 |  [ND] |

- **F. Problems / findings**:
  - F18 [HO, VERIFIED]: exact getrandom =0.2.17 (Cargo.toml:13) forces a second getrandom copy into six app lockfiles; used as a fn value (lib.rs:141) so 0.3 needs getrandom::fill. Name mismatch (no tauri dependency).
  - F65: fold-into-shell proposal contested.

### 2.47 `rightkit-transcript`  (coverage: both)

- **A. Identity**: source `0.0.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable.
- **B. Responsibility / API**: Word-timed transcript contract, cue segmentation, SRT/VTT/JSON import/export. API: `Transcript`, `Word`, `build_cues`, `to_srt`, `parse_vtt`, `check_alignment`.
- **C. Consumers**: RightKit crates: rightkit-align(dep), rightkit-tts(dep). Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 14; `--no-default-features` 14.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/contract.rs:10, src/contract.rs:15 | NEEDED | sub 7 / excl 3 |  [ND] |
| `serde_json` 1.0.150 | normal | src/contract.rs:153, src/contract.rs:158 | NEEDED | sub 5 / excl 4 |  [ND] |
| `thiserror` 2 | normal | src/contract.rs:78, src/contract.rs:80 | REPLACEABLE (hand impl) | sub 6 / excl 3 | parse.rs:7-11, contract.rs:78; 3 exclusive [HO·V] |
| `hound` 3.5 | dev | tests/roundtrip.rs:32 | dev only | n/a (dev) |   |
| `rightkit-asr` path | dev | tests/roundtrip.rs:13 | MOVE test out (dev dep) | n/a (dev) | tests/roundtrip.rs:13-14; ~125-145 dev crates [HO·V] |

- **F. Problems / findings**:
  - F45 [HO, VERIFIED]: dev-dependency on rightkit-asr (Cargo.toml:23) only for tests/roundtrip.rs:13-14 (macOS CoreML Parakeet) adds roughly 125-145 dev-only crates. F13: thiserror for two enums.
  - Consumers: rightkit-tts (path) and rightkit-align (path). Plan-owned pipeline crate (RK4).

### 2.48 `rightkit-tts`  (coverage: both)

- **A. Identity**: source `0.0.0`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: macOS say, Windows SAPI, Unix.
- **B. Responsibility / API**: Local speech synthesis: QTS client/worker (feature `engine`, bin rightkit-tts-worker) and OS voice backend (say/SAPI), model manifest + SHA-256 verification, WAV, sentence chunking.
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 27; `--no-default-features` 27.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `serde` 1.0.228 +derive | normal | src/models.rs:7, src/models.rs:74 | NEEDED | sub 7 / excl 0 |  [ND] |
| `serde_json` 1.0.150 | normal | src/client.rs:140, src/client.rs:163 | NEEDED | sub 5 / excl 0 |  [ND] |
| `sha2` 0.10 | normal | src/models.rs:8 | NEEDED | sub 10 / excl 8 |  [ND] |
| `thiserror` 2 | normal | src/system.rs:70, src/system.rs:72 | REPLACEABLE with transcript | sub 6 / excl 0 | 0 alone [HO] |
| `rightkit-process` 0.3.0 | normal | src/client.rs:10 | NEEDED | sub 5 / excl 3 |  [ND] |
| `rightkit-transcript` 0.0.0 | normal | src/system.rs:4, src/system.rs:158 | NEEDED | sub 14 / excl 0 |  [ND] |
| `qts` 0.2.0 | normal/opt | src/client.rs:118, src/client.rs:132 | NEEDED | off by default |  [ND] |
| `libc` 0.2 | normal/cfg(unix) | src/worker.rs:28 | NEEDED | sub 1 / excl 0 |  [ND] |

- **F. Problems / findings**:
  - F34: find_onnxruntime (models.rs:149-153) re-implements ORT_DYLIB_PATH discovery owned by rightkit-ort. F37: 4 raw Commands. F36b: unbounded lines() framing (client.rs:161, worker.rs:82).
  - Zero consumers; `engine` adds 77 crates (almost all rightkit-qts). Plan-owned pipeline crate (RK4 local alignment audio path).

### 2.49 `rightkit-vad`  (coverage: both)

- **A. Identity**: source `0.0.2`; publish=false; versions-json registry `-` / staged `-`; crates.io live `NOT PUBLISHED (404)`; platforms: portable via ort.
- **B. Responsibility / API**: Silero v5 VAD scorer (caller-supplied ONNX), hysteresis/padding/chunking policy, streaming frame carry. API: `Silero`, `FrameScorer`, `scores_to_spans`, `chunk_spans`.
- **C. Consumers**: RightKit crates: rightkit-asr(dep). Apps (manifest pins; .cache/worktree copies excluded): none.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 47; `--no-default-features` 10.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `ort` =2.0.0-rc.13 no-default +std,ndarray,api-24 | normal | src/silero.rs:2, src/silero.rs:90 | TRIM (ndarray) + opt-in download-binaries; route via rightkit-ort | sub 47 / excl 47 | default 47 -> 10 [HO·V] |
| `hound` 3.5 | dev | tests/e2e_silero.rs:70 | dev only | n/a (dev) |   |
| `rightkit-media` 0.2.3 | dev | tests/e2e_silero.rs:17 | dev only | n/a (dev) |   |
| `rightkit-ort` 0.2.0 | dev | tests/e2e_silero.rs:18 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F07 / F08 [HO, VERIFIED, measured]: default = ["ort-defaults"] (Cargo.toml:25) enables download-binaries/tls-native/copy-dylibs/tracing: 47 crates vs 10 with --no-default-features (37 crates; reproduced); `ndarray` feature has zero uses (silero.rs:91-93 use shape tuples).
  - F34: direct ort (Cargo.toml:17); rightkit-ort only dev (:22). Sole consumer: rightkit-asr (path).

### 2.50 `rightkit-watch`  (coverage: both)

- **A. Identity**: source `0.3.1`; publishable; versions-json registry `0.3.1` / staged `0.3.1`; crates.io live `0.3.1`; platforms: portable via notify (FSEvents/inotify/ReadDirectoryChangesW).
- **B. Responsibility / API**: Coalesced typed filesystem watching with rename handling, directory/file-set options, tokio subscription. API: `ChangeCoalescer`, `WatchOptions`, `subscribe`, `TypedWatcher`, `pub use notify` (lib.rs:19).
- **C. Consumers**: RightKit crates: none. Apps (manifest pins; .cache/worktree copies excluded): coderight =0.1.0, =0.3.0, workspace; viewright =0.3.1.
- **E. Totals (measured now, host macOS, normal+build, unique registry crates)**: default features 19; `--no-default-features` 19.
- **D. Direct dependencies** (every Cargo.toml entry; `excl` = registry crates reachable only through that dep, measured on the host default-feature tree; `sub` = full subtree):

| dep (spec) | kind | use sites | verdict | measured | note [source·verification] |
|---|---|---|---|---|---|
| `notify` 8.2.0 | normal | src/coalesce.rs:13, src/coalesce.rs:14 | NEEDED | sub 8 / excl 8 |  [ND] |
| `tokio` 1.53.0 +rt,sync,time,macros | normal | src/subscribe.rs:26, src/subscribe.rs:27 | NEEDED | sub 7 / excl 2 |  [ND] |
| `tracing` 0.1 | normal | src/options.rs:556, src/options.rs:563 | NEEDED (8 use sites) | sub 9 / excl 4 | Haiku "replace with 3 warnings" refuted [HO·R] |
| `tempfile` 3 | dev | src/coalesce.rs:216 | dev only | n/a (dev) |   |
| `tokio` 1.53.0 +rt-multi-thread,macros,time,sync | dev | src/subscribe.rs:26 | dev only | n/a (dev) |   |

- **F. Problems / findings**:
  - F46 [HO, REFUTED]: Haiku b4 "replace tracing (3 warnings)" is wrong: tracing is used at 8 sites (options.rs:556,563; sync.rs:391-392; subscribe.rs:606,619,678,685), several are operational failures with no other channel.
  - F47: `pub use notify` makes notify semver part of this crate's public API. Consumers pin three generations (=0.1.0, =0.3.0, =0.3.1).

## 2B. Merged per-package table: npm (`packages/*`, 29 directories)

Identity facts: source versions are from each `package.json`; "registry" is the live npm registry `dist-tags.latest` fetched 2026-10-08 for the six packages I spot-checked (doc-render 0.1.3, i18n 0.1.0, qa 0.4.0, updates 0.2.5, app-shell 0.2.1, release 0.2.135) plus Haiku's `pnpm view` of all 22 public packages (every `latest` equals the source version). Closure numbers come from Haiku's lockfile walk; I reproduced doc-render (236 total, mermaid 109 exclusive, katex 2 exclusive, shiki 27, remark-gfm 17) exactly. Consumers come from my manifest scan of the 14 app repos plus `rightkit` (app copies under `.cache`/worktrees excluded). Tags: AG agreed, HO Haiku-only, LO Luna-only, CF conflict; V verified, R refuted, U unverifiable.

| Package | Src / state | Responsibility (exports in brief) | Consumers (pins) | Direct deps and peers: verdict (closure) | Problems / findings |
|---|---|---|---|---|---|
| `@rightkit/app-shell` | 0.2.1; public, registry 0.2.1 | App chrome: nav, theme, palette, shortcuts, toasts, layout, UI primitives; `./react`, `./shell.css` | CutRight 0.2.0, GenRight 0.2.1, ScrapeRight 0.2.0, HeardRight 0.1.0, Cockpit 0.2.1; peer of onboarding, settings-ui | peers react `>=18 <20`, platform-ui: NEEDED (0) | React peer `<20` vs `<21` elsewhere (N07); consumers drift (N11). Both inventories |
| `@rightkit/ax` | 0.2.4; public | `right-ax`: AX conformance static/behavioral/adversarial/report, plugin validate | Legion 0.2.4, CodeRight context 0.2.4 (dev) | none (0) | none found. Both |
| `@rightkit/doc-render` | 0.1.3; public | Markdown (unified/remark) + sanitized HTML renderer, Shiki, footnotes, citations | ViewRight 0.1.3 | 12 runtime deps: mermaid UNUSED (113 sub / 109 excl), katex UNUSED (2), remark-math half-wired (5), others NEEDED (236 closure, 125 after removal) | N01, N02. Haiku-only (Luna listed deps, no verdicts) |
| `@rightkit/git` | 0.2.26; public | `right-git`: public-repo workflow install/sync/drift/adopt/uninstall | Legion, CodeRight context 0.2.24 (dev) | none (0) | consumers one patch behind. Both |
| `@rightkit/hooks` | 0.1.2; public | `rightkit-hooks` neutral lifecycle runtime | Legion, CodeRight context 0.1.1 | none (0) | build-control-policy implements hook decisions itself (R09). Both |
| `@rightkit/i18n` | 0.1.0; public (registry 0.1.0) | Catalog merge + translator factory (`createI`, `mergeCatalog`) | none | peer react: NEEDED (0) | N08 zero consumers. Both |
| `@rightkit/legal` | 0.3.1; public | Legal manifest schema, assent receipts | CutRight 0.3.1(dev), ScrapeRight 0.3.0(dev), ViewRight(dev), HeardRight, CodeRight, MailRight(dev), RightSites, SellRight 0.3.1 | none (0) | ScrapeRight drift. Both |
| `@rightkit/legal-ui` | 0.1.3; public | Legal acceptance gate (`LegalGate`) | CutRight, ScrapeRight, ViewRight, HeardRight 0.1.3; peer of onboarding | peer react `<20` (0) | N07. Both |
| `@rightkit/license` | 0.1.8; public | License client, tiers, entitlements | CutRight, ViewRight, HeardRight, CodeRight, MailRight 0.1.8; ScrapeRight 0.1.6 | optional peer plugin-http (0) | drift (ScrapeRight). Both |
| `@rightkit/logs` | 0.1.4; public | Structured log client (`createLogsClient`, command-name constants) | CutRight, ViewRight, HeardRight, CodeRight, MailRight 0.1.4; ScrapeRight 0.1.3; template 0.1.4 | none (0) | N06 command names duplicated in @rightkit/tauri. Both |
| `@rightkit/media-ui` | 0.2.6; public | Media player, image viewer/editor; root + `./image` (needs fabric) | CutRight 0.2.0, GenRight 0.2.6, ScrapeRight 0.2.5, ViewRight 0.2.6 | react-image-crop OPTIONAL (2; only `./image`), peer fabric optional | N04. Haiku-only deps |
| `@rightkit/onboarding` | 0.2.3; public | First-run onboarding (license, models, permissions steps) | CutRight 0.2.0, GenRight 0.2.3, ScrapeRight 0.2.2, HeardRight 0.1.0 | peers app-shell, legal-ui, react: NEEDED | four generations pinned (N11). Both |
| `@rightkit/pdf-ui` | 0.1.2; public | PDF viewer over pdf.js | ViewRight 0.1.2 | pdfjs-dist NEEDED (13) | worker/wasm URLs host-configured. Both |
| `@rightkit/platform-ui` | 0.1.2; public | Platform detect, chord format, window-control adapters; `./react` | CutRight 0.1.1, GenRight 0.1.2, ScrapeRight 0.1.2, Cockpit 0.1.2, template 0.1.2; peer of app-shell | peers plugin-os, react: NEEDED | N09: QA helpers ARE used (genright/src/lib/platform.ts:10,32) |
| `@rightkit/qa-chrome` | 0.1.3; public | Chrome-for-Testing resolve/launch/close, code-sign clone helpers | qa (dep, workspace pin); ScrapeRight 0.1.0, ViewRight 0.1.2, HeardRight 0.1.3 (dev) | none (0) | three dev generations; N10 code-sign helpers ownership unclear |
| `@rightkit/qa` | 0.4.0; public (registry 0.4.0) | QA harness (evidence, frames, guard, loopback, ownership, process, workspace); `./chrome` | no app; devDependency of 7 RightKit packages | qa-chrome (exact pin via workspace:*): NEEDED | N14: no app consumers; exact publish pin |
| `@rightkit/release` | 0.2.135; public | `right-release` build/sign/verify/publish/ownership CLI+SDK | 12 consumers 0.2.109 to 0.2.135 (dev) | smol-toml 1.7.0 exact: NEEDED (1) | drift (N11); `managed-roots.mjs` and ownership helpers are imported by the broker packages by relative path (R03) |
| `@rightkit/settings-ui` | 0.2.7; public | Settings panel, sections, search, hooks | CutRight 0.2.7, GenRight 0.2.7, ScrapeRight 0.2.7, HeardRight 0.1.0 | 7 peers: unbounded `license`, `logs`, `updates` (N07) | Luna correct on HeardRight; Haiku listed 3 consumers |
| `@rightkit/shell` | 0.1.0; public | Typed renderer bridge for native shell commands | CutRight 0.1.0, CodeRight 0.1.0; peer of settings-ui | none (0) | single large file (size unverified) |
| `@rightkit/tauri` | 0.1.1; public | Typed Tauri host adapters (license/log/device commands) + per-app factories | ScrapeRight 0.1.0, ViewRight, HeardRight, CodeRight, MailRight 0.1.1; template | `@rightkit/updates` exact 0.2.4: TRIM -> peer (type-only, index.ts:1) | N03, N06, N15 |
| `@rightkit/theme` | 0.2.1; public | Brand/colour tokens, CSS generation, runtime application; `./node` | CutRight 0.2.0, GenRight 0.2.1, ScrapeRight 0.2.0, HeardRight 0.1.0 | none (0) | HeardRight drift. Both |
| `@rightkit/updates` | 0.2.5; public | Auto-update client, release-status fetch | CutRight 0.2.4; ScrapeRight, ViewRight, HeardRight, CodeRight, MailRight 0.2.5; template 0.2.4; transitively via tauri 0.2.4 | none (0) | N05; Luna's table omitted this package (Haiku only) |
| `@rightkit/agent-room-mcp` | 0.1.0; PRIVATE | Stdio MCP bridge to agent-room-core HTTP server (12 `room_*` tools) | none (root `test` script) | none (0) | A01: stores bearer+resume credentials as plaintext JSON mode 0600 (index.ts:100-112) and re-implements an MCP handler that rightkit-mcp provides |
| `@rightkit/build-control-core` | 0.1.0-phase1; PRIVATE | Protocol constants, policy, storage governance | broker, clients, staging, policy (relative) | none (0) | **-> RustRight**: R03/R06 |
| `@rightkit/build-control-clients` | 0.1.0-phase1; PRIVATE | `rightkit` CLI frontend, managed-agent, storage CLI, remote worker | broker (dep+dev) | core declared and imported by name (index.mjs:2) AND by relative path (storage-cli.mjs:4-5,38) | **-> RustRight**: R03 |
| `@rightkit/build-control-broker` | 0.1.0-phase1; PRIVATE | Local build broker (~85 modules), cutover/bootstrap CLIs, native addons | none | smol-toml 1.7.0 NEEDED; core, clients NEEDED | **-> RustRight**: R04 TOCTOU, R05, R06 |
| `@rightkit/build-control-policy` | no version; PRIVATE | Agent hook decisions, write-time ownership gate, installers | none | none | **-> RustRight**: R03, R05 |
| `@rightkit/build-control-staging` | 0.1.0-phase1; PRIVATE | 11-line stage/sync-back descriptors | none | core declared, unreferenced | **-> RustRight**: R01 |
| `@rightkit/build-control-phase0` | 0.0.0-phase0; PRIVATE | Feasibility evidence harness | none (root scripts only) | ajv dev (5) | **-> RustRight**: R02 |

Luna's npm table: 28 rows (no `updates`), manifest-level only; where it states publication state it follows the stale `rightkit-versions.json` `npm` section (see N13).

## 2C. Swift (`swift/`)

| Item | Merged facts | Findings |
|---|---|---|
| `swift/RightKitSwift` (VERSION 0.3.8, tools 6.0, iOS 17 / macOS 14) | 14 library products = 14 targets, 14 test targets, zero third-party deps (Package.swift:1-35). Consumers: HeardRight (xcodeproj exact 0.3.2; Mobile E2E exact 0.3.2), ScrapeRight AppleCredentials helper exact 0.3.7, RightKitCloudKitHelper (path), ios-app-template. | S01 README drift, S02, S05 |
| `RightKitLicense` (1534 LOC) | Token verify, activation, refresh; Foundation, CryptoKit, Security, Combine, UIKit(guarded) | license-v2 vector copy (F42); `LicenseController: ObservableObject` at LicenseClient.swift:860 (S02) |
| `RightKitDiagnostics` (150) | Bounded JSONL diagnostics + privacy redactor | parallel to rightkit-logs (judgement) |
| `RightKitArtifacts` (259) | Verified staged artifact install/rollback; links system zlib | no app pin confirmed |
| `RightKitPurchases` (874) | StoreKit 2 ledger; depends on RightKitLicense | |
| `RightKitCloudSync` (2186) | CloudKit sync engine; paired with rightkit-sync (Rust) via helper protocol | intentional dual implementation |
| `RightKitStore` (506) | App Group config, leases, locks, revisioned state | |
| `RightKitImportQueue` (432) | Share-extension import queue; depends on Store | |
| `RightKitLiveActivity` (177), `RightKitWatchBridge` (247), `RightKitIntents` (139), `RightKitUI` (143) | as named | |
| `RightKitSpeech` (3915; resource whisper_mel_128.bin) | Apple Speech + Parakeet/Whisper CoreML + VAD; WhisperMelSpectrogram.swift, ParakeetBeamSearch.swift, VoiceActivityDetector.swift mirror Rust asr/vad algorithms | S05 intentional dual-runtime; share fixtures not code (judgement) |
| `RightKitAppleCredentials` (373) | Keychain/LocalAuthentication backend | parallel to rightkit-secrets (G13) |
| `RightKitOverlay` (166) | Overlay placement (AppKit under #if) | duplicates rightkit-overlay Rust (F62) |
| `swift/RightKitCloudKitHelper` | Executable `rightkit-cloudkit-helper`, line-JSON `rightkit-sync-helper/1`, path dep on RightKitSwift (not tag) | covered by Haiku only |
| `swift/ios-app-template`, `swift/scraperight-ios-spike`, `swift/scripts/archive-package.mjs`, `swift/contracts/` | Template (project.yml consumes products), spike (not a package), second distribution script (S03: referenced only by its own test), cross-language golden vector test (`test:swift-contracts`, package.json) | S03, F42 |

## 2D. `ownership-approval/`, `contracts/`, `scripts/`, templates and other trees

| Tree | Merged facts | Coverage | Findings |
|---|---|---|---|
| `ownership-approval/` (no package.json) | Node enrollment/trust tooling (`install-trust.mjs` 195, `prepare-enrollment.mjs`, `trust-schema.mjs` 164), adapters (private-local 330, public-check-producer 294, verifier, receipt), conformance (native-source-conformance.test.mjs reading Swift+C# sources, v2-vectors.json), native signers (macos/ownership-approval.swift 1046, windows/OwnershipApproval.cs 697, install-class1-service.ps1), `migrations/drafts` (102 unsigned drafts across 8 repos; counts by capability: ort 25, secrets 17, shell 12, db 12, mcp 9, process 8, oauth 8, sandbox 4, browser 3, watch 2, embed 2) | both (Haiku detailed) | O01: no script runs its tests (VERIFIED: root package.json and packages/release/package.json contain no reference); O02 [HO, VERIFIED]: `trust-schema.mjs:52,56` sha256/canonicalJson duplicate release helpers (ownership-approval.mjs:76,81; direct-bootstrap.mjs:14; ownership-key-log.mjs:10; ownership-report.mjs:21); 30 non-test createHash('sha256') sites across build-control-* and ownership-approval (Haiku 35); O03: two native signers share one event model guarded only by the conformance test |
| `contracts/` (6 JSON Schemas, fixtures) | content-brief, daemon-api, delivery-envelope, demo-plan, record-event, record-session v1; consumed only by `crates/rightkit-contracts` (include_str!) | both | Plan RK0 output; untracked at snapshot |
| `scripts/` | `generate-index.mjs` (INDEX.md), `install-right-ctl.sh` (builds+signs right-ctl; root overridable by RIGHTKIT_MANAGED_ROOT, so Haiku "hard-coded" is PARTIAL: R06), `run-ui-tests.sh` | both | F60 INDEX stale |
| `desktop-app-template/` | Tauri template (own Cargo workspace + pnpm workspace; 4 crates src-tauri, protocol, engine, tools) with exact pins `rightkit-process =0.2.1`, `rightkit-logs =0.2.0`, `rightkit-tauri =0.1.1`, `rightkit-framed-sidecar =0.1.0`, `@rightkit/updates 0.2.4`, `@rightkit/release 0.2.119` | **neither** (inventoried here) | N17 stale pins (process 0.2.1 vs 0.3.1; release 0.2.119 vs 0.2.135; updates 0.2.4 vs 0.2.5) and stale `placeholders.json` blockers; 96 MB node_modules present; consumed by nobody (copied to start apps) |
| `spikes/rk2-macos/` (untracked) | Content-pipeline spike RK2-A: fixture Tauri app depending on rightkit-control (path), svc-call crate (rightkit-service path), helper, evidence streams, run.py/analyze.py; not a workspace member (workspace is `crates/*`) | **neither** | Plan-owned (RK2); no action |
| `characterization/` (asr 222 lines, media 247, ocr 330, phase5-adoption 200) | Node characterization harnesses (parity contracts, fixtures, evidence JSON) that spawn RightKit binaries; imports are Node built-ins only (zero third-party deps); consumers none | **neither** | N16: not referenced by any root script |
| `infrastructure/` (branded-installs worker 260 lines + wrangler.toml for legion/membrane install hosts bound to R2 `rightapps-downloads`; release-preflight 930 lines) | Cloudflare worker + repin tooling, release preflight; Node built-ins only, no package.json | **neither** | N16: tests not wired in root scripts; Cloudflare/R2 behaviour is credential-bound and not inspected |
| `release-evidence/`, `legal/`, `docs/`, `tasks/`, `crates/rightkit-shell/example-app`, `crates/rightkit-control/examples/control-test-app` | Evidence JSON, legal templates, docs/plans, handoffs, nested example apps | **neither** (listed only) | `release-evidence/gate/cargo-rightkit-{db,fs,service}.json` are modified in the working tree (git status); example apps have their own Cargo.lock |

## 3. Findings matrix

Class: **AGREED** (both inventories), **HAIKU-ONLY**, **LUNA-ONLY**, **CONFLICT** (inventories disagree, or two Haiku batches disagree). Verification (mandatory for every non-AGREED row, and added to AGREED rows where I checked): **VERIFIED**, **REFUTED** (evidence given), **UNVERIFIABLE** (reason given). "Measured" counts are registry crates (name@version) that disappear from the host (macOS) normal+build tree, from my `rightkit cargo tree` runs of 2026-10-08 (815-package lock); Windows-only effects are never measured here (no Windows target in the broker) and are flagged. Route: **RK** = RightKit action, **RR** = moves to RustRight, **PLAN** = content-pipeline owned, **DEC** = owner decision, **NONE** = no action.

### 3.1 Unused dependencies and feature trims (Rust)

| ID | Finding | Scope | Class | Verification | Evidence (file:line) | Route |
|---|---|---|---|---|---|---|
| F01 | `eyre` declared, never used | rightkit-asr | AGREED (Haiku b1 UNUSED; Luna "candidate") | VERIFIED (systematic scan of all 50 crates: 0 refs in src/tests/build.rs/examples); measured 2 crates | asr/Cargo.toml:28 | RK |
| F02 | `windows-implement` direct dep unreferenced; `windows-core` must stay | rightkit-audio | AGREED for windows-implement; CONFLICT for windows-core (Luna: candidate unused) | windows-implement VERIFIED static (build UNVERIFIABLE: needs Windows host); Luna's windows-core REFUTED | audio/Cargo.toml:43-44; lifecycle.rs:161,198; windows-implement-0.60.2/src/gen.rs:72,85,145,183 emit `::windows_core::` | RK (confirm on Windows) |
| F03 | `serde`, `serde_json` declared, unused | rightkit-playback | AGREED | VERIFIED (0 refs in src/tests); 0 crates (shared) | playback/Cargo.toml:22-23 | RK |
| F04 | dev `serde_json` unused (Luna called it a runtime dep) | rightkit-overlay | AGREED | VERIFIED (it is `[dev-dependencies]`; tests/ empty; src has no use) | overlay/Cargo.toml:19 | RK |
| F05 | optional `axum`/`tokio` used only by an example | rightkit-oauth | HAIKU-ONLY | VERIFIED (features server-example; 0 src refs) ; 0 crates in consumer trees | oauth/Cargo.toml:24,53-54 | RK (hygiene) |
| F06 | `rightkit-framed-sidecar` unused in qa | rightkit-qa | CONFLICT (Haiku b3 UNUSED vs Luna lists it) | **REFUTED**: it is used | qa/src/engine.rs:6; qa/Cargo.toml:19 | NONE |
| F07 | `ort-defaults` (default feature) turns on `ort/download-binaries`, `tls-native`, `copy-dylibs`: build-time network fetch; vad pulls it into asr | rightkit-vad, rightkit-asr | CONFLICT (Haiku b1+b4 YES; Haiku b3 "not enabled anywhere") | b1/b4 **VERIFIED**; b3 **REFUTED**. vad default 47 vs `--no-default-features` 10 (37 crates, reproduced); asr tree identical with and without its own defaults (163 lines) because vad defaults flow in; effective asr saving 31 crates | vad/Cargo.toml:25,29; asr/Cargo.toml:37,51,56; trees under scratchpad | RK |
| F08 | ort `ndarray` unused in vad; ort `ndarray` + `tracing` unused in qts | vad, qts | HAIKU-ONLY | VERIFIED static (no `ndarray`/`tracing` identifier; tuple inputs silero.rs:91-93; qts uses `try_extract_tensor` only); qts removal measured 6 crates; build not run | vad/Cargo.toml:17; qts/Cargo.toml:47; vocoder.rs:685,687 | RK |
| F09 | objc2-foundation features NSValue/NSGeometry and Windows features possibly unused | rightkit-ocr | HAIKU-ONLY | names unreferenced VERIFIED (vision.rs:33; win_native.rs:26-30); removal safety **UNVERIFIABLE** (objc2-vision/WinRT may need them transitively; Windows build required) | ocr/Cargo.toml:30,43 | RK (low value, 0 crates) |
| F10 | nix `signal` feature unused | rightkit-process | HAIKU-ONLY | **REFUTED**: `nix::sys::signal::kill`, `nix::unistd::Pid`, `nix::errno::Errno` at owned.rs:692-693 | process/Cargo.toml:15; owned.rs:692 | NONE (see F11) |
| F11 | nix used for one `fcntl`/`kill` or just the `nix::libc` re-export: replace by direct `libc` | rightkit-fs (lock.rs:254-256), rightkit-jobs (runner.rs:60-62), rightkit-process (libc re-export + one `kill`) | HAIKU-ONLY (fs, jobs: b2); process extension is mine | VERIFIED; measured nix = 4 exclusive in fs, 5 in process; nix 0.28 stays via portable-pty (F43); POSIX record-lock semantics are deliberate (runner.rs:63-66) so rewrite needs care | fs/Cargo.toml:25; jobs/Cargo.toml:26; process/Cargo.toml:15 | RK |
| F12 | `serde`/`serde_json` unconditional in rightkit-fs; rightkit-db never needs them | rightkit-fs -> rightkit-db | HAIKU-ONLY (b1 and b2 agree; Luna silent) | VERIFIED: uses identity.rs:22,247,260; lock.rs:18,137,379; bin/rightkit-clonefile.rs:18,27 only; db tree 30 -> 23 (7; b1 measured 8) | fs/Cargo.toml:19-20; db/Cargo.toml:29 | RK |
| F13 | `thiserror` -> hand-written impls | fs (3 enums), settings, secrets, transcript, tts, compose | HAIKU-ONLY | VERIFIED; 3 exclusive crates each for fs/settings/secrets/transcript; saving is realized only in consumers that have no other thiserror-2/syn-3 path | settings/store.rs:17-23; secrets/error.rs:5-14; transcript/parse.rs:7-11 | RK (low) |
| F14 | `uuid` -> `getrandom` + hex | rightkit-jobs, rightkit-qa | HAIKU-ONLY (jobs) | VERIFIED; 2 exclusive each | jobs/store.rs:438,1191,1205; qa/util.rs:56 | RK (low) |
| F15 | logs: chrono -> time (3), getrandom 0.4 -> 0.3 (1), objc2-foundation -> feature `thermal` (6, macOS) | rightkit-logs | HAIKU-ONLY | VERIFIED; measured | logs/Cargo.toml:23,27,30; collect.rs:3,261; metrics.rs:201 | RK |
| F16 | `gzip` default feature of rightkit-http: llm/oauth could use default-features=false | http, llm, oauth | HAIKU-ONLY | VERIFIED; http 49 -> 44 | http/Cargo.toml:15-17; llm/Cargo.toml:15; oauth/Cargo.toml:39 | RK |
| F17 | base64 -> small decoder/encoder | license (1), secrets (1), models rightapps (1), oauth server (0) | HAIKU-ONLY | VERIFIED (excl 1 each); 0.22/0.23/0.21/0.13 coexist in the lock | license/Cargo.toml:16; secrets/Cargo.toml:27 | RK (low) |
| F18 | exact `getrandom =0.2.17` forces a second copy into app lockfiles | rightkit-tauri | HAIKU-ONLY | VERIFIED; 1 crate; code change `getrandom::getrandom` -> `fill` (lib.rs:141) | tauri/Cargo.toml:13 | RK |
| F19 | models: zip (10), tar (6), flate2, dirs (3), chrono (3), base64 not optional/replaceable | rightkit-models | HAIKU-ONLY | VERIFIED measured; consumers (CutRight, GenRight) use the default set so the optional split needs their opt-in | models/Cargo.toml:26,29,31-35 | RK |
| F20 | sha2 0.10 vs 0.11 split; direction | cache, models, pdf (0.11) vs 11 others (0.10) | CONFLICT (Haiku b1/b2: move 0.11 -> 0.10; Haiku b3: align everything to 0.11) | b1/b2 **VERIFIED**, b3 **REFUTED**: ed25519-dalek 2.2.0, tauri-codegen and wry pin sha2 0.10.9 (Cargo.lock), so 0.10 cannot disappear; lopdf 0.44 pulls 0.11 (lopdf 0.42 pulls 0.10), so pdf cannot fully leave 0.11. Moving cache+models to 0.10 saves 8 crates each | cache/Cargo.toml:13; models/Cargo.toml:29; pdf/Cargo.toml:35 | RK |
| F21 | exact pins `sha2 =0.10.9` (media, playback), `parking_lot =0.12.5` | media, playback | AGREED (Luna lists the exact pins; Haiku flags) | VERIFIED | media/Cargo.toml:18; playback/Cargo.toml:21,24 | RK (low) |
| F22 | agent-room-core: split HTTP host behind `http` feature (axum 29 + tokio-stream 3), `rand` -> `getrandom` (6), icacls duplicate | agent-room-core | HAIKU-ONLY | VERIFIED measured; zero consumers today | agent-room-core/Cargo.toml:10,11,17; auth.rs:205; runtime.rs:57,82 | RK |
| F23 | `browser` default feature = 104 exclusive crates | rightkit-agent-tools | HAIKU-ONLY (Luna: "gated appropriately") | VERIFIED default; BUT right-ctl needs it (install-right-ctl.sh:10) and there is no library consumer; Haiku's "bin fails to compile" **REFUTED** (right-ctl.rs:89,103 gated); description names a non-dependency (Cargo.toml:6) | agent-tools/Cargo.toml:12 | RK (doc fix only unless a lib consumer appears) |
| F24 | audio `capture` default (cpal/rubato/ringbuf: 23) | rightkit-audio | HAIKU-ONLY | VERIFIED 64 -> 41; zero consumers | audio/Cargo.toml:15-17 | RK |
| F25 | crash `zip` -> feature `support-bundle` (16) | rightkit-crash | HAIKU-ONLY | VERIFIED; zero consumers | crash/Cargo.toml:26; archive.rs | RK |
| F26 | shell `engine` default pulls jobs/db/bundled SQLite | rightkit-shell | HAIKU-ONLY | VERIFIED 253 -> 244 (9; Haiku 13); all 3 consumers already default-features=false | shell/Cargo.toml:13 | RK |
| F27 | search `sqlite` default (8 crates) | rightkit-search | HAIKU-ONLY | VERIFIED 8 -> 0 | search/Cargo.toml:13 | RK |
| F28 | mcp carries two HTTP stacks (ureq + reqwest/tokio/futures-util) | rightkit-mcp (+ http, models, logs: F41) | AGREED (Luna: "assess"; Haiku: remove) | VERIFIED; reqwest exclusive 25 (Haiku 30); high risk (streaming semantics) | mcp/Cargo.toml:19-22; client.rs:251,774; advanced_http.rs:8,53,295 | RK |
| F29 | **Security**: RNG error discarded; default loopback bearer value is all-zero if `getrandom::fill` fails | rightkit-mcp | HAIKU-ONLY | VERIFIED | mcp/src/server.rs:249-253 (`let _ =`), :934 | RK (fail closed) |
| F30 | drop bindgen via pregenerated bindings for darwin/linux (24 build crates, libclang requirement) | rightkit-qts-ggml-sys | HAIKU-ONLY | VERIFIED; measured | build.rs:226-256 (only Windows file exists) | RK |
| F31 | build.rs probes home-dir/Homebrew/Python-wheel `cmake` and sets CMAKE | rightkit-qts-ggml-sys | HAIKU-ONLY | VERIFIED (only when CMAKE unset) | build.rs:281-309 | RK (review) |
| F32 | prost-build 0.13 (build) vs prost 0.14; PINNED.md ort rc.12 stale; vendored protoc executed in build | rightkit-qts | HAIKU-ONLY | VERIFIED (lock has both prost versions; PINNED.md:8) | qts/Cargo.toml:46,51; build.rs:10-12 | RK (low) |
| F33 | `rightkit-qts-ggml` is a 59-line wrapper nobody uses beyond `sys` | qts-ggml | AGREED (Luna "retain if boundary else merge") | VERIFIED | qts-ggml/src/lib.rs:3,8-35; qts/pipeline/backend.rs:7; genright-qts/Cargo.toml:14 | DEC -> RK |
| F44 | pdf: `fontdb` (4) -> feature; `pdf-extract` brings lopdf 0.42 + RustCrypto duplicates (24 when enabled) | rightkit-pdf | HAIKU-ONLY | VERIFIED (lock: lopdf 0.42 + 0.44) | pdf/Cargo.toml; font.rs:15,64 | RK (low) |
| F45 | transcript's dev-dependency on rightkit-asr (~125-145 dev crates) for one macOS test | rightkit-transcript | HAIKU-ONLY | VERIFIED | transcript/Cargo.toml:23; tests/roundtrip.rs:13-14 | RK (plan-aware: move test into align or asr) |
| F46 | watch: replace `tracing` with the 3 warnings | rightkit-watch | HAIKU-ONLY | **REFUTED**: 8 `warn!` sites | watch/src/options.rs:556,563; sync.rs:391-392; subscribe.rs:606,619,678,685 | NONE |
| F47 | `pub use notify` ties public API to notify semver | rightkit-watch | HAIKU-ONLY | VERIFIED (design) | watch/src/lib.rs:19 | DEC |
| F48 | db `hooks` feature "has one cfg gate" | rightkit-db | HAIKU-ONLY | **REFUTED**: forwarding feature only (db/Cargo.toml:25), no cfg in src | db/src (grep) | NONE |
| F49 | embed: vector store bypasses rightkit-db; no `bundled` passthrough; `embedder` feature split (tokenizers 58) | rightkit-embed | HAIKU-ONLY | VERIFIED; the "search overlaps db" companion claim is **REFUTED** | embed/Cargo.toml:15,21; vector.rs:1 | RK |
| F50 | `ConductorStableKey` product leakage | rightkit-cache | HAIKU-ONLY | VERIFIED | cache/src/prefix.rs:61,66 | RK (low) |
| F51 | product knowledge in shared crates (secrets, service, speech-client) | secrets, service, speech-client | HAIKU-ONLY | VERIFIED | secrets/migrate.rs:104; service/discovery.rs:28-33; speech-client/lib.rs:226 | RK (low) |
| F53 | sync fake-helper test fixture is a shipped bin; raw spawn | rightkit-sync | HAIKU-ONLY | VERIFIED | sync/src/bin/rightkit-sync-fake-helper.rs; helper/process.rs | RK (low) |
| F54 | stray tracked files: `crates/rightkit-control/registration.patch`, `crates/rightkit-license/Cargo.lock` | control, license | HAIKU-ONLY | VERIFIED (`git ls-files`) | - | RK (delete) |

### 3.2 Duplicate responsibilities and merges

| ID | Finding | Scope | Class | Verification | Evidence | Route |
|---|---|---|---|---|---|---|
| F34 | ONNX Runtime ownership: five crates declare `ort =2.0.0-rc.13` directly and call `Session::builder`; rightkit-ort is only a dev-dep of vad/asr; tts re-implements ORT_DYLIB_PATH discovery | vad, asr, ocr, qts, embed, tts -> rightkit-ort | AGREED | VERIFIED; app drafts show 25 `ort` adoption drafts | vad/Cargo.toml:17; asr:32; ocr:23,41; qts:47; embed:13; tts/models.rs:149-153 | RK |
| F35 | Atomic-write copies: fs/atomic.rs:51 (owner) + logs/atomic.rs:14, models/store.rs:981, settings/atomic.rs:11, service/record.rs:122, pdf/commit.rs:23, shell/recovery.rs:146, cache/disk.rs:252, agent-room-core/store.rs:234,316, automation/redact.rs:100, control/lease_store.rs:284, crash/hook.rs:122, asr/whisper/coreml/bridge.rs:234 | many | AGREED (Luna: fs/settings/cache; Haiku: 8) - my count 13 | VERIFIED; only rightkit-db depends on rightkit-fs; consolidate after F12/F11 | see Evidence | RK |
| F36 | Lock copies: fs/lock.rs (POSIX record), service/discovery.rs:277-288 (BSD flock), jobs/runner.rs:59-82 (record lock by design), settings/store.rs:143, models/lock.rs:93,157, models/suite/lease.rs:239, suite/refs.rs:72 (std `File::try_lock`) | many | HAIKU-ONLY | VERIFIED duplicates, but **semantics differ deliberately** (runner.rs:63-66) so a blind merge is unsafe | see Evidence | DEC -> RK |
| F36b | Hand-rolled unbounded JSON-lines framing: tts client.rs:161/worker.rs:82, sync helper/process.rs:125, mcp client.rs; (media listed by Haiku: REFUTED, probe.rs:121,154 parse ffmpeg stderr) | tts, sync, mcp | HAIKU-ONLY | VERIFIED (partly REFUTED for media) | see Evidence | RK (low) |
| F37 | Raw `Command::new` outside rightkit-process: agent-room-core 11, qa 20, models 7, control 5, tts 4, shell 6, automation 3, sandbox 3, service 3, crash 2, sync/secrets/fs/ocr/mcp/speech-client/audio/computer 1 each | many | AGREED (Luna lists process/sandbox/browser/computer; Haiku 24 sites) | VERIFIED counts (my grep) | grep of `Command::new` per crate | RK (incremental) |
| F38 | Windows job objects implemented 3x: process/owned.rs:800 + process_windows.rs:68 (owner), sandbox/windows_job.rs:7-34, automation/win/sys.rs:579-601 | process, sandbox, automation | HAIKU-ONLY | VERIFIED + **VERIFIED (Windows lead)** | see Evidence | RK (needs Windows build) |
| F39 | `windows` crate generations: 0.61 in automation, hotkeys, process, qa, sandbox, service, speech-client (7); 0.62 in audio, ocr; control and shell carry both on purpose (wv2-0_38/wv2-0_39) | 9 crates | CONFLICT (Haiku b1/b3: "align windows/webview2 across control/shell/...", Luna: intentional for control/shell) | **VERIFIED (Windows lead)**: control/shell dual stays until apps drop webview2-com 0.38; real cleanup = move the seven 0.61 crates to 0.62 together with F38; I confirmed the manifest lines | automation/Cargo.toml:23; hotkeys:17; process:18; qa:42; sandbox:20; service:23; speech-client:23; audio:35; ocr:42; control:29,31; shell:43,52 | RK (Windows build) |
| F40 | hotkeys `windows` -> `windows-sys` 0.61 (~10 crates, Windows, estimated) | rightkit-hotkeys | HAIKU-ONLY | **VERIFIED (Windows lead)**: plain Win32 only, drop-in; source read windows/mod.rs:12-19 | hotkeys/Cargo.toml:17 | RK |
| F41 | HTTP clients bypass rightkit-http: logs/upload.rs:81,88; models/download.rs:184 + rightapps.rs:184; mcp/client.rs:774 (+ reqwest, F28) | logs, models, mcp | AGREED | VERIFIED | see Evidence | RK (medium) |
| F42 | `license-v2.json` golden vector: 4 byte-identical copies | swift/contracts, rightkit-license, packages/license, RightKitSwift/Tests | HAIKU-ONLY | VERIFIED (md5 085249124998683b7739e5b20fb43aba x4) | paths in 2.D | RK (low) |
| F43 | portable-pty 0.9 carries nix 0.28, cfg_aliases 0.1, thiserror 1, bitflags 1 (15 exclusive) | rightkit-sandbox | HAIKU-ONLY | VERIFIED (Cargo.lock: portable-pty -> nix 0.28.0); upgrade depends on upstream | sandbox/Cargo.toml:14 | RK (wait upstream) |
| L01 | SHA-256 is implemented per crate for distinct purposes (cache key, embed digest, auth hash); keep domain-level hashes local rather than build one shared API | cache, embed, agent-room-core, others | LUNA-ONLY | VERIFIED as observation: 14 crates depend on sha2 (cache/key.rs:5, embed/embedder.rs:9, agent-room-core/auth.rs:4); no shared canonicalisation contract exists, so no action; the only actionable aspect is the version split (F20) | Cargo.toml sha2 lines (section 6.2) | NONE |
| F52 | windows-sys 0.45/0.52/0.59/0.60.2/0.61.2 in the lock; RightKit crates (crash, logs, ort, overlay, secrets) all on 0.61, the rest are third-party transitive | lock | AGREED (listed by Haiku b1/b2/b4 and Luna) | **VERIFIED (Windows lead)** | crash:29; logs:34; ort:45; overlay:13; secrets:32 | NONE |
| F55 | App pin drift (see 6.3): process 4 generations, ort 4, db 3, secrets all =0.1.0 vs 0.1.1, sandbox 0.2.4 vs 0.3.1, qa 0.2.12 vs 0.2.13, control 0.1.6 (Cockpit), watch 3 generations | apps | AGREED | VERIFIED (my manifest scan) | cons scan | RK (app-owner coordination) |
| F57 | `[workspace.dependencies]` has only serde/serde_json; only agent-tools, overlay, pdf inherit | workspace | AGREED | VERIFIED (3 manifests use `workspace = true`) | Cargo.toml:7-9 | RK (low) |
| F58 | `[profile.dev.package."*"]` comment says 275 crates; lock has 815 | workspace | HAIKU-ONLY | VERIFIED | Cargo.toml (comment above `debug = false`) | RK (trivial) |
| F59 | Cargo.lock multi-version crates | workspace | AGREED | VERIFIED (reproduced: 815 packages, 77 names, 176 entries); groups in 6.2 | Cargo.lock | RK |
| F60 | `INDEX.md` stale (47 crates; versions old; "crates.io" column = publishable, not published) | INDEX | AGREED | VERIFIED (overlay/hotkeys say crates.io but 404 on crates.io) | INDEX.md:7,23,35 | RK (regenerate) |

### 3.3 Publication state, removals and merges

| ID | Finding | Scope | Class | Verification | Evidence | Route |
|---|---|---|---|---|---|---|
| F56 | Publication-state conflicts. (a) automation + agent-room-core are `publish = false` yet in `stagedCargo`; (b) hotkeys: Luna "published", Haiku "unpublished"; (c) llm/overlay: Luna "staged"; (d) playback: Haiku "conflict with CutRight comment"; (e) versions JSON `registry` stale for shell/sandbox; (f) Luna: npm "nine published" | many | CONFLICT | (a) VERIFIED. (b) Luna **REFUTED** (crates.io 404). (c) Luna **REFUTED** (absent from stagedCargo; crates.io 404). (d) published (crates.io 0.1.1): CutRight comment stale. (e) VERIFIED: crates.io shell 0.1.4, sandbox 0.3.1 vs JSON 0.1.3/0.3.0. (f) see N13 | live crates.io API 2026-10-08; versions JSON | RK |
| F61 | Remove `rightkit-computer` | computer | AGREED | VERIFIED: zero consumers; README "folded into automation" | computer/README.md; cons scan | RK |
| F62 | Remove/archive `rightkit-overlay` (Rust) vs adopt | overlay | HAIKU-ONLY | zero consumers VERIFIED; Swift duplicate VERIFIED; adoption intent **UNVERIFIABLE** (README names HeardRight/Cockpit hosts) | overlay/README.md; Overlay.swift:97-124 | DEC |
| F63 | Merge `rightkit-align` into transcript; delete `rightkit-compose` | align, compose | HAIKU-ONLY | **REFUTED by plan context** (RK4 and RK9 are new content-pipeline packages; consumers do not exist yet by design) | docs/plans/2026-10-08-content-pipeline-execution-plan.md:26-34 | PLAN |
| F64 | Zero-consumer crates called removal candidates by Haiku: cache, search, hotkeys, playback, tts(+ggml), llm, audio, pdf, ocr, sync, crash; Luna recommends none | those | CONFLICT | zero manifest consumers VERIFIED for all (my scan; excluding crates with consumers that Haiku also mislabelled: db, embed, control). Disposition table in 6.4: only computer is remove-now; pdf/ocr are adoption-pending (ViewRight keeps crates/pdf 4,586 lines and crates/ocr 755 lines); tts/asr/vad/transcript/align are plan RK4; the rest are owner decisions | cons scan; viewright/src-tauri/Cargo.toml:2 | DEC |
| F65 | Fold rightkit-tauri into rightkit-shell and @rightkit/tauri into @rightkit/shell | tauri, shell | HAIKU-ONLY | premise (no tauri dep, 898 lines, depends only on license) VERIFIED; the fold is a design call: shell is 191-crate-heavy and ScrapeRight/HeardRight use tauri but not shell | tauri/Cargo.toml:12-14 | DEC |
| F66 | Haiku b1 section C states "no app manifest depends on" rightkit-db and rightkit-embed, and lists only Cockpit as an app consumer of rightkit-control | db, embed, control | HAIKU-ONLY | **REFUTED**: db has 5 app repos (GenRight, CodeRight, MailRight, ViewRight, ScrapeRight), embed 2 (MailRight, CodeRight), control 3 (CutRight, GenRight, Cockpit) | 6.3 pin table | NONE (correction) |

### 3.4 npm packages

| ID | Finding | Scope | Class | Verification | Evidence | Route |
|---|---|---|---|---|---|---|
| N01 | `mermaid` and `katex` declared in doc-render, zero references; removal 236 -> 125 packages (111) | @rightkit/doc-render | HAIKU-ONLY | VERIFIED (grep + reproduced closure). Caveat: ViewRight (sole consumer) declares mermaid/katex itself and imports them (viewright/package.json:115-116; MermaidBlock.tsx:15; Math.tsx:14), so ViewRight's own lock keeps them; app-level saving ~0 today | doc-render/package.json:34-35 | RK |
| N02 | remark-math parses math but output is raw text (half-wired) | doc-render | HAIKU-ONLY | VERIFIED | renderers.tsx:71-72; parseMarkdown.ts:5,17 | DEC |
| N03 | `@rightkit/updates` exact dep in @rightkit/tauri is type-only: make a peer; creates second `updates` copy in 5 consumers | @rightkit/tauri | HAIKU-ONLY | VERIFIED | tauri/src/index.ts:1; package.json:19 | RK |
| N04 | `react-image-crop` hard dep used only by `./image` | media-ui | HAIKU-ONLY | VERIFIED | ImageCropTool.tsx:2; package.json:42 | RK (2 packages) |
| N05 | `UPDATE_HOST_CONTRACT_VECTORS` exported from runtime entry, used only in tests | @rightkit/updates | HAIKU-ONLY | VERIFIED (3 test files, no runtime use) | updates/src/index.ts:12; mailright/tests/licenseUpdates.test.ts | RK (low) |
| N06 | Log command names duplicated between @rightkit/tauri and @rightkit/logs | tauri, logs | HAIKU-ONLY | VERIFIED | tauri/src/index.ts:18,155; logs/src/index.ts:1-3 | RK (low) |
| N07 | Peer ranges inconsistent (react `<20` in 5 packages (app-shell, legal-ui, onboarding, platform-ui, settings-ui) vs `<21` in 4 (doc-render, i18n, media-ui, pdf-ui); settings-ui peers license/logs/updates unbounded) | UI packages | HAIKU-ONLY | VERIFIED (package.json peerDependencies) | settings-ui/package.json | RK (low) |
| N08 | `@rightkit/i18n` has no consumers | i18n | AGREED | VERIFIED; published 0.1.0 (registry) | consumer scan | DEC |
| N09 | platform-ui QA helpers are dead weight | platform-ui | HAIKU-ONLY | **REFUTED**: used by GenRight | genright/src/lib/platform.ts:10,32 | NONE |
| N10 | qa-chrome code-sign helpers belong elsewhere | qa-chrome | HAIKU-ONLY | **UNVERIFIABLE** (ownership judgement) | - | DEC |
| N11 | Consumer drift on staged npm versions | apps | AGREED | VERIFIED (2.B column) | consumer scan | RK (app owners) |
| N12 | pnpm-lock multi-version names | workspace | AGREED (Luna 12 names, partial; Haiku 43) | VERIFIED: 539 entries, 471 names, 43 multi-version | pnpm-lock.yaml | RK (low) |
| N13 | `rightkit-versions.json` `npm` section stale (nine entries; doc-render only in stagedNpm) while all 22 public packages are published at source version | release manifest | CONFLICT (Luna reads "nine published"; Haiku: stale) | Haiku **VERIFIED** by live npm registry (doc-render, i18n, qa, updates, app-shell, release: latest = source) ; Luna's inference REFUTED | versions JSON `npm`/`stagedNpm` vs registry | RK |
| N14 | `@rightkit/qa` has no app consumer | qa | AGREED | VERIFIED | consumer scan | NONE |
| N15 | @rightkit/tauri embeds per-app factories for five apps | tauri | HAIKU-ONLY | VERIFIED | tauri/src/index.ts:168,214-218 | DEC |
| N16 | Test wiring: root `test` runs 8 packages; `ownership-approval`, `characterization`, `infrastructure`, app-shell, media-ui, theme, shell, qa-chrome, i18n are not in it | tooling | HAIKU-ONLY (ownership-approval) + mine | VERIFIED (package.json scripts) | package.json:12-35 | RK |
| N17 | desktop-app-template: exact pins are stale (process =0.2.1 vs 0.3.1, release 0.2.119 vs 0.2.135, updates 0.2.4 vs 0.2.5) and `placeholders.json` still lists as unpublished four capabilities that are published (app-shell 0.2.1 and media-ui 0.2.6 on npm; rightkit-jobs 0.2.4 and rightkit-qa on crates.io) with stand-in `src/shell/AppShell.tsx` still present | template | neither inventory (found here) | VERIFIED | template/package.json; src-tauri/Cargo.toml:35; placeholders.json (npm/cargo/harness keys); live registries | RK |
| N18 | `@rightkit/qa` README says the package is "intentionally private until the phase gate passes" but it is published public (publishConfig access public; registry latest 0.4.0, 10 versions) | qa | LUNA-ONLY observation (Luna: "package states private in README"); Haiku lists it public | VERIFIED (both true: README stale) | packages/qa/README.md:7; package.json:40-42; npm registry | RK (doc) |

### 3.5 Private packages (broker; routes to RustRight), Swift, ownership, agent-room-mcp

| ID | Finding | Scope | Class | Verification | Evidence | Route |
|---|---|---|---|---|---|---|
| R01 | `build-control-staging` is an 11-line dead module; declares unused core dependency | staging | HAIKU-ONLY | VERIFIED (no `@rightkit` import in src; no consumers) | staging/package.json; src/index.mjs | **RR** |
| R02 | `build-control-phase0` evidence harness: archive | phase0 | HAIKU-ONLY | consumers VERIFIED none beyond root scripts; "served its purpose" is a judgement (UNVERIFIABLE) | package.json:26-27 | **RR** |
| R03 | Undeclared relative imports from broker packages into `packages/release` (managed-roots.mjs etc.) and between packages | clients, broker, policy | HAIKU-ONLY | VERIFIED (partly: clients declares and imports core by name too, index.mjs:2, so "used only in tests" is REFUTED) | clients/index.mjs:1; broker.mjs:2; policy/install-agent-policy.mjs:2; storage-cli.mjs:4-5 | **RR** |
| R04 | **Security**: native addon verified by reading the file twice, then `require(path)` reads it a third time (TOCTOU) | broker | HAIKU-ONLY | VERIFIED | broker/src/windows-job.mjs:25,26,29; macos-resource.mjs:25,26,29 | **RR** |
| R05 | Byte-identical copies: background-process.mjs (release vs policy); ownership catalog + session catalog + fingerprints (md5 e0b29f29...); rightkit-versions.json x3 (md5 6bf02710...); ownership-availability.mjs | release, clients, policy | HAIKU-ONLY | VERIFIED (diff -q, md5) | listed paths | **RR** (versions JSON owner stays release) |
| R06 | Hard-coded `/Volumes/D` and `D:\` fallbacks | core, broker, install-right-ctl.sh | HAIKU-ONLY | PARTLY REFUTED: install-right-ctl.sh:8 honours RIGHTKIT_MANAGED_ROOT; storage.mjs:12 and cache-proof-cli.mjs:90 are real | cited | **RR** |
| R07 | `build-control-broker/legacy-v1/` is an empty directory | broker | HAIKU-ONLY | VERIFIED | `ls` empty | **RR** |
| R08 | Node primitive duplication (locks G17, atomic write G18, sha256File G21, spawn wrappers G19) across core/clients/broker/release | build-control-* | HAIKU-ONLY | VERIFIED samples (thin-state.mjs:32,43 vs clients/index.mjs:393); full list not re-checked | Haiku crosscut G17-G21 | **RR** |
| R09 | build-control-policy re-implements hook decisions that @rightkit/hooks should host | policy, hooks | HAIKU-ONLY | **UNVERIFIABLE** (design) | - | **RR** |
| R10 | `thin-logger.mjs` duplicates logging | broker | HAIKU-ONLY | UNVERIFIABLE (judgement; 41 lines) | - | **RR** |
| A01 | agent-room-mcp stores bearer + resume credentials as plaintext JSON (mode 0600) and re-implements an MCP handler | agent-room-mcp | HAIKU-ONLY | VERIFIED | agent-room-mcp/src/index.ts:100-112 | RK |
| S01 | `swift/README.md` drift: names `RightKitLicensing`/`RightKitQA` (absent), "16/16" tests (14 test dirs), "published 0.1.0" (VERSION 0.3.8) | swift docs | HAIKU-ONLY | VERIFIED | swift/README.md:3-5,25,28; VERSION | RK (low) |
| S02 | `LicenseController: ObservableObject` contradicts README rule | RightKitLicense | HAIKU-ONLY | VERIFIED | LicenseClient.swift:860; swift/README.md:37 | DEC |
| S03 | `swift/scripts/archive-package.mjs` second distribution path, referenced only by its own test | swift | HAIKU-ONLY | VERIFIED | grep | RK (delete) |
| S04 | RightKitCloudKitHelper uses a path dep instead of a tag | helper | HAIKU-ONLY | VERIFIED (design) | helper Package.swift | NONE |
| S05 | Speech/overlay algorithms duplicated in Rust and Swift | RightKitSpeech/Overlay vs asr/vad/overlay | HAIKU-ONLY | files VERIFIED present; "should not exist" **UNVERIFIABLE** (dual-runtime by design) | WhisperMelSpectrogram.swift etc. | DEC |
| O01 | ownership-approval tests are not run by any script | ownership-approval | HAIKU-ONLY | VERIFIED | root package.json; packages/release/package.json | RK |
| O02 | canonical-JSON/sha256 copies | ownership-approval vs release | HAIKU-ONLY | VERIFIED | trust-schema.mjs:52,56 etc. | RK |

### 3.6 Remaining cross-cutting duplicate-responsibility claims (Haiku G-list and Luna G)

| ID | Claim | Class | Verification | Evidence | Route |
|---|---|---|---|---|---|
| G7 | Archive extraction and member-path validation duplicated (models root vs `suite/`; Swift ArtifactInstaller) | HAIKU-ONLY | VERIFIED inside models: same function names twice; cross-language copy is by design | models/src/archive.rs:128,171 vs suite/archive.rs:181,224; 9,267 lines in crate | RK (with F19/F36) |
| G12 | License verification in Rust, Swift, JS with one golden vector | AGREED (Luna: "keep one wire/token contract") | VERIFIED existence and the 4 vector copies (F42) | rightkit-license/src/lib.rs:303 area; RightKitLicense; packages/license | RK (F42) |
| G13 | Credential storage in rightkit-secrets, Swift AppleCredentials, ownership-approval Keychain service, agent-room-mcp plaintext file | HAIKU-ONLY | existence VERIFIED (Swift RightKitAppleCredentials 373 LOC; agent-room-mcp/src/index.ts:100-112 plaintext); "should be one" is a judgement (UNVERIFIABLE) ; A01 is the actionable part | see A01 | RK (A01) |
| G14 | Redaction/diagnostics in rightkit-logs, @rightkit/logs, Swift Diagnostics, Node thin-logger | AGREED (Luna: "align schema/redaction contracts, retain native APIs") | VERIFIED existence; keep native per language is both inventories' conclusion | rightkit-logs/src/redact.rs; PrivacyRedactor.swift; broker/thin-logger.mjs | NONE / RR (thin-logger R10) |
| G15 | Chrome resolution/launch duplicated, "drop the copy in rightkit-qa" | HAIKU-ONLY | **REFUTED** for qa: qa/src/browser.rs (300 lines) is a thin adapter over rightkit-browser (browser.rs:1-14), not a resolver copy. The cross-language overlap (rightkit-browser/config.rs vs @rightkit/qa-chrome) is UNVERIFIABLE (different runtimes) | qa/src/browser.rs:1-14 | NONE |
| G23 | Hook runtime split between @rightkit/hooks and build-control-policy | HAIKU-ONLY | UNVERIFIABLE (design) | R09 | RR |
| G25 | macOS AX/CGEvent code in automation, control, computer, hotkeys | HAIKU-ONLY | existence VERIFIED (AXUIElement in 3+1+1+2 files; CGEvent in 3+3+1+6); consolidation owner is a judgement; removal of `computer` (F61) removes one copy | grep over crates/*/src | DEC |
| G26 / G27 | PDF and OCR engines exist in RightKit and, privately, in ViewRight | HAIKU-ONLY | VERIFIED: viewright/src-tauri/crates/pdf 4,586 lines, crates/ocr 755 lines, both path members (viewright/src-tauri/Cargo.toml:2,70,73); rightkit-pdf header says it was extracted from ViewRight (lib.rs:1-3) | see Evidence | DEC (F64) |
| G29 | Settings persistence in rightkit-settings, @rightkit/settings-ui, Swift RevisionedStateStore | HAIKU-ONLY | **REFUTED as duplication for settings-ui** (it is the UI port over rightkit-settings: settings-ui/src/types.ts:7,11,53); Swift store is a native peer | settings-ui/src/types.ts | NONE |
| G30 | Two model stacks inside rightkit-models | HAIKU-ONLY | VERIFIED (G7 + lock/store/install pairs) | models/src/lib.rs:19-31 | RK (high risk: live installs) |
| G31 | Cloud sync in Rust (rightkit-sync) and Swift (RightKitCloudSync) | HAIKU-ONLY | VERIFIED intentional (helper protocol `rightkit-sync-helper/1`, helper/mod.rs:1,36) | rightkit-sync/src/helper/mod.rs | NONE |
| G32 | MCP JSON-RPC in rightkit-mcp and agent-room-mcp (TS) | HAIKU-ONLY | VERIFIED existence (agent-room-mcp 237 lines, 12 tools) | A01 | RK (low) |
| G33 / G34 | Job queue/lease and cache concepts duplicated by broker `thin-job-service`/`thin-cache` | HAIKU-ONLY | UNVERIFIABLE (different domains: app jobs vs build jobs) | - | RR |
| L-G | Luna G: licensing, diagnostics, process, atomic/locking, JSONL all "overlap but keep native/layered" | LUNA-ONLY framing, matches G12/G14/G3-5/G1-2/G16 | consistent with the Haiku rows above | - | NONE |

## 4. Corrections for context the inventories lacked

1. **Content-pipeline crates are new and consumer-less by design.** `rightkit-compose` (RK9), `rightkit-align` (RK4) and `rightkit-contracts` (RK0) are specified in `docs/plans/2026-10-08-content-pipeline-execution-plan.md:26-34`; their consumers (CutRight, GenRight, PostRight, the daemon host RK10, `rightkit-record` RK3) do not exist yet. Haiku crosscut section I/H6 ("merge align into transcript", "confirm owner, then delete compose") is therefore dropped (F63). None of the three is listed as a removal candidate anywhere in this document. Their plan rows also change how neighbours must be read: `rightkit-asr`, `rightkit-vad`, `rightkit-transcript`, `rightkit-tts`, `rightkit-qts` carry zero app consumers because RK4 (local alignment route "QTS audio through rightkit-asr") is their consumer; `rightkit-oauth` `server` mode and a `rightkit-secrets` Linux backend are RK1; a server-side HTTPS listener on `rightkit-http` is RK10; `rightkit-record`/`rightkit-qa record: true` are RK3. Findings against those crates are written as hygiene (F01, F07, F34, F45), never as removal.
2. **Broker packages are moving to RustRight.** Everything about `@rightkit/build-control-{core,clients,broker,policy,staging,phase0}`, the `rightkit` CLI (`build-control-clients` bin), `managed-cargo.mjs` and `managed-roots.mjs` is routed **RR** (R01-R10). Two couplings stay visible for the hand-off: (a) the three broker packages import `packages/release/managed-roots.mjs` by relative path (R03), and `scripts/install-right-ctl.sh` plus every `rightkit cargo` command used by RightKit docs need the `rightkit` CLI on PATH; (b) `packages/release/rightkit-versions.json` is copied into clients and policy (R05).
3. **Windows facts from the Windows lead are applied and tagged VERIFIED (Windows lead)**: `windows` 0.61.1 in automation, hotkeys, process, qa, sandbox, service, speech-client; 0.62.2 in audio and ocr; control and shell carry both on purpose via `wv2-0_38`/`wv2-0_39` (webview2-com 0.38 needs windows 0.61; 0.39 needs 0.62), which is NOT duplication to remove until apps drop webview2-com 0.38; the real cleanup is moving the seven 0.61 crates to 0.62 together with consolidating job objects into rightkit-process (F38, F39). windows-sys 0.45/0.52/0.59/0.60.2/0.61.2 are in the lock; RightKit crates (crash, logs, ort, overlay, secrets) are all on 0.61, older ones are third-party transitive (F52); hotkeys `windows` -> `windows-sys` 0.61 is a drop-in (F40). I confirmed each manifest line (F39 evidence) but did not build on Windows.
4. **Corrections to the inventories (each with evidence in the matrix).** Haiku: b3 "download-binaries not enabled anywhere" (F07), b3 "qa framed-sidecar unused" (F06), b3 "process nix `signal` unused" (F10), b3 "align sha2 to 0.11" (F20), b4 "watch tracing replaceable" (F46), b1 "no app consumes db/embed; control only Cockpit" (F66), b1 "right-ctl fails without browser" (F23), b1 "db hooks cfg gate" (F48), npm "platform-ui QA helpers dead" (N09), crosscut "install-right-ctl hard-coded" (R06), "media listed under JSON framing" (F36b), "search overlaps db" (F49). Luna: hotkeys "published" (F56), overlay/llm/watch "staged" (F56), ocr/embed/audio consumers (6.3), `windows-core` unused candidate (F02), overlay `serde_json` called a runtime dependency (F04), npm "nine published" (N13), "28 npm packages" table missing `updates`.

## 5. Ranked action list

Ranking: correctness/security first, then measured dependency removal per unit risk, then hygiene, then owner decisions. "Removed" counts are measured registry crates on the macOS host (Windows effects are estimated and say so). Risk is for the change itself, not for apps. "Found by": H = Haiku, L = Luna, H+L = both, Me = found/extended here.

### 5.1 RightKit actions

| # | Change | Crates / packages affected | Removed (measured unless noted) | Risk | Found by | Matrix |
|---|---|---|---|---|---|---|
| 1 | Make `random_hex` fail closed (propagate RNG error, refuse to start HTTP server) | rightkit-mcp | 0 | low (one function; CodeRight pins =0.3.1 so ships in next mcp bump) | H | F29 |
| 2 | Turn off `ort/download-binaries`, `tls-native`, `copy-dylibs`, `tracing` from default (`default = []`, opt-in feature), drop vad `ndarray`; `asr` takes vad with `default-features = false` and its own `ort-defaults` becomes opt-in | rightkit-vad, rightkit-asr (consumers: asr, transcript dev) | vad 47 -> 10 (37); asr 31 effective; removes a build-time network fetch | medium (dev builds must supply ORT through rightkit-ort/`load-dynamic`; plan RK4 owns asr) | H (b1,b4); b3 wrong | F07, F08 |
| 3 | Route all ONNX Runtime setup through `rightkit-ort` (delete five direct `ort` pins, `find_onnxruntime` in tts) | vad, asr, ocr, qts, embed, tts | 0 (removes 5 duplicate pin declarations; qts also -6 via F08 ndarray/tracing) | medium-high (behavioural; provider/session setup) | H+L | F34, F08 |
| 4 | rightkit-fs: add default-on `json` feature gating serde/serde_json (+ bin `required-features`); rightkit-db depends with `default-features = false`; replace nix with libc in fs, jobs, process (record locks and `kill`) | rightkit-fs, db, jobs, process | db -7 (b1: -8); fs-only consumers -4 (nix); process -5; nix 0.30.1 leaves non-test builds | low (json), medium (lock semantics, runner.rs:63-66) | H (b1,b2); process extension Me | F12, F11 |
| 5 | agent-room-core: `http` feature (axum, tokio-stream), `rand` -> `getrandom` | agent-room-core | 32 + 6 (store-only consumers); 0 consumers today | low | H | F22 |
| 6 | Commit pregenerated ggml bindings for darwin/linux, drop bindgen; review cmake discovery | rightkit-qts-ggml-sys | 24 build-time crates + libclang requirement | low-medium (binding drift vs ggml pin) | H | F30, F31 |
| 7 | sha2: move cache and models to 0.10; relax media/playback exact pins to `"0.10"`/`"0.12"` | cache, models, media, playback | 8 each for cache and models; no workspace-wide removal (ed25519-dalek, tauri keep 0.10; pdf keeps 0.11 via lopdf 0.44) | low | H (b1,b2) | F20, F21 |
| 8 | Remove reqwest/tokio-net/futures-util by moving the streamable-HTTP path to ureq or rightkit-http streaming; consolidate logs/models onto rightkit-http | rightkit-mcp, models, logs, http | mcp 25 (Haiku 30); removes reqwest 0.12 duplicate | HIGH (streaming semantics; needs full MCP HTTP test coverage; CodeRight is the only consumer) | H+L | F28, F41 |
| 9 | Default-feature hygiene: shell `engine` off; search `sqlite` off; audio `capture` off; crash `zip` behind `support-bundle`; agent-tools doc fix (browser stays default for right-ctl) | rightkit-shell, search, audio, crash, agent-tools | 9, 8, 23, 16, 0 (all for consumers that do not exist yet except shell, whose apps already opt out) | low | H | F26, F27, F24, F25, F23 |
| 10 | models: make zip, tar/flate2 optional features; dirs -> std; chrono -> time (rightapps) | rightkit-models (CutRight, GenRight consumers must opt in) | 10 + 6 + 3 + 3 | medium (consumer opt-in) | H | F19 |
| 11 | rightkit-http `gzip` off for llm/oauth; logs trims (chrono, getrandom 0.3, `thermal` feature); tauri getrandom 0.3 | llm, oauth, logs, tauri | 5 + 5; 3 + 1 + 6 (macOS); 1 | low | H | F16, F15, F18 |
| 12 | Small replacements: thiserror hand impls (fs, settings, secrets, transcript, tts), uuid -> getrandom (jobs, qa), base64 inline (license, secrets, models) | those crates | 3 each (thiserror), 2 (uuid), 1 (base64) when the consumer has no other path | low | H | F13, F14, F17 |
| 13 | Remove proven-unused dependencies | asr (`eyre`), playback (`serde`, `serde_json`), overlay (dev `serde_json`), audio (`windows-implement`, confirm on Windows), oauth (`axum`/`tokio` to dev) | 2 + 0 + 0 + 0 (Windows est. 1-2) + 0 | none-low | H+L | F01-F05 |
| 14 | npm: move `@rightkit/updates` to a peer in `@rightkit/tauri`; make react-image-crop optional; move `UPDATE_HOST_CONTRACT_VECTORS` out of the runtime entry; share log command names; align peer ranges | tauri, media-ui, updates, logs, settings-ui and UI peers | 1 duplicate copy per consumer lockfile (5 apps); 2 packages; 0 | low-medium (republish; consumers already install updates) | H | N03-N07 |
| 15 | doc-render: remove unused `mermaid` and `katex`; decide `remark-math` | @rightkit/doc-render | 111 packages (236 -> 125) in doc-render's closure; ViewRight's own mermaid/katex stay, so ~0 in ViewRight today | low | H | N01, N02 |
| 16 | Consolidate atomic writes (then locks, then spawns) onto rightkit-fs / rightkit-process after #4; one safe-archive module in models | fs, logs, models, settings, service, pdf, shell, cache, agent-room-core, automation, control, crash, asr; process | 0 (removes ~13 copies) | medium (lock semantics must be preserved; F36 decision first) | H+L | F35-F37 |
| 17 | Windows: move the seven `windows` 0.61 crates to 0.62 and fold the sandbox/automation job objects into rightkit-process; hotkeys -> windows-sys | automation, hotkeys, process, qa, sandbox, service, speech-client | Windows only, not measurable here (estimated ~10 for hotkeys) | medium (Windows build and qualification) | H, Windows lead | F38-F40 |
| 18 | Release hygiene: reconcile publish state (automation, agent-room-core staged but `publish = false`; `registry` entries for shell/sandbox; npm `npm` section); regenerate INDEX.md; delete stray files; fix stale comments/docs | versions JSON, INDEX.md, control, license, qts PINNED.md, swift/README.md, Cargo.toml comment, swift archive script | 0 | low | H+L | F54, F56, F58, F60, N13, S01, S03 |
| 19 | Remove `rightkit-computer`; merge `rightkit-qts-ggml` into -sys or qts (update GenRight's three forwarding deps) | computer, qts-ggml, genright-qts | 0 crates, -2 workspace members | low | H+L | F61, F33 |
| 20 | Wire tests: ownership-approval, characterization, infrastructure, remaining packages into a root script; dedupe ownership helpers; move agent-room-mcp credentials to rightkit-secrets | root package.json, ownership-approval, agent-room-mcp | 0 | low | H | N16, O01, O02, A01 |
| 21 | App pin reconciliation (process, ort, db, secrets, sandbox, qa, control, watch, template) | apps and desktop-app-template | removes duplicate crate copies per app lockfile (not counted here) | medium (per app) | H+L | F55, N11, N17 |
| 22 | Embed/pdf/transcript/product-leak cleanups: embed `bundled` passthrough + embedder feature + vector store on rightkit-db; pdf `system-fonts` feature; move transcript's ASR round-trip test; move product names out of cache/secrets/service/speech-client; fix sync fixture bin | embed, pdf, transcript, cache, secrets, service, speech-client, sync | embed ~58-60 (tokenizers) for vector-only; pdf 4; transcript 125-145 dev-only | medium (API/feature splits) | H | F44, F45, F49-F51, F53 |
| 23 | Owner decisions (no change proposed until decided): overlay adopt-or-archive; pdf/ocr adoption vs ViewRight's own crates; tauri->shell fold; watch `pub use notify`; zero-consumer published crates (6.4); `LicenseController`; qa-chrome code-sign helpers | overlay, pdf, ocr, tauri, shell, watch, cache, search, hotkeys, playback, audio, crash, llm, sync | n/a | n/a | H | F62-F65, F47, S02, N10 |

### 5.2 RustRight actions (broker; not RightKit work)

| # | Change | Packages | Risk | Found by | Matrix |
|---|---|---|---|---|---|
| 1 | Fix native-addon TOCTOU: read once, hash the buffer, load from a verified copy | build-control-broker (windows-job.mjs:25-29, macos-resource.mjs:25-29) | medium (both hosts) | H | R04 |
| 2 | Declare or remove undeclared cross-package relative imports (into `packages/release/managed-roots.mjs` and between build-control packages); settle clients' mixed by-name/relative imports of core | clients, broker, policy | medium | H | R03 |
| 3 | Remove byte-identical copies (background-process.mjs, ownership catalogs/fingerprints/availability, versions JSON x3); consume generated outputs | clients, policy, release | low-medium | H | R05 |
| 4 | Delete `build-control-staging` (11 lines, unused core dependency); archive `build-control-phase0` and its root scripts/catalog entries; delete empty `legacy-v1/` | staging, phase0, broker | low | H | R01, R02, R07 |
| 5 | Parameterise `/Volumes/D` / `D:\` fallbacks | core storage.mjs:12, broker cache-proof-cli.mjs:90 | low | H | R06 |
| 6 | Share one lock/atomic-write/hash/spawn helper inside build-control-core; decide whether policy should consume `@rightkit/hooks`; drop `thin-logger.mjs` duplication | core, broker, clients, policy | low-medium | H | R08-R10 |

## 6. Appendices

### 6.1 Measured totals (default features vs `--no-default-features`, host macOS, unique registry crates, normal+build)

| Crate | default | no-default | largest exclusive subtrees (excl crates) |
|---|---|---|---|
| `agent-room-core` | 80 | 80 | axum 29; rusqlite 13; sha2 8 |
| `rightkit-agent-tools` | 124 | 12 | rightkit-browser 104 |
| `rightkit-align` | 14 | 14 | rightkit-transcript 7 |
| `rightkit-asr` | 137 | 137 | tokenizers 54; objc2-core-ml 5; realfft 5 |
| `rightkit-audio` | 64 | 41 | rubato 16; rightkit-media 11; cpal 5 |
| `rightkit-automation` | 12 | 12 | rightkit-service 7 |
| `rightkit-browser` | 124 | 124 | chromiumoxide 54; tempfile 4; rightkit-process 2 |
| `rightkit-cache` | 10 | 10 | sha2 10 |
| `rightkit-compose` | 10 | 10 | serde 4; thiserror 3 |
| `rightkit-computer` | 0 | 0 |  |
| `rightkit-contracts` | 65 | 65 | jsonschema 54 |
| `rightkit-control` | 0 | 0 |  |
| `rightkit-crash` | 39 | 39 | zip 16; rightkit-logs 11 |
| `rightkit-db` | 30 | 27 | rightkit-fs 11; rusqlite 10; zeroize 1 |
| `rightkit-embed` | 90 | 90 | tokenizers 58; sha2 7; rusqlite 5 |
| `rightkit-framed-sidecar` | 11 | 11 | serde 6; serde_json 4 |
| `rightkit-fs` | 19 | 19 | nix 4; serde_json 4; serde 3 |
| `rightkit-hotkeys` | 0 | 0 |  |
| `rightkit-http` | 49 | 44 | ureq 27; sha2 8; serde 6 |
| `rightkit-jobs` | 46 | 46 | rightkit-db 14; sha2 8; time 6 |
| `rightkit-license` | 30 | 30 | ed25519-dalek 18; serde 6; serde_json 4 |
| `rightkit-llm` | 49 | 49 | rightkit-http 38 |
| `rightkit-logs` | 45 | 23 | ureq 22; objc2-foundation 6; serde 6 |
| `rightkit-mcp` | 107 | 107 | reqwest 25; sha2 8; ureq 4 |
| `rightkit-media` | 24 | 24 | sha2 8; serde 6; serde_json 4 |
| `rightkit-models` | 68 | 68 | ureq 21; zip 10; sha2 8 |
| `rightkit-oauth` | 56 | 49 | rightkit-http 26; rightkit-secrets 4; rightkit-process 3 |
| `rightkit-ocr` | 51 | 51 | image 16; sha2 8; serde 6 |
| `rightkit-ort` | 5 | 0 | ort 5 |
| `rightkit-overlay` | 7 | 7 | serde 7 |
| `rightkit-pdf` | 85 | 85 | lopdf 32; subsetter 12; fontdb 4 |
| `rightkit-playback` | 29 | 29 | parking_lot 5 |
| `rightkit-process` | 5 | 5 | nix 5 |
| `rightkit-qa` | 46 | 46 | sha2 8; toml 8; png 7 |
| `rightkit-qts-ggml-sys` | 28 | 28 | bindgen 24; cmake 4 |
| `rightkit-qts-ggml` | 28 | 28 | rightkit-qts-ggml-sys 28 |
| `rightkit-qts` | 84 | 84 | rightkit-qts-ggml 15; prost-build 15; ort 10 |
| `rightkit-sandbox` | 20 | 20 | portable-pty 15; rightkit-process 2 |
| `rightkit-search` | 8 | 0 | rusqlite 8 |
| `rightkit-secrets` | 19 | 19 | serde_json 4; getrandom 3; serde 3 |
| `rightkit-service` | 12 | 12 | libc 1 |
| `rightkit-settings` | 14 | 14 | serde_json 4; serde 3; thiserror 3 |
| `rightkit-shell` | 253 | 244 | tauri 191; rightkit-jobs 7; objc2-user-notifications 2 |
| `rightkit-speech-client` | 12 | 12 | rightkit-service 1 |
| `rightkit-sync` | 11 | 11 | serde 6; serde_json 4 |
| `rightkit-tauri` | 31 | 31 | rightkit-license 28; getrandom 1 |
| `rightkit-transcript` | 14 | 14 | serde_json 4; serde 3; thiserror 3 |
| `rightkit-tts` | 27 | 27 | sha2 8; rightkit-process 3 |
| `rightkit-vad` | 47 | 10 | ort 47 |
| `rightkit-watch` | 19 | 19 | notify 8; tracing 4; tokio 2 |

### 6.2 Multi-version crates in Cargo.lock and the RightKit-controlled causes

Reproduced counts: 815 packages, 77 multi-version names, 176 name@version entries (Haiku b4 J.2 has the full puller list; it is lock-derived and I did not re-derive it per member). Actionable groups, with the manifest line that causes them:

| Group | Versions | RightKit-controlled cause | Third-party cause | Action |
|---|---|---|---|---|
| sha2 / digest / block-buffer / crypto-common / cpufeatures / const-oid | 0.10.9 + 0.11.0 | cache (Cargo.toml:13), models (:29), pdf (:35) on 0.11; 11 crates on 0.10 | lopdf 0.44 -> 0.11, lopdf 0.42 -> 0.10; ed25519-dalek 2.2.0, tauri-codegen, wry -> 0.10 | F20: cache + models to 0.10 |
| getrandom | 0.2.17, 0.3.4, 0.4.3 | tauri =0.2.17 (:13); logs 0.4 (:27); pdf 0.4.3 (:36); http/mcp/models/oauth/secrets 0.3 | ring (0.2.17), uuid (0.4.3), lopdf (0.4.3) | F15, F18 |
| rand | 0.9.5, 0.10.3 | agent-room-core 0.9 (:11), qts 0.10 (:43) | tokenizers/tungstenite 0.9; lopdf 0.10 | F22 |
| nix | 0.28.0, 0.30.1 | fs, jobs, process (0.30.1) | portable-pty (0.28.0) | F11, F43 |
| windows / windows-core / -link / -numerics / -future / -strings / -result / -threading / -collections | 0.61.x + 0.62.x | seven crates on 0.61, audio+ocr on 0.62, control+shell both | tauri/wry stack | F39 (Windows lead) |
| windows-sys | 0.45, 0.52, 0.59, 0.60.2, 0.61.2 | RightKit crates all 0.61 | third-party | F52 (no action) |
| prost / prost-derive | 0.13.5 + 0.14.4 | qts build dep prost-build 0.13 | - | F32 |
| libloading / itertools / shlex / nom | 0.7/0.8/0.9; 0.13/0.14; 1.3/2.0; 7/8 | qts-ggml-sys bindgen (clang-sys -> libloading 0.8, itertools 0.13, shlex 1.3) | tokenizers, cc | F30 removes the bindgen-only copies |
| reqwest / wasm-streams | 0.12.28 + 0.13.5 | mcp (0.12) | chromiumoxide/tauri (0.13) | F28 |
| base64 | 0.13.1, 0.21.7, 0.22.1, 0.23.1 | license, secrets, models, oauth, ocr (dev) on 0.22 | ureq 0.23, chromiumoxide 0.22, tokenizers 0.13, swift-rs 0.21 | F17 |
| thiserror / syn | thiserror 1.0.69 + 2.0.21; syn 1.0.109 + 2.0.118 + 3.0.6 | all RightKit crates on thiserror 2 (-> syn 3) and serde derive (-> syn 2) | portable-pty, tauri (thiserror 1, syn 1) | F13 (only local effect) |
| toml (+ _datetime, _edit, winnow, serde_spanned) | 0.8.2, 0.9.12, 1.1.2 | qa toml 0.8 (:30) | tauri build stack | low (qa Cargo.toml:30) |
| dirs | 6.0.0, 7.0.0 | models 7 (:33) | tauri 6 | F19 |
| lopdf / md-5 / aes / cbc / ecb / cipher / block-padding / inout / nom 8 | two generations each | pdf `extract` (pdf-extract -> lopdf 0.42) | pdf-extract | F44 |
| png, jni, objc/foreign-types, heck, indexmap, hashbrown, darling, schemars, bitflags 1/2, webview2-com(-sys) | two to three each | none directly | tauri/wry/cpal stacks | NONE |
| esbuild (3), vite (2), jsdom (2), @types/node (2), commander (3), katex (2), d3-* , cose-base, layout-base, internmap, aria-query, dom-accessibility-api, rrweb-cssom, tinyexec, undici-types | npm | doc-render (mermaid chain, katex 0.18 + 0.16), media-ui vs others (jsdom, aria-query) | mermaid | N01, N12 |

### 6.3 Consumer pins (apps; `.cache`, worktree and example copies excluded)

| Crate | source | pins (version: apps) |
|---|---|---|
| `rightkit-browser` | 0.2.3 | `=0.2.0`: coderight; `=0.2.2`: genright; `workspace`: coderight |
| `rightkit-control` | 0.1.9 | `=0.1.6`: cockpit; `=0.1.9`: cutright, genright |
| `rightkit-db` | 0.3.1 | `=0.2.1`: coderight, mailright, scraperight, viewright; `=0.3.0`: coderight, genright; `workspace`: coderight |
| `rightkit-embed` | 0.2.2 | `=0.2.1`: mailright; `=0.2.2`: coderight |
| `rightkit-framed-sidecar` | 0.1.0 | `=0.1.0`: template |
| `rightkit-fs` | 0.2.0 | `=0.1.0`: coderight; `workspace`: coderight |
| `rightkit-http` | 0.2.0 | `=0.1.0`: coderight, mailright; `workspace`: coderight |
| `rightkit-jobs` | 0.2.4 | `=0.2.1`: cutright; `=0.2.2`: genright |
| `rightkit-license` | 0.1.3 | `=0.1.3`: coderight, heardright, mailright, scraperight, viewright |
| `rightkit-logs` | 0.2.0 | `=0.1.0`: heardright, scraperight, viewright; `=0.2.0`: coderight, genright, mailright, template |
| `rightkit-mcp` | 0.4.0 | `=0.3.1`: coderight; `workspace`: coderight |
| `rightkit-media` | 0.2.3 | `=0.2.3`: genright |
| `rightkit-models` | 0.2.3 | `=0.2.1`: cutright; `=0.2.2`: genright |
| `rightkit-oauth` | 0.3.0 | `=0.2.0`: coderight, mailright; `workspace`: coderight |
| `rightkit-ort` | 0.2.1 | `=0.1.4`: cutright; `=0.2.0`: coderight, genright, scraperight; `workspace`: coderight |
| `rightkit-process` | 0.3.1 | `=0.1.0`: cutright; `=0.2.1`: template; `=0.3.0`: genright; `=0.3.1`: coderight |
| `rightkit-qa` | 0.2.13 | `=0.2.12`: cockpit, cutright, genright |
| `rightkit-qts` | 0.2.1 | `=0.2.1`: genright |
| `rightkit-qts-ggml` | 0.1.0 | `=0.1.0`: genright |
| `rightkit-qts-ggml-sys` | 0.1.0 | `=0.1.0`: genright |
| `rightkit-sandbox` | 0.3.1 | `=0.2.4`: coderight |
| `rightkit-secrets` | 0.1.1 | `=0.1.0`: coderight, cutright, genright, mailright, scraperight; `workspace`: coderight |
| `rightkit-service` | 0.1.1 | `=0.1.0`: heardright |
| `rightkit-settings` | 0.1.0 | `=0.1.0`: genright |
| `rightkit-shell` | 0.1.4 | `=0.1.2`: coderight, mailright; `=0.1.3`: viewright |
| `rightkit-speech-client` | 0.3.0 | `=0.3.0`: cutright, heardright |
| `rightkit-tauri` | 0.1.1 | `=0.1.1`: coderight, heardright, mailright, scraperight, template, viewright |
| `rightkit-watch` | 0.3.1 | `=0.1.0`: coderight; `=0.3.0`: coderight; `=0.3.1`: viewright; `workspace`: coderight |

npm consumer pins are in the 2.B table. Stale-pin highlights: `rightkit-process` four generations (CutRight =0.1.0, template =0.2.1, GenRight =0.3.0, CodeRight =0.3.1) against source 0.3.1; `rightkit-ort` CutRight =0.1.4; `rightkit-secrets` every app =0.1.0 against 0.1.1; `rightkit-sandbox` CodeRight =0.2.4 against 0.3.1; `rightkit-control` Cockpit =0.1.6 against 0.1.9; `rightkit-watch` CodeRight =0.1.0/=0.3.0 and ViewRight =0.3.1.

### 6.4 Zero-consumer crates: disposition table (F64)

"Consumers" = Cargo manifests in the 14 app repos and `rightkit` (excluding examples/spikes/templates). Haiku proposed removing or folding many of these; Luna proposed only `rightkit-computer`.

| Crate | Zero consumers | crates.io | Context | Recommendation |
|---|---|---|---|---|
| rightkit-computer | yes | not published | README: folded into automation | remove (F61) |
| rightkit-align, -compose, -contracts | yes (by design) | not published | plan RK4, RK9, RK0 | keep (PLAN) |
| rightkit-asr, -vad, -transcript, -tts | yes (asr: transcript dev only; tts: none) | not published | plan RK4 local route | keep (PLAN); apply F01/F07/F34/F45 |
| rightkit-qts, -qts-ggml(-sys) | GenRight consumes | published 0.2.1/0.1.0/0.1.0 | TTS stack | keep; F33 merge decision |
| rightkit-agent-tools | none as library | not published | installed as `right-ctl` by scripts/install-right-ctl.sh | keep |
| agent-room-core | none | not published | server for @rightkit/agent-room-mcp | keep |
| rightkit-automation | agent-tools (optional) only | not published (staged) | desktop automation for right-ctl; supersedes computer | keep; reconcile publish state |
| rightkit-pdf | none | published 0.1.0 | extracted from ViewRight crates/pdf (ViewRight still ships its own 4,586-line copy) | DEC: adopt in ViewRight or stop publishing |
| rightkit-ocr | none | not published | ViewRight/ScrapeRight keep their own OCR crates | DEC |
| rightkit-cache, -search | none | published 0.1.0 | no known owner plan | DEC (published, small) |
| rightkit-hotkeys, -overlay | none | not published | HeardRight/Cockpit hosts named in README; Swift overlay exists | DEC |
| rightkit-audio, -playback, -crash, -llm, -sync | none | audio/playback/crash published; llm/sync not | CutRight uses a TS playback adapter; sync pairs with Swift CloudKit helper | DEC |
| rightkit-settings | GenRight (=0.1.0) | published | - | keep |
| rightkit-db / -embed / -control | **have** consumers (Haiku b1 said none) | published | - | keep |

### 6.5 Items I could not verify (and what is needed)

| Item | Why UNVERIFIABLE | Needed |
|---|---|---|
| F02 removal of `windows-implement` | compiled only for Windows | Windows build of rightkit-audio |
| F09 ocr feature trims (objc2-foundation, windows) | transitive feature needs of objc2-vision / WinRT bindings | macOS + Windows build after trimming |
| F08 vad/qts ort feature trims | static only; no build run | `rightkit cargo check -p rightkit-vad -p rightkit-qts` with a scratch manifest edit |
| F38-F40 Windows consolidation and windows-sys swap | no Windows target in the broker | Windows build + qualification (Windows lead) |
| F28 reqwest removal safety | behavioural (streaming) | MCP streamable-HTTP integration tests |
| F36 lock unification | semantics differ by design | design decision + tests |
| F62, F64, F65, N10, S02, S05, R09, R10 | ownership or design judgements | owner decision |
| Advisory/unmaintained status of any crate | no RustSec access in this session | `cargo audit` / `cargo deny` run on a networked host |
| ed25519-dalek default-feature trim | not measured by Haiku or me | scratch manifest + tree |
| Per-member puller lists for the 176 lock duplicates | I reproduced the counts, not each puller | Haiku b4 J.2 (lock-derived) is the reference |
