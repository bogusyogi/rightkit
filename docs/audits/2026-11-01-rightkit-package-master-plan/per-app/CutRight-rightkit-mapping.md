# CutRight: RightKit mapping

Repo `CutRight` at `f9c7bdd` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`CutRight-rightkit-files.json`](CutRight-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 320 | 94,577 |
| EXTEND | existing RightKit package gains this | 89 | 23,802 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 12 | 1,332 |
| RECIPE | app recipes/app-specific agent skills: stay in the app | 101 | 42,787 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 108 | 40,422 |
| VENDOR-OWN | vendored 3rd-party: own/rewrite in RightKit | 4 | 432 |
| VENDOR-KEEP | vendored 3rd-party: keep as plain dependency, delete vendored copy | 6 | 6,243 |
| DROP | delete | 11 | 513 |
| SHELL | thin app composition that stays in app | 11 | 384 |
| CONFIG | app manifest/config that stays | 4 | 250 |
| NOTCODE | docs/data, not code | 4 | 25,319 |
| **Total** | | **670** | **236,061** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **126,899 lines**. Staying in the app as product code: **0 lines**; as shell/config: **634 lines** (0% of code), plus **40,422 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared presets | 3 | 57 | `./`, `.cargo/` |
| `@rightkit/video-ui` | npm | NEW | project, captions, colour, shorts, recipes, outputs, batch & finished-edit screens | 33 | 9,692 | `apps/cutright/src/components/`, `apps/cutright/src/ui/`, `apps/cutright/src/views/` |
| `@rightkit/video-ui` | npm | NEW | video editor UI: timeline, preview, inspector | 18 | 4,232 | `apps/cutright/src/editor/`, `apps/cutright/src/hooks/` |
| `@rightkit/video-ui` | npm | NEW | client lib & engine bindings | 15 | 2,412 | `apps/cutright/src/lib/` |
| `RightKitMedia` | Swift | NEW | macOS media worker (Swift): AVFoundation render, audio finish, typography, Vision, motion compositor; Silero VAD & vision anchor | 13 | 1,859 | `sidecars/macos-media-worker/`, `sidecars/model-worker/`, `sidecars/render-worker/` |
| `rightkit-agent-demo` | crate | NEW | consented agent demo mode: agent drives a catalog app via rightkit-automation, recorded via rightkit-record, badge, kill hotkey, bundles | 21 | 4,434 | `apps/cutright/src-tauri/src/demo/`, `apps/cutright/src/demo/`, `apps/demo-fixture/` |
| `rightkit-autoedit` | crate | NEW | transcript-driven cut & editorial engines, edit plans, named variants, style measurement, mic alignment | 45 | 11,894 | `crates/cutright-cuts/`, `crates/cutright-cuts/src/`, `crates/cutright-cuts/tests/` +7 dirs |
| `rightkit-autoedit` | crate | NEW | scoped editing rules & resolution matrix, autonomy levels | 10 | 2,385 | `crates/cutright-engine/src/`, `crates/cutright-rules/`, `crates/cutright-rules/src/` +1 dirs |
| `rightkit-captions` | crate | NEW | caption styles, per-language caption sets, placement, swatch preview | 18 | 3,177 | `crates/cutright-captions/`, `crates/cutright-captions/src/`, `crates/cutright-captions/tests/` +1 dirs |
| `rightkit-color` | crate | NEW | colour grading & scopes | 13 | 4,108 | `crates/cutright-color/`, `crates/cutright-color/src/`, `crates/cutright-color/tests/` +1 dirs |
| `rightkit-compositor` | crate | NEW | native wgpu layer compositor, GPU effects & SDF masks (adapted from OpenCut, MIT) | 27 | 3,088 | `crates/cutright-compositor/`, `crates/cutright-compositor-effects/`, `crates/cutright-compositor-effects/src/` +9 dirs |
| `rightkit-doctor` | crate | NEW | doctor report | 1 | 177 | `crates/cutright-engine/src/` |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 2 | 260 | `crates/cutright-engine/src/` |
| `rightkit-pipeline` | crate | NEW | cross-engine orchestration: call generation engine, follow jobs, place results | 12 | 9,024 | `crates/cutright-engine/src/`, `crates/cutright-genright/`, `crates/cutright-genright/examples/` +3 dirs |
| `rightkit-pipeline` | crate | NEW | brief check/run, channel recipes, unattended recipe runs, durable batch delivery | 5 | 4,729 | `crates/cutright-engine/src/` |
| `rightkit-reframe` | crate | NEW | tracked 9:16 reframe: ONNX face detect, tracking, smoothed crop path | 14 | 4,222 | `crates/cutright-reframe/`, `crates/cutright-reframe/src/`, `crates/cutright-reframe/tests/` |
| `rightkit-shorts` | crate | NEW | shorts finder & scoring, on-screen-text moment search | 18 | 4,992 | `crates/cutright-engine/src/`, `crates/cutright-shorts/`, `crates/cutright-shorts/src/` +1 dirs |
| `rightkit-social-analytics` | crate | NEW | retention feedback import & analysis | 1 | 1,300 | `crates/cutright-engine/src/` |
| `rightkit-timeline` | crate | NEW | portable timeline core: tracks, clips, rational time, edits, undo | 17 | 6,032 | `crates/cutright-timeline/`, `crates/cutright-timeline/src/`, `crates/cutright-timeline/tests/` +1 dirs |
| `rightkit-timeline` | crate | NEW | FCPXML & OpenTimelineIO export | 7 | 1,979 | `crates/cutright-fcpxml/`, `crates/cutright-fcpxml/src/`, `crates/cutright-fcpxml/tests/` +3 dirs |
| `rightkit-video-project` | crate | NEW | video project layer: sources & integrity, revisions, portable bundle export/import, card slots, cache storage | 26 | 10,357 | `crates/cutright-engine/src/`, `crates/cutright-project/`, `crates/cutright-project/examples/` +2 dirs |
| `rightkit-video-project` | crate | NEW | media workflow methods: projects, transcription, render | 1 | 4,167 | `crates/cutright-engine/src/` |
| `@rightkit/legal` | npm | EXTEND | corresponding-source build | 1 | 308 | `scripts/legal/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 2 | 73 | `apps/cutright/src/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 201 | `apps/cutright/` |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 1 | 371 | `apps/cutright/src/` |
| `rightkit-align` | crate | EXTEND | word timings + VAD for talking-head (on rightkit-asr/vad/align) | 10 | 2,477 | `crates/cutright-speech/`, `crates/cutright-speech/examples/`, `crates/cutright-speech/src/` |
| `rightkit-compose` | crate | EXTEND | evaluated timeline -> compositor frame descriptors | 20 | 5,679 | `crates/cutright-compose/`, `crates/cutright-compose/src/`, `crates/cutright-compose/tests/` |
| `rightkit-daemon` | crate | EXTEND | installed suite-app discovery | 1 | 567 | `crates/cutright-engine/src/` |
| `rightkit-ffmpeg` | crate | EXTEND | ffmpeg editing build staging & qualification | 5 | 1,338 | `tools/ffmpeg-editing/`, `tools/qualify/` |
| `rightkit-models` | crate | EXTEND | runtime pack builders (ffmpeg, llama, whisper, parakeet, silero, kokoro, ORT staging, model conversion) | 10 | 662 | `scripts/runtime/` |
| `rightkit-motion` | crate | EXTEND | contact sheets & thumbnails | 2 | 626 | `crates/cutright-engine/src/` |
| `rightkit-playback` | crate | EXTEND | native playback session over evaluated revision | 2 | 1,819 | `apps/cutright/src-tauri/src/` |
| `rightkit-review` | crate | EXTEND | final-render QA gate & social packaging checks | 16 | 5,035 | `crates/cutright-qa/`, `crates/cutright-qa/src/`, `crates/cutright-qa/src/bin/` +1 dirs |
| `rightkit-spend` | crate | EXTEND | productivity/command ledger & preference policy | 15 | 4,345 | `crates/cutright-ledger/`, `crates/cutright-ledger/src/`, `crates/cutright-ledger/tests/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 2 | 298 | `apps/cutright/src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 3 | `apps/cutright/src-tauri/` |
| `@rightkit/legal` | npm | ADOPT | legal | 1 | 87 | `apps/cutright/src/legal/` |
| `@rightkit/release` | npm | ADOPT | gates & sample validation | 3 | 320 | `scripts/`, `scripts/gates/`, `scripts/release/` |
| `rightkit-crash` | crate | ADOPT | crash | 1 | 35 | `apps/cutright/src-tauri/src/` |
| `rightkit-fs` | crate | ADOPT | paths | 1 | 137 | `crates/cutright-engine/src/` |
| `rightkit-license` | crate | ADOPT | licence | 1 | 126 | `apps/cutright/src-tauri/src/` |
| `rightkit-logs` | crate | ADOPT | logs | 1 | 105 | `apps/cutright/src-tauri/src/` |
| `rightkit-models` | crate | ADOPT | model store adapter | 3 | 246 | `crates/cutright-engine/src/`, `crates/cutright-model-store/`, `crates/cutright-model-store/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | engine host | 1 | 276 | `apps/cutright/src-tauri/src/` |
| `@rightkit/video-ui` | npm | VENDOR-OWN | vendored hyperframes player bits | 4 | 432 | `apps/cutright/src/editor/vendor/hyperframes/` |
| `rightkit-typeset` | crate | VENDOR-KEEP | CSL style parser (MIT): keep as crates.io dep of typeset/citations | 6 | 6,243 | `vendor/citationberg/`, `vendor/citationberg/src/` |
| `none` | other | DROP | provenance copies of imported skills/behaviour notes (reference only) | 11 | 513 | `imports/provenance/cutaway-finish/cutaway/scripts/`, `imports/provenance/cutaway-finish/finish/scripts/` |
| `app-recipes` | other | RECIPE | app recipes (editing/design recipes & scripts the app ships for agents): stay in CutRight | 101 | 42,787 | `cutaway/scripts/`, `skills/brand-identity/scripts/`, `skills/content/specialists/image-enhancement/scripts/` +19 dirs |
| `app-shell` | other | APPTEST | app journey/e2e tests | 107 | 40,062 | `apps/cutright/src-tauri/tests/`, `apps/cutright/src-tauri/tests/support/`, `crates/cutright-engine/tests/` +4 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 1 | 360 | `crates/cutright-engine/examples/` |
| `app-shell` | other | SHELL | tauri entry & grants | 3 | 177 | `apps/cutright/src-tauri/`, `apps/cutright/src-tauri/.cargo/` |
| `app-shell` | other | SHELL | app entry | 7 | 140 | `apps/cutright/`, `apps/cutright/scripts/lib/`, `apps/cutright/src/` |
| `app-shell` | other | SHELL | engine binary & method table | 1 | 67 | `crates/cutright-engine/` |
| `app-shell` | other | CONFIG | app manifests | 4 | 250 | `./`, `bindings/ts/`, `finish/scripts/` +1 dirs |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 3 | 25,040 | `apps/cutright/legal/` |
| `none` | other | NOTCODE | doc/prototype page | 1 | 279 | `./` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `apps/cutright/src-tauri/Cargo.toml` | 122 |
| `crates/cutright-engine/Cargo.toml` | 67 |
| `apps/cutright/src-tauri/deny.toml` | 47 |
| `apps/cutright/about.toml` | 28 |
| `apps/cutright/src/brand.css` | 28 |
| `apps/cutright/vite.config.ts` | 27 |
| `apps/cutright/scripts/lib/idempotent-copy.mjs` | 19 |
| `apps/cutright/scripts/lib/target-root.mjs` | 18 |
| `apps/cutright/index.html` | 13 |
| `apps/cutright/src-tauri/.cargo/audit.toml` | 8 |
| `apps/cutright/src/vite-env.d.ts` | 7 |
