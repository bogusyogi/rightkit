# Apps audit G2: CutRight, GenRight, PostRight (what still has to move onto RightKit)

Date: 2026-10-10. Read-only. Sources: `rightkit/INDEX.md`, `rightkit/docs/CAPABILITY-MAP.md`, `rightkit/docs/audits/2026-10-08-inventory-final.md:148-171`, `rightkit/docs/plans/2026-10-08-content-pipeline-requests.md` and `...-execution-plan.md`, `rightkit/packages/release/rightkit-versions.json` (`registry` section = "latest published" for all stale-pin tests below).

Method notes and caveats
- "Latest" = `rightkit-versions.json` `registry`. `INDEX.md` is older than that file for several crates (it still says asr, ocr, vad, tts, transcript, fsindex, ownership are "staged, never published"; the registry now records them). I used the registry and did not verify against crates.io/npm.
- Registry has NO record for: rightkit-llm, -review, -record, -scenario, -align, -channel-memory, -compose, -daemon, -contracts (staged only), -automation (staged only), -agent-tools, -hotkeys, -overlay, -sync (staged only). Anything that depends on these is blocked on publication, not just on app work.
- LOC are `wc -l` of the named files on the working tree (CutRight has 4 uncommitted modified files: `cutright-engine/examples/fake_genright.rs`, `genright_cmd.rs`, `cutright-genright/src/orchestrator.rs`, `cutright-speech/src/lib.rs`; GenRight is clean apart from untracked `.claude/`, `model-promotions/`). "est." = my estimate of touched lines inside a larger file.
- Scanner: `right-release ownership --app` FAILED for cutright and genright with `protected-evidence: committed ownership: protected release checkout has tracked or untracked changes` (dirty checkouts). It succeeded for postright. For the two failures I ran a read-only emulation of the dependency and source fingerprints in `packages/build-control-policy/src/ownership-fingerprints.json` (regex over git-tracked files, tests excluded, comments not stripped); results are in each app's section and are an approximation, not scanner output.
- Saved JSON: `apps-g2-cutright-ownership.json` (error body), `apps-g2-genright-ownership.json` (error body), `apps-g2-postright-ownership.json` (full, ok).

---

## CutRight

### 1. Shape
- Repo `/Volumes/D/claude/cutright`, last commit `532fadf` 2026-10-10 ("RightKit first" rule). Targets: macOS + Windows (Tauri 2 desktop app, `apps/cutright/src-tauri/tauri.conf.json`, `tauri.macos.conf.json`, `tauri.windows.conf.json`; `right-release` hooks).
- Manifests: root `Cargo.toml` (workspace, 25 members), `apps/cutright/src-tauri/Cargo.toml` (separate `[workspace]`), `apps/cutright/package.json` (pnpm 11.24, React 19, Vite 8). Also `.rightax.json` at root.
- In-house Rust LOC (all `.rs`, incl. tests): engine 23,479; project 6,800; genright 4,616; timeline 4,572; editorial 4,406; reframe 3,906; ledger 3,566; shorts 3,425; compose 3,376; src-tauri 3,365; qa 3,282; speech 3,057; ui-e2e 2,928; color 2,468; captions 2,285; cuts 1,801; variants 1,555; audio 1,354; compositor 1,089; otio 942; fcpxml 938; compositor-masks 622; compositor-gpu 539; compositor-effects 348; transcript 206; model-store 81. Total about 82k Rust. UI: about 14.7k TS/TSX/CSS under `apps/cutright/src` (plus vendored `src/editor/vendor/hyperframes`).

### 2. RightKit adoption today (pins vs registry)
| Manifest:line | Package | Pin | Latest | Verdict |
|---|---|---|---|---|
| Cargo.toml:51 | rightkit-process | =0.1.0 | 0.3.3 | STALE and dead: `[workspace.dependencies]` entry, no member uses it (grep of crates/ and src-tauri: zero `rightkit_process` hits); `Cargo.lock` carries 0.2.1 and 0.3.0 transitively |
| apps/cutright/src-tauri/Cargo.toml:20 | rightkit-control | =0.1.9 | 0.1.11 | stale (patch) |
| crates/cutright-audio/Cargo.toml:20 | rightkit-audio | =0.2.0 | 0.2.0 | ok |
| crates/cutright-engine/Cargo.toml:18 | rightkit-jobs | =0.2.1 | 0.2.6 | stale |
| crates/cutright-model-store/Cargo.toml:11 | rightkit-models | =0.2.1 | 0.2.7 | stale |
| crates/cutright-project/Cargo.toml:21 | rightkit-fs | =0.3.0 | 0.3.3 | stale |
| crates/cutright-qa/Cargo.toml:21 | rightkit-fs | =0.3.0 | 0.3.3 | stale |
| crates/cutright-reframe/Cargo.toml:20 | rightkit-ort | =0.1.4 | 0.2.2 | STALE, breaking: 0.1 to 0.2 minor on a 0.x crate; GenRight is already on 0.2.2 (suite wants one ort pin) |
| crates/cutright-speech/Cargo.toml:27 | rightkit-ort (windows) | =0.1.4 | 0.2.2 | STALE, breaking (same) |
| crates/cutright-speech/Cargo.toml:11 | rightkit-speech-client | =0.3.0 | 0.3.2 | stale |
| crates/cutright-shorts/Cargo.toml:27 | rightkit-secrets | =0.1.0 | 0.1.2 | stale |
| crates/cutright-ui-e2e/Cargo.toml:11 | rightkit-qa | =0.2.12 | 0.2.14 | stale |
| apps/cutright/package.json:39 | @rightkit/legal | 0.3.1 | 0.3.1 | ok |
| :40 | @rightkit/release | 0.2.133 | 0.2.147 | stale |
| :51 | @rightkit/app-shell | 0.2.0 | 0.3.0 | stale, 0.x minor |
| :52 | @rightkit/legal-ui | 0.1.3 | 0.2.0 | stale, 0.x minor |
| :53 | @rightkit/license | 0.1.8 | 0.1.8 | ok, but never imported (see licensing) |
| :54 | @rightkit/logs | 0.1.4 | 0.1.4 | ok, never imported |
| :55 | @rightkit/media-ui | 0.2.0 | 0.3.0 | stale, 0.x minor |
| :56 | @rightkit/onboarding | 0.2.0 | 0.3.0 | stale, 0.x minor |
| :57 | @rightkit/platform-ui | 0.1.1 | 0.2.0 | stale, never imported |
| :58 | @rightkit/settings-ui | 0.2.7 | 0.3.0 | stale, 0.x minor |
| :59 | @rightkit/shell | 0.1.0 | 0.1.0 | ok, never imported |
| :60 | @rightkit/theme | 0.2.0 | 0.2.1 | stale |
| :61 | @rightkit/updates | 0.2.4 | 0.2.5 | stale, never imported |

Stale pins: 20 manifest lines = 18 distinct packages (9 cargo, 9 npm). Resolved lock versions also lag (`Cargo.lock`: rightkit-control 0.1.8, rightkit-db 0.2.1, rightkit-fs 0.1.0 and 0.3.0, rightkit-process 0.2.1 and 0.3.0). Packages pinned in `package.json` but with zero imports in `apps/cutright/src`: @rightkit/license, logs, platform-ui, shell, updates.
Not pinned at all: rightkit-http, -llm, -logs, -crash, -settings, -cache, -search, -db, -media (only transitive via models/qa: lock shows rightkit-media 0.2.4, rightkit-db 0.2.1), -transcript, -vad, -asr, -ocr, -playback, -shell, -tauri, -service (transitive), -framed-sidecar (transitive).

### 3. Still-to-move table
Legend: ON = already on RightKit; DUP = in-house duplicate (name, LOC, target); GAP = RightKit owner missing or insufficient; N/A.

In-house duplicates (the work list):
| # | App code (path, LOC) | Target owner | Notes |
|---|---|---|---|
| D1 | `crates/cutright-audio/src/{bus 67, preset 103, profile 92, room_tone 359, tools 466}.rs` = 1,087 | rightkit-audio 0.2.0 (`bus`, `preset`, `profile`, `room_tone`, `measure::AudioTools` over rightkit-media runner) | `chain/ducking/filter` already re-exported from rightkit-audio (`cutright-audio/src/lib.rs:21`) so inventory 3.6 is half done. `room_tone.rs` differs from RightKit's by about 94 diff lines: reconcile before deleting. Tools still spawns ffmpeg itself (`tools.rs:96`). |
| D2 | `crates/cutright-speech/src/{window 517, coreml 329, coreml_tdt 307, onnx_tdt 381, words 222, audio 102}.rs` = 1,858 | rightkit-asr 0.0.2 (`longform`, `parakeet_coreml`, `transducer`, `audio`) | rightkit-asr states "CutRight's cutright-speech is a copy of the SR file and is covered by the same API" (`rightkit-asr/src/parakeet_coreml/mod.rs:13`). Verify `EndSource` semantics and VAD refinement (`words.rs`) are covered. Windows ORT bootstrap `ort_init.rs` (50) already uses rightkit-ort. |
| D3 | `crates/cutright-speech/src/vad.rs` 456 | rightkit-vad 0.0.2 | RightKit-side GAP: rightkit-vad scores Silero through ORT only; CutRight's macOS path is CoreML Silero (`vad.rs:55,193`) and has an energy+ZCR fallback reported in the transcript. |
| D4 | `crates/cutright-transcript/src/lib.rs` 206 | rightkit-transcript 0.1.0 (`contract.rs`; accepts `cutright.transcript.v1` on read, `contract.rs:8`) | Schema string change needs a compat read; on-disk project files keep `cutright.transcript.v1`. |
| D5 | `crates/cutright-captions/src/{segment 354, export 93, words 63, align 53}.rs` = 563 | rightkit-transcript (`segment`, `export`, `words`, `align`) | Caption styling/burn/placement (about 1,100 LOC) is app policy and stays. |
| D6 | `crates/cutright-project/src/{worker 376, loudness 265, probe 300, tools 118}.rs` = 1,059 | rightkit-media 0.2.4 (`runner`, `loudnorm`, `probe`, `dev_resolve`/`native`) | Plus loudness types/filter in `cutright-timeline/src/ffmpeg.rs` (`LoudnessTarget`, `LoudnessMeasurement`, `loudnorm_filter` at :124-170, :633) about 100 est. The typed `FfmpegCommand` builder (921 LOC) stays app-owned. |
| D7 | `crates/cutright-project/src/lock.rs` 144 | rightkit-fs 0.3.3 `FileLock` + `LockOwner` | same owner-file design (`rightkit-fs/src/lock.rs:33`) |
| D8 | `crates/cutright-qa/src/{probe 556, run 852, spec 186, report 107}.rs` = 1,701 (blackdetect/freezedetect/silencedetect/ebur128 parsers, checks, verdicts) | rightkit-review (RK5) + rightkit-media probe + rightkit-qa loudness | Execution plan C10 says extract and retire. BLOCKED: rightkit-review is private, no registry record. Receipt/package/zip (`receipt 190, package 499, zip 210`) are CutRight policy and stay. |
| D9 | `crates/cutright-shorts/src/llm.rs` about 150 est (AnthropicProvider `:190-283`, `build_request_body :133`, `parse_response :150`) + direct `ureq 3.4` (`Cargo.toml:24`) | rightkit-llm (Anthropic Messages, tool use) over rightkit-http | BLOCKED: rightkit-llm has no registry record. The secrets read is already on rightkit-secrets. Scanner-class hit: http-client advise (`ureq`). |
| D10 | `crates/cutright-genright/src/{sha256 165, journal lock about 40, engine.rs spawn about 120}` about 325 est | rightkit-fs `digest` feature (`sha256_reader`), `FileLock`; rightkit-process `OwnedCommand` | hand-rolled FIPS 180-4 SHA-256 (`sha256.rs:1`). |
| D11 | `crates/cutright-engine/src/discovery.rs` 637 | rightkit-service `discovery` (0.1.3; `/Applications`, `~/Applications`, Windows uninstall registry, `rightkit-service/src/discovery.rs:48-166`) | Same job as GenRight `genright-core/src/discovery.rs` (352). |
| D12 | `apps/cutright/src-tauri/src/engine_supervisor.rs` 899 | rightkit-jobs `EngineClient` via rightkit-shell `engine` feature (rightkit-shell 0.1.4) or rightkit-process `RestartPolicy` + rightkit-framed-sidecar `wire` | Hand-rolled hello wait, bounded restart (`MAX_RESTARTS 3`), NDJSON. Donor of RightKit's own `engine.rs`. |
| D13 | `crates/cutright-engine/src/media_cmd.rs` `settings.json` get/set (`:503-509`, `:1869-1920`) about 75 est | rightkit-settings 0.1.0 | Engine-side, plain read/modify/write JSON, no versioning/migration/corruption recovery. |
| D14 | `crates/cutright-engine/src/{thumb_cmd 185, storage_cmd 110}.rs` cache part about 200 est | rightkit-cache 0.1.0 (hard size cap, LRU, atomic writes) | thumbnail/waveform cache is uncapped. |
| D15 | `crates/cutright-project/src/audio_sync.rs` 298, `crates/cutright-reframe/src/media.rs` 283 (ffmpeg decode helpers; about 250 est touched) | rightkit-media runner + rightkit-audio `decode_mono_f32` | raw `Command::new(ffmpeg)` with local `hide_console` instead of RightKit process ownership. |
| D16 | Raw `std::process::Command` for ffmpeg/ffprobe/hostname across project, qa, audio, speech, reframe, compose, color (about 14 src files) | rightkit-process `OwnedCommand` (via rightkit-media) | No `rightkit_process` use anywhere in CutRight sources; children are not tree-owned. Counted inside D1/D6/D8/D15. |

Total in-house duplicates: 15 distinct rows (D1-D15; D16 is cross-cutting). LOC to move: about 10,100 (8,610 whole-file: D1, D2, D3, D4, D5, D6, D7, D8, D11, D12; about 1,500 partial est.: D6 timeline part, D9, D10, D13, D14, D15). Of that, about 1,850 is blocked on unpublished RightKit crates (D8 1,701 + D9 150).

Capability decisions (CAPABILITY-MAP order):
| Capability | State | Evidence / action |
|---|---|---|
| agent-experience-gates | not-applicable | `.rightax.json` present; `@rightkit/ax` not pinned (unverified need) |
| agent-room, agent-tools, daemon-host, cloud-sync, desktop-overlay, global-hotkeys, desktop-automation, browser-automation, browser-qa, embeddings, file-watching, mcp, oauth-sign-in, os-sandbox, pdf-engine, pdf-ui, document-rendering, lifecycle-hooks, github-workflows, localization, qa-evidence, framed-sidecar (direct), file-index | not-applicable | no use in CutRight; engine agent surface is NDJSON/CLI by design (`serve.rs:1-14`) |
| app-shell-ui | ON (stale 0.2.0) | `ui/index.tsx:6`, `main.tsx`; app-only extras (Thumbnail, ProgressRow, Drawer, ErrorPanel) stay |
| audio | DUP (D1) + GAP | capture n/a; GAP: audio OUTPUT for preview: `src-tauri/src/playback.rs` (1,400) drives `cpal` directly (`cpal 0.15` at `src-tauri/Cargo.toml:21`; scanner-class `audio` advise hit); rightkit-audio only owns capture |
| channel-memory | not-applicable now | plan C15 recipes consume RK6 later; crate private/unpublished |
| composition-model | DUP + GAP (RK9) | `cutright-compose/src/{mapper 396, snapshot 200}` about 600 should target `rightkit-compose` `FrameDescription`; compositor/GPU stack (compose 3,376 incl. decode 697, pcm 290, scheduler 188, clock 233, cache 134; compositor* 2,598) has no RightKit owner (rightkit-compose is "no rendering, codec or I/O", `lib.rs:12`). rightkit-compose unpublished and RK9 says spring easing and click effects not modelled |
| content-contracts | GAP (RK0) | no `brief`/export-manifest code in CutRight (grep: zero engine hits); C11 not started; rightkit-contracts staged 0.1.2, not in registry |
| content-review | DUP (D8) | blocked on rightkit-review publication (RK5) |
| crash-reporting | not adopted | no crash capture in app; rightkit-crash 0.2.0 published; 23 `eprintln!` sites only |
| desktop-control | ON (stale 0.1.9) | `src-tauri/src/lib.rs` embedded Control plugin |
| desktop-qa | ON (stale =0.2.12) | `cutright-ui-e2e` |
| desktop-shell | DUP (D12) + not adopted | no webview hardening, window-state, single-instance, tray; `@rightkit/shell` pinned, unused; GAP (small): reveal-in-file-manager (`src-tauri/src/lib.rs` `reveal_command` about 35 LOC) not in rightkit-shell (`opener.rs` is URLs only) |
| disk-cache | DUP (D14) | |
| http-client | DUP (D9) | `ureq` direct |
| legal-assent, legal-ui, onboarding-ui | ON (legal ok; legal-ui, onboarding stale) | `legal/legalConfig.ts`, `OnboardingGate.tsx`; manifest read from `legal/legal-manifest.json` |
| licensing | not wired (not a duplicate) | `@rightkit/license 0.1.8` pinned, no import in `src/`; contract is "free edition" per STATUS.md; confirm intended |
| llm-providers | DUP (D9), blocked | |
| local-services | DUP (D11) | |
| media-pipelines | DUP (D6, D15, D16) | |
| media-playback | adoption GAP | rightkit-playback 0.1.1 is published; `lib/mediaAdapters.ts:5-6` still says "not published yet", so `preparePlayback` is unwired |
| media-ui | ON (stale 0.2.0) | `MediaPlayer` in 5 views; `components/Player.tsx` (532) is the native timeline stage (app-only) |
| model-installation | ON (suite stack, stale 0.2.1) | `cutright-model-store` is an 81-line adapter over `rightkit_models::suite` (`lib.rs:1-14`). CutRight calls only the suite stack, so audit 4.3 (retire root stack) is NOT blocked by CutRight; GenRight is the blocker |
| ocr | GAP-not-started | C6 and C13 need rightkit-ocr (0.1.2 in registry) and rightkit-db; no use today |
| onnx-runtime | ON (stale 0.1.4) | `reframe/ort_rt.rs`, `speech/ort_init.rs`, `onnx_tdt.rs` |
| os-credential-store | ON (stale 0.1.0) | `shorts` |
| owned-processes | not adopted (D16) | dead workspace pin |
| persistent-jobs | ON (stale 0.2.1) | `serve.rs` `CommandSet`/`Dispatcher` |
| platform-ui | pinned, unused | |
| release-tooling | ON (stale 0.2.133) | `right-release` hooks; app scripts `sign-macos-bundle.mjs` 63, `mac-dmg.mjs` 63, `win-nsis.mjs` 33: signing machinery in a product repo, check against `docs/rules/release-signing.md` |
| safe-filesystem | ON for atomic writes + DUP (D7, D10 digest) | `cutright-project/src/fsutil.rs`, `cutright-qa/src/fsutil.rs` delegate to rightkit-fs (inventory 3.7 done for CutRight) |
| scenario-runtime, screen-recording | GAP-not-started (RK8, RK3) | C13 demo import reads `rightkit-record` bundles |
| script-alignment | not-applicable | GenRight RK4 |
| search | not-applicable | `phrase.search` (`media_cmd.rs:922`, about 55 LOC lexical scan) too small to move; C6 lexical moment search would use rightkit-db FTS5 + rightkit-search predicate compiler |
| settings-persistence | DUP (D13) | |
| settings-ui | ON (stale 0.2.7) | `lib/settingsBackend.ts` backs it with the engine `settings.json` |
| shared-speech-service | ON (stale 0.3.0) | `cutright-speech` re-exports `rightkit_speech_client` (`lib.rs:55`) |
| speech-recognition | DUP (D2) | in-process Parakeet copy |
| sqlite-storage | not-applicable now | CutRight stores JSON/blake3 files; C6 would add rightkit-db |
| structured-logging | not adopted | `@rightkit/logs` pinned, unused; no rightkit-logs |
| tauri-host-adapters | not adopted | rightkit-tauri / @rightkit/tauri absent |
| text-to-speech | not-applicable | GenRight owns voices |
| theme-ui | ON (stale 0.2.0) | `rightkit-theme generate` script, `app-shell` theme store (`lib/shell.ts`) |
| timed-transcripts | DUP (D4, D5) | |
| update-policy | not adopted | no updater plugin; `@rightkit/updates` pinned, unused |
| voice-activity-detection | DUP (D3) | |

### 4. Ownership scanner
- `right-release ownership --app /Volumes/D/claude/cutright --json` exit 1: `{"ok":false,"error":{"type":"protected-evidence","message":"committed ownership: protected release checkout has tracked or untracked changes"},"rows":[],"violations":[],...}` (saved as `apps-g2-cutright-ownership.json`). Counts per state: unavailable (0 rows).
- Emulated fingerprints (approximate): block 0; advise 3 hits: `http-client` (`ureq`, `crates/cutright-shorts/Cargo.toml:24`), `audio` (`cpal`, `src-tauri/Cargo.toml:21`), `theme-ui` relative-luminance in `crates/cutright-color/src/sample.rs` (probably a false positive: video colour math). Scanner needs a clean (committed) checkout to produce real output.

### 5. Handover list (priority order)
1. Bump all 18 stale pins in one PR, ort first: rightkit-ort =0.1.4 to 0.2.2 (reframe + speech Windows), plus fs 0.3.3, jobs 0.2.6, models 0.2.7, secrets 0.1.2, speech-client 0.3.2, qa 0.2.14, control 0.1.11; delete the dead `rightkit-process =0.1.0` workspace pin or adopt 0.3.3; npm: app-shell/media-ui/onboarding/settings-ui 0.3.0, legal-ui 0.2.0, platform-ui 0.2.0, theme 0.2.1, updates 0.2.5, release 0.2.147. ~60 LOC of manifests plus UI breakage triage; blocker: 0.x minor UI jumps need a build + `cutright-ui-e2e` run.
2. cutright-speech in-process Parakeet + VAD (D2, D3, D4) → rightkit-asr@0.0.2, rightkit-vad@0.0.2, rightkit-transcript@0.1.0; est. 2,520 LOC deleted; blocker: rightkit-vad lacks CoreML Silero scorer; golden-transcript parity on Mac and XPS.
3. ffmpeg runner/probe/loudness/lock (D6, D7, D15, D16) → rightkit-media@0.2.4 + rightkit-process@0.3.3 + rightkit-fs@0.3.3; est. 1,650 LOC; blocker: `FfmpegCommand` stays app-owned, so adapter needed; run render e2e.
4. cutright-audio remainder (D1) → rightkit-audio@0.2.0; est. 1,087 LOC; blocker: reconcile `room_tone.rs` divergence; compare serialized presets and generated graphs (inventory 3.6).
5. Captions segmentation/export (D5) → rightkit-transcript@0.1.0; est. 563 LOC; blocker: none.
6. Engine/Tauri plumbing (D11, D12, D13, D14, D10) → rightkit-service@0.1.3 (`discovery`), rightkit-jobs@0.2.6 `EngineClient` / rightkit-shell@0.1.4 `engine`, rightkit-settings@0.1.0, rightkit-cache@0.1.0, rightkit-fs digest; est. 2,200 LOC; blocker: rightkit-shell adoption also pulls hardening/window-state (needs Tauri CSP review); Windows run.
7. Wire `preparePlayback` from rightkit-playback@0.1.1 (`mediaAdapters.ts`); est. 30 LOC; blocker: none (stale comment says unpublished).
8. cutright-qa critic extraction (D8) → rightkit-review (RK5) and shorts LLM (D9) → rightkit-llm + rightkit-http@0.2.4; est. 1,850 LOC; blocker: both crates unpublished; do not start before publication.
9. C11 brief/export manifest → rightkit-contracts (RK0) and C12/C13 → rightkit-compose (RK9), rightkit-record (RK3), rightkit-scenario (RK8), rightkit-ocr@0.1.2; est. ~600 LOC mapper retarget plus new code; blocker: all unpublished; RK9 lacks spring easing/click effects; no RightKit owner for the GPU compositor or audio output (`playback.rs`).
10. Adopt rightkit-logs@0.2.0 + rightkit-crash@0.2.0 (and decide licensing/updates wiring: `@rightkit/license`, `@rightkit/updates`, `@rightkit/logs` are pinned but unused); est. 150 LOC; blocker: product decision (free edition).

---

## GenRight

### 1. Shape
- Repo `/Volumes/D/claude/genright`, last commit `8463e35` 2026-10-10. Targets: macOS + Windows (Tauri 2 shell `src-tauri`, `right-release.config.mjs` mac/win), Rust engine CLI/sidecar `genright-engine`.
- Manifests: root `Cargo.toml` (workspace: src-tauri + 8 crates), `src-tauri/Cargo.toml`, root `package.json` (pnpm 11.24, React 19, Vite 8, TS 6), `pnpm-workspace.yaml`.
- In-house Rust LOC (git-tracked `.rs`): genright-studio 18,029; genright-gen 11,476; genright-local-image 6,067; genright-e2e 4,386; src-tauri 2,023; genright-engine 1,572; genright-qts 1,319; genright-core 1,059; genright-protocol 335. Total about 46k Rust. UI `src/` about 17.4k TS/TSX/CSS; legacy Node `server/` + `compat-lib` about 6.6k MJS (kept only for JS tests and `scripts/legacy-api.mjs`, per `docs/LEGACY-MEDIA-CONSOLE-CUTOVER.md`; not RightKit-relevant); `scripts/` 1.8k.

### 2. RightKit adoption today (pins vs registry)
| Manifest:line | Package | Pin | Latest | Verdict |
|---|---|---|---|---|
| genright-e2e/Cargo.toml:24,31 | rightkit-qa | =0.2.12 | 0.2.14 | stale |
| genright-e2e/Cargo.toml:29, engine :17, gen :22, qts :31 | rightkit-ort | =0.2.2 | 0.2.2 | ok |
| genright-e2e/Cargo.toml:32 | rightkit-browser | =0.2.2 | 0.2.2 | ok |
| e2e :33, gen :25, local-image :21, qts :29, src-tauri :43 | rightkit-process | =0.3.0 | 0.3.3 | stale (5 lines) |
| gen :14, local-image :17 | rightkit-db | =0.3.0 | 0.3.2 | stale |
| gen :15, local-image :20 | rightkit-jobs | =0.2.2 | 0.2.6 | stale |
| gen :20 | rightkit-secrets | =0.1.0 | 0.1.2 | stale |
| gen :26, studio :21 | rightkit-fs | =0.3.0 | 0.3.3 | stale |
| local-image :18 | rightkit-logs | =0.2.0 | 0.2.0 | ok |
| protocol :11, local-image :19, qts :35, studio :20 | rightkit-models | =0.2.2 | 0.2.7 | stale (4 lines; root `Store` stack, see below) |
| studio :19 | rightkit-media | =0.2.3 | 0.2.4 | stale |
| qts :34 | rightkit-tts | 0.1.0 (not exact) | 0.1.0 | ok |
| qts :37-39 | rightkit-qts / -qts-ggml / -qts-ggml-sys | =0.2.1 / =0.1.0 / =0.1.0 | 0.2.4 / 0.1.1 / 0.1.1 | stale (3) |
| src-tauri/Cargo.toml:41 | rightkit-settings | =0.1.0 | 0.1.0 | ok |
| src-tauri/Cargo.toml:44 | rightkit-control | =0.1.9 | 0.1.11 | stale |
| package.json:57-61 | @rightkit/app-shell 0.2.1, media-ui 0.2.6, onboarding 0.2.3, platform-ui 0.1.2, settings-ui 0.2.7 | | 0.3.0, 0.3.0, 0.3.0, 0.2.0, 0.3.0 | stale, 0.x minor |
| package.json:62 | @rightkit/theme | 0.2.1 | 0.2.1 | ok |
| package.json:76 | @rightkit/release | 0.2.135 | 0.2.147 | stale |

Stale pins: 29 manifest lines = 18 distinct packages (12 cargo: qa, process, db, jobs, secrets, fs, models, media, qts, qts-ggml, qts-ggml-sys, control; 6 npm). `pnpm-workspace.yaml:6-14` `minimumReleaseAgeExclude` lists only old versions (app-shell 0.2.0||0.2.1, etc.), so the 0.3.0 jump also needs new exclude entries.
Not pinned: rightkit-http, -llm, -crash, -cache, -search, -asr, -vad, -ocr, -transcript, -service, -shell, -tauri, -playback, and npm `@rightkit/legal`, `legal-ui`, `license`, `logs`, `shell`, `updates`.

### 3. Still-to-move table
| # | App code (path, LOC) | Target owner | Notes |
|---|---|---|---|
| G1 | `crates/genright-qts/src/{client 519, worker 226, models 331, protocol 81}.rs` = 1,157 | rightkit-tts@0.1.0 (`client`, `engine` features) + rightkit-qts@0.2.4 | `chunk`/`wav` already re-exported from rightkit-tts (`lib.rs:13`), so inventory 3.8 is done. The rest is the same design but diverged (diff vs `rightkit-tts/src/{protocol,worker,models,client}.rs`: 66, 243, 246, 480 lines). Execution plan G-package says reconcile "keeping the stronger GenRight voice" path. Pins qts =0.2.1 vs 0.2.4 are stale. |
| G2 | Root `rightkit_models::Store` stack callers: `genright-protocol/src/model_store.rs` 67, `genright-local-image/src/profiles.rs` 542, `genright-studio/src/model_management/{hardware 71, install 379, mod 923}.rs`, `genright-qts/src/models.rs:69` (counted in G1) = 1,982 whole-file, about 700 est. touched | `rightkit_models::suite` (the stack CutRight, ScrapeRight, HeardRight use) | CONFIRMS audit 4.3/G30: every GenRight call is the root stack (`rightkit_models::{Store, ModelSpec, RevisionSpec, InstallOptions, Event, Hardware, ...}`), zero `suite::` calls (grep). This is what blocks retiring RightKit's root stack (about 5,093 lines). Persisted formats must stay byte-identical (install records in settings table `models.installs`, pre-store flat layout adoption). |
| G3 | `genright-gen/src/providers/{fal 1,317, wavespeed 1,175, elevenlabs 624, evolink 613, nanobanana 591, heygen 577}.rs`, `genright-studio/src/{voices 2,413, characters 1,258, avatars 1,108, balances 261, model_management/remote 789, pipeline_workflows/asr 183}.rs`, `genright-local-image/src/provider.rs` 423: direct `ureq 3` in 4 manifests (`gen:16, local-image:16, studio:18, src-tauri:40`), 13 files, 31 references | rightkit-http@0.2.4 (timeouts, retry/backoff, Retry-After, SSE, redaction) | about 700 LOC est. touched (HTTP call + error mapping per provider). Scanner-class `http-client` advise hits on all 4 manifests. Provider contract tests: `genright-e2e/tests/paid_contract.rs`. |
| G4 | ffmpeg path resolution duplicated: `genright-gen/src/providers/local_lipsync.rs:156` (`ffmpeg_path`, 5 raw `Command::new`), `src-tauri/src/motion/encode.rs:33` (167 LOC), `genright-studio/src/pipeline_workflows/media.rs` (own `Tools`, about 100) vs the compliant `genright-studio/src/ffmpeg.rs` (118, on rightkit-media) | rightkit-media@0.2.4 (`resolve_ffmpeg`, `verify_ffmpeg`, `dev_resolve`) | about 250 LOC est. Safety: `local_lipsync.rs:157` honours `GENRIGHT_FFMPEG` in release builds, which rightkit-media's resolver refuses. |
| G5 | Whisper CLI wrapper `genright-studio/src/pipeline_workflows/asr.rs` 183 + whisper discovery in `media.rs` | rightkit-asr@0.0.2 (`whisper::cpp`) | execution plan names this reconciliation explicitly. Needs timestamped/word output parity. |
| G6 | Sidecar protocol: `genright-protocol/src/lib.rs` `EngineFrame` set 268, `genright-engine/src/main_stdio.rs` 181, `src-tauri/src/engine_supervisor.rs` 273 = 722 | rightkit-jobs `CommandSet`/`Dispatcher` + `EngineClient` (rightkit-shell `engine` feature), rightkit-framed-sidecar `wire` | hello/request/response/event frames with `rpc_version 1`; CutRight already serves on the rightkit-jobs contract. Internal protocol, CLI contract v2 (`docs/ENGINE-CLI.md`) unaffected. |
| G7 | `genright-core/src/discovery.rs` 352 | rightkit-service `discovery`@0.1.3 | same as CutRight D11; `KNOWN_APPS` list. |
| G8 | SHA-256 helpers: `sha2` direct in 5 crates, file/digest code in 9 files (`library.rs, local_lipsync.rs, musicgen.rs, runtime.rs, store.rs, qts/models.rs, eyes.rs, remote.rs, asr.rs`) | rightkit-fs `digest` (`sha256_reader`, `hex`) | about 150 LOC est. |
| G9 | Raw `std::process::Command` bypassing `OwnedCommand`: `local_lipsync.rs` 5, `local-image/runtime.rs` 3 (of 5), `remote.rs` ssh/scp 2, `media.rs` 1, `motion/{encode,mod}.rs` 2, `engine/worker.rs` 1 | rightkit-process@0.3.3 `OwnedCommand` | about 150 LOC est. (other crates already use it: qts client, realesrgan, supervisor) |
| G10 | Hand-rolled temp+rename atomic writes: `genright-studio/src/voices.rs:176`, `genright-local-image/src/commands.rs:544-557`, `genright-studio/src/pipeline_workflows/asr.rs:156` (all other writers already call `rightkit_fs::atomic_write`) | rightkit-fs 0.3.3 | about 40 LOC est. Inventory 3.7 mostly done. |
| G11 | `tauri-plugin-updater` registered directly (`src-tauri/src/lib.rs:38`, `tauri.conf.json:44`, `capabilities/default.json:12`) with no `@rightkit/updates` policy | @rightkit/updates@0.2.5 | about 50 LOC est. |
| G12 | Hard-coded ONNX Runtime download manifest in `genright-studio/src/model_management/mod.rs:100-135` | rightkit-ort runtime resolution / rightkit-models catalog | about 60 LOC est.; `genright-qts/src/models.rs:222 find_onnxruntime` already defers to `rightkit_ort::developer_override_candidate`. |
| G13 | `scripts/qa-browser.ps1` 64, `qa-functional.mjs` 30, `qa-run.mjs` 39, `qa-shot.mjs` 19, `qa-browser-stop.ps1` 11 | @rightkit/qa-chrome@0.1.3 / @rightkit/qa@0.4.0 | about 160 LOC; PowerShell hidden-browser QA predates RightKit's resolver. Low priority. |

Total in-house duplicates: 13 rows. LOC to move: about 4,500 touched (G1 1,157; G2 about 700 of 1,982; G3 about 700; G4 250; G5 183; G6 722; G7 352; G8 150; G9 150; G10 40; G11 50; G12 60; G13 160 not counted in this sum because it is script QA), or about 5,800 counting whole root-stack files in G2. Blocked on unpublished RightKit crates: none of G1-G12 (all targets have registry records); G2 additionally waits on RightKit finishing the suite API surface GenRight needs (`Hardware`, `RemoteRoute`, `Requirements`, `Measurement`, `ProfileAlias`, `Extract`, `Platform` exist only in the root stack today: check `rightkit_models::suite` exports before porting).

Capability decisions:
| Capability | State | Evidence / action |
|---|---|---|
| agent-experience-gates, agent-room, agent-tools, daemon-host, cloud-sync, desktop-overlay, global-hotkeys, desktop-automation, embeddings, file-watching, framed-sidecar (direct), file-index, mcp, oauth-sign-in, os-sandbox, pdf-engine, pdf-ui, document-rendering, lifecycle-hooks, github-workflows, localization, search, media-playback | not-applicable | engine agent surface is the CLI (`ENGINE-CLI.md`, plan: "no MCP server inside the engine") |
| app-shell-ui | ON (stale 0.2.1) | |
| audio | not-applicable | voices/WAV via rightkit-tts |
| browser-automation | ON (e2e only, rightkit-browser =0.2.2 ok) | |
| browser-qa | DUP (G13) | |
| channel-memory | GAP-not-started (RK6, G7) | GenRight reads it, never stores its own; crate private/unpublished |
| composition-model | not-applicable | motion render is GenRight's own (`src-tauri/src/motion`, 1,000 LOC webview capture + encode); plan says donor capability is published through RightKit first |
| content-contracts | GAP-not-started (RK0) | no brief/envelope code; `rightkit-contracts` staged only |
| content-review | GAP-not-started (RK5, G6) | human eyes gate (`genright-studio/src/eyes.rs` 238) stays app policy; jury goes to rightkit-review |
| crash-reporting | not adopted | no rightkit-crash; rightkit-logs only inside local-image runtime (`runtime.rs:45`) |
| desktop-control | ON (stale 0.1.9) | `src-tauri` `rightkit-control` tauri-plugin |
| desktop-qa | ON (stale 0.2.12) | `genright-e2e` |
| desktop-shell | DUP (G6) + not adopted | `@rightkit/shell` not pinned; no hardening/window-state/single-instance |
| disk-cache | not-applicable | no cache layer found |
| http-client | DUP (G3) | |
| legal-assent, legal-ui | GAP (content-blocked) | `src/lib/onboarding.ts` `LEGAL_STEP_CONFIG.documents: []`, comment "BLOCKED on content": no published GenRight legal pages; `@rightkit/legal`, `legal-ui` not pinned |
| licensing | not wired | no `@rightkit/license`, no rightkit-license; confirm if GenRight is a licensed SKU |
| llm-providers | GAP (partial) | only LLM use is fal `any-llm` gateway (`fal.rs:46-50`); a future text model/vision judge belongs on rightkit-llm (unpublished) |
| local-services | DUP (G7) | |
| media-pipelines | ON for resolution in studio (`ffmpeg.rs`), DUP (G4) elsewhere | rightkit-media =0.2.3 |
| media-ui | ON (stale 0.2.6) | |
| model-installation | ON but WRONG STACK (G2) | |
| ocr | GAP-not-started | carousel acceptance needs rightkit-ocr@0.1.2 (plan) |
| onboarding-ui | ON (stale 0.2.3) | `src/lib/onboarding.ts` |
| onnx-runtime | ON (=0.2.2 ok), minor DUP (G12) | |
| os-credential-store | ON (stale 0.1.0) | `genright-gen/src/credentials.rs` (373, rightkit-secrets with `KeyringCompat`) |
| owned-processes | ON partially (G9) | |
| persistent-jobs | ON (stale 0.2.2) | `genright-gen/src/{store 434, jobs 287, ledger 127}.rs` adopt rightkit-jobs `JobStore` + `StoreHook` (spend ledger) |
| platform-ui | ON (stale 0.1.2) | one scanner-class hit: `src/lib/browser-mock-api.ts` UA sniff (advise) |
| qa-evidence | DUP (G13) | |
| release-tooling | ON (stale 0.2.135) | scripts `mac-dmg.mjs`, `sign-macos-bundle.mjs`, `win-nsis.mjs` are release hooks (check against `docs/rules/release-signing.md`) |
| safe-filesystem | ON (stale 0.3.0) | G8, G10 residue |
| scenario-runtime, screen-recording | GAP-not-started | carousel-from-demo reads rightkit-record bundles (plan) |
| script-alignment | GAP-not-started (RK4) | plan G12 alignment; rightkit-align private/unpublished |
| settings-persistence | ON | `src-tauri/src/settings.rs` (152) on rightkit-settings =0.1.0 (ok); frontend still folds localStorage once (`src/lib/settings-backend.ts`) |
| settings-ui | ON (stale 0.2.7) | |
| shared-speech-service | not-applicable (producer, not consumer) | |
| speech-recognition | DUP (G5) | |
| sqlite-storage | ON (stale 0.3.0) | `rightkit_db::rusqlite` in `store.rs`/`ledger.rs`; no direct rusqlite dep |
| structured-logging | partial | rightkit-logs 0.2.0 in local-image only; no `@rightkit/logs` |
| tauri-host-adapters | not adopted | |
| text-to-speech | DUP (G1) | |
| theme-ui | ON (0.2.1 ok) | `brand:sync` via `rightkit-theme` |
| timed-transcripts | GAP | alignment/captions for presenter master clock plan (RK4); not used today |
| update-policy | DUP (G11) | |
| voice-activity-detection | not-applicable | |

### 4. Ownership scanner
- `right-release ownership --app /Volumes/D/claude/genright --json` exit 1, same `protected-evidence` error as CutRight (checkout has untracked `.claude/` and `model-promotions/`). Saved as `apps-g2-genright-ownership.json`; counts: 0 rows.
- Emulated fingerprints (approximate): block 0; advise 6 hits: `http-client` `ureq` x4 manifests (`genright-gen`, `genright-local-image`, `genright-studio`, `src-tauri`), `platform-ui` UA sniff (`src/lib/browser-mock-api.ts`), `theme-ui` relative-luminance (`scripts/verify-brand-icons.mjs`, likely false positive). Real scanner needs a clean commit.

### 5. Handover list (priority order)
1. Port the model stack off the root `rightkit_models::Store` onto `rightkit_models::suite` (G2) and bump rightkit-models =0.2.2 to 0.2.7; est. 700 LOC touched (1,982 whole files); blocker: confirm `suite` exports (`Hardware`, `Requirements`, `RemoteRoute`, `Extract`) before starting; fixture-dir byte-identity before/after, both OSes. This unblocks RightKit retiring about 5,000 root-stack lines (audit 4.3).
2. Bump all 18 stale pins (process 0.3.3 x5, fs 0.3.3, db 0.3.2, jobs 0.2.6, secrets 0.1.2, media 0.2.4, qa 0.2.14, control 0.1.11, qts 0.2.4 / ggml 0.1.1 / sys 0.1.1; npm app-shell/media-ui/onboarding/settings-ui 0.3.0, platform-ui 0.2.0, release 0.2.147 and add the new versions to `pnpm-workspace.yaml` `minimumReleaseAgeExclude`); est. 60 LOC manifest + UI triage; blocker: 0.x minor UI jumps need e2e + visual check.
3. QTS client/worker/models/protocol (G1) → rightkit-tts@0.1.0 `client`/`engine` + rightkit-qts@0.2.4; est. 1,157 LOC; blocker: diverged code (66-480 diff lines), voice-clone prompt format and `.verified.json` cache semantics must survive.
4. Direct `ureq` in providers (G3) → rightkit-http@0.2.4; est. 700 LOC; blocker: paid-contract mocks (`paid_contract.rs`) must still pass; Retry-After HTTP-date is not parsed by RightKit (`retry.rs:77`, numeric only, capped 30 s).
5. Collapse ffmpeg resolution to `genright-studio/src/ffmpeg.rs` (rightkit-media) in lipsync, motion encode, pipeline media (G4); est. 250 LOC; removes the release-build env override; blocker: none.
6. Engine sidecar protocol (G6) and discovery (G7) → rightkit-jobs@0.2.6 / rightkit-shell@0.1.4 `engine`, rightkit-service@0.1.3; est. 1,074 LOC; blocker: sidecar wire change touches `src/lib/api.ts` consumers and e2e.
7. Whisper CLI wrapper (G5) → rightkit-asr@0.0.2; est. 183 LOC; blocker: word-timestamp parity; also feeds RK4 alignment.
8. Small consolidations (G8 digest, G9 OwnedCommand, G10 atomic write, G12 ORT manifest) → rightkit-fs/process/ort; est. 400 LOC; blocker: none.
9. Updater and legal (G11 + legal): `tauri-plugin-updater` → @rightkit/updates@0.2.5; legal/license: add @rightkit/legal@0.3.1 + legal-ui@0.2.0, but blocked on content (GenRight legal pages do not exist, `onboarding.ts` documents `[]`).
10. Content-pipeline consumption (not started): rightkit-contracts (RK0), rightkit-align (RK4), rightkit-review (RK5), rightkit-channel-memory (RK6), rightkit-record bundle reader (RK3), rightkit-ocr@0.1.2; est. new code; blocker: all except ocr are unpublished; do not build GenRight-local copies.

---

## PostRight

### 1. Shape (honest state: plan-only, no code)
- Repo `/Volumes/D/claude/postright`, last commit `db0276f` 2026-10-10; first commits 2026-10-08. Tracked files: 7 (`.gitignore`, `.audit/arcane/...`, `docs/p0-last-mile-spike-research.md` 265 lines, `docs/p1-platform-apps.md` 475 lines, `docs/plans/2026-10-08-postright-execution-plan.md` 185 lines). No `README`, no `AGENTS.md`.
- `find ... -name Cargo.toml -o -name package.json -o -name Package.swift` (depth 5, excl. node_modules/target): zero manifests. Zero in-house code, zero RightKit pins.
- Planned shape (plan §2): one Rust crate `postright-engine`, desktop (Mac/Windows, Tauri) plus Hetzner daemon (`serve`), no Node/Python at runtime.

### 2. RightKit adoption today
None. Plan names exact pins to consume (`rightkit-jobs`, `-db`, `-secrets`, `-oauth`, `-http`, `-llm`, `-sync`, `-settings`, `-logs`, `-process`; UI `@rightkit/app-shell`, `settings-ui`, `onboarding`, `legal-ui`, `theme`, `media-ui`). Stale pins: 0 (nothing pinned). The plan also says "rightkit-sync is not the transport and is not used for the queue" (§2).

### 3. Still-to-move table (nothing to move; mapping of planned needs)
Priority gaps are RightKit-side. State key: PLAN = plan says consume; GAP = RightKit owner missing or insufficient.
| Capability / need | State | RightKit status (verified in source) |
|---|---|---|
| daemon-host (RK10) | GAP | `rightkit-daemon` 0.1.0, `publish = false`, no registry record (`Cargo.toml:9`); 1,820 LOC; plan: HTTPS listener, device enrolment, resumable uploads, three route classes |
| content-contracts (RK0): brief, delivery envelope, daemon API v1 | GAP | `rightkit-contracts` 0.1.2 staged only; plan P2 says publish waits on vendoring schemas into the crate |
| oauth-sign-in server mode (RK1) | GAP | `rightkit-oauth` 0.3.0 `server` feature exists (README:15), but no Linux secrets backend (`rightkit-secrets/README.md:19`: "Linux: no OS store is wired. Every call fails closed"); provider identity verification, refresh scheduler, Meta 60-day exchange not built |
| os-credential-store on Linux/Hetzner | GAP | same as above |
| http-client | PLAN, GAP (partial) | rightkit-http 0.2.4 is outbound only; `parse_retry_after` is numeric-seconds only (`retry.rs:77`), `max_retry_after` capped at 30 s (`:28`): plan P5 needs durable per-provider quota cooldowns and HTTP-date form |
| llm-providers | PLAN, GAP | rightkit-llm unpublished (no registry record); text-only: plan P3 "visual input gate" needs image/frame input |
| persistent-jobs | PLAN, GAP (partial) | rightkit-jobs 0.2.6 `serve` is stdio; daemon needs HTTPS (RK10) |
| sqlite-storage, os-credential-store (desktop), settings-persistence, structured-logging, owned-processes | PLAN | rightkit-db 0.3.2, -secrets 0.1.2, -settings 0.1.0, -logs 0.2.0, -process 0.3.3 all published |
| crash-reporting | PLAN (not named in plan) | rightkit-crash 0.2.0; plan names logs but not crash: add |
| channel-memory (RK6) | GAP | `rightkit-channel-memory` 0.1.0 private, unpublished |
| content-review (RK5) | GAP | `rightkit-review` private, unpublished; plan P4 quality gate + autonomy ladder sit on it |
| scenario-runtime (RK8), screen-recording (RK3) | GAP | `product_demo` briefs dispatch to rightkit-scenario; both private/unpublished |
| script-alignment (RK4) | GAP | rightkit-align private; plan says local route scaffolded only |
| composition-model (RK9) | not-applicable | PostRight never renders |
| cloud-sync | not-applicable (explicitly not used) | plan §2 |
| local-services | PLAN | desktop to engines on the same machine; rightkit-service 0.1.3 published (same-user sockets/pipes only) |
| agent-tools | PLAN (not named) | `right-ctl` surface; private/unpublished |
| platform capability rules authority (media specs, durations, caption lengths per platform) | GAP, no owner | plan P4: "RightKit owns versioned platform and account capability rules with source dates"; no crate or package exists in INDEX |
| skills execution machinery (resumable steps, tool execution, controlled memory writes) | GAP, no owner | plan P4 "reusable execution machinery ... is a RightKit capability"; none in INDEX |
| Linux native build route (RK7) | GAP | plan: Hetzner broker; "in progress" per `rightkit/docs/plans/2026-10-08-content-pipeline-execution-plan.md:62` |
| app-shell-ui, settings-ui, onboarding-ui, legal-ui, legal-assent, theme-ui, media-ui, update-policy, release-tooling, desktop-shell, tauri-host-adapters, desktop-control, desktop-qa | PLAN / future | all published; PostRight also needs legal pages first (P1 pack records a legal-page gap, `docs/p1-platform-apps.md`) |
| Remaining capabilities (agent-room, audio, browser-*, desktop-automation, desktop-overlay, embeddings, file-watching, framed-sidecar, global-hotkeys, mcp, media-*, model-installation, ocr, onnx-runtime, os-sandbox, pdf-*, search, shared-speech-service, speech-recognition, text-to-speech, timed-transcripts, voice-activity-detection, ...) | not-applicable | plan reads transcripts/analysis from HeardRight and ScrapeRight installed engines; reel decoder uses their engines, not RightKit crates |

In-house duplicates: 0. LOC to move: 0. RightKit gaps that gate PostRight (distinct): RK0 publish, RK1, RK5, RK6, RK7, RK8, RK10, Linux secrets backend, rightkit-llm publish + image input, rightkit-http Retry-After/quota, platform-rules authority, skills-execution machinery (12).

### 4. Ownership scanner
`right-release ownership --app /Volumes/D/claude/postright --json` exit 0, `ok: true`, scanner 2.6.0, catalog 3.0.0, source `committed-git-tree`, commit `db0276f`. 71 rows, all `not-detected` (0 detected, 0 covered), 0 violations, 0 problems, 0 block, 0 advise. Consistent with an empty repo (saved as `apps-g2-postright-ownership.json`).

### 5. Handover list (priority order)
1. Scaffold `postright-engine` from the RightKit desktop-app template and pin exact published versions: rightkit-jobs@0.2.6, -db@0.3.2, -secrets@0.1.2, -settings@0.1.0, -logs@0.2.0, -crash@0.2.0, -process@0.3.3, -http@0.2.4, -fs@0.3.3; est. 500 LOC skeleton; blocker: none for the desktop half.
2. First provider (Bluesky) end-to-end through the daemon; blocked on RK10 (rightkit-daemon unpublished, `publish = false`) and RK7 (Linux native build for Hetzner); est. 800 LOC app code; do not hand-roll an HTTPS listener.
3. OAuth providers → rightkit-oauth@0.3.0 `server`; blocker: RK1 (Linux secrets backend, provider identity verification, refresh scheduling); its redirect-path convention fixes the URIs filed in P1 (Adrian filing; P1 acceptance text says start now).
4. Contracts: consume rightkit-contracts (RK0) for brief/envelope/daemon API; blocker: publish (staged 0.1.2, vendor schemas into crate).
5. LLM skills → rightkit-llm; blockers: unpublished, text-only (visual input gate for P3 style extraction), failover pricing marked unverified.
6. Quota-aware publishing (P5): needs rightkit-http Retry-After HTTP-date + durable cooldown (`retry.rs:77`, 30 s cap); file as a RightKit request, do not fork a client.
7. Channel memory (RK6) and review/autonomy ladder (RK5): consume rightkit-channel-memory and rightkit-review when published; PostRight keeps policy and copy only.
8. File two RightKit requests that have no owner today: versioned platform/account capability rules with source dates; resumable skill-execution machinery.
9. Add rightkit-crash and rightkit-agent-tools (right-ctl) to the plan; neither is mentioned.
10. UI later: @rightkit/app-shell@0.3.0, settings-ui@0.3.0, onboarding@0.3.0, legal-ui@0.2.0, theme@0.2.1, media-ui@0.3.0, updates@0.2.5; blocker: PostRight legal pages (P1 gap).

---

## Cross-app observations
- Three duplicates recur and should be done once in RightKit-owned adapters: installed-app discovery (CutRight 637 + GenRight 352 vs rightkit-service), the sidecar supervisor (CutRight 899 + GenRight 273 + 181 + 268 vs rightkit-jobs `EngineClient`), and hand-rolled SHA-256/atomic writes.
- Suite ort pin is split: CutRight =0.1.4 vs GenRight =0.2.2; two 0.x majors cannot share one dynamic runtime story, so CutRight's ort bump is on the critical path of any single-pin claim.
- Stale-pin totals: CutRight 18 packages (20 lines), GenRight 18 packages (29 lines), PostRight 0.
- `INDEX.md` vs `rightkit-versions.json` disagree on whether asr, ocr, vad, tts, transcript, fsindex, ownership are published; RightKit lead should reconcile before apps are told to pin them.
