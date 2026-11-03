# GenRight: RightKit mapping

Repo `genright` at `8e7208d` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`genright-rightkit-files.json`](genright-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 232 | 108,784 |
| EXTEND | existing RightKit package gains this | 70 | 25,482 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 40 | 4,118 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 86 | 27,033 |
| DROP | delete | 64 | 8,627 |
| SHELL | thin app composition that stays in app | 16 | 629 |
| CONFIG | app manifest/config that stays | 9 | 252 |
| NOTCODE | docs/data, not code | 3 | 7,261 |
| **Total** | | **520** | **182,186** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **147,011 lines**. Staying in the app as product code: **0 lines**; as shell/config: **881 lines** (0% of code), plus **27,033 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared presets | 3 | 96 | `./`, `.cargo/` |
| `@rightkit/media-gen-ui` | npm | NEW | generation studio UI: generate forms, gallery & library, jobs, recipes & templates, character/quality/video panels, voices, avatars, motion, production, pipelines, KDP, local image chat | 117 | 21,626 | `src/`, `src/components/`, `src/legal/` +6 dirs |
| `rightkit-approvals` | crate | NEW | approval tokens & cards for paid drafts/deliveries (minted by shell, verified by engine) | 3 | 1,485 | `crates/genright-protocol/src/`, `crates/genright-studio/src/`, `src-tauri/src/` |
| `rightkit-blender-worker` | crate | NEW | Blender render worker (Python, runs inside Blender): scene, materials, product registration/review, finish profiles, contract | 29 | 4,386 | `workers/blender/`, `workers/blender/renderright/`, `workers/blender/tests/` +1 dirs |
| `rightkit-doctor` | crate | NEW | doctor command | 1 | 184 | `crates/genright-engine/src/` |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 7 | 2,813 | `crates/genright-core/src/`, `crates/genright-engine/src/`, `crates/genright-protocol/src/` |
| `rightkit-library` | crate | NEW | owned media library: import w/ provenance, asset facts (origin/evidence/rights), FTS search, artifact index | 7 | 4,596 | `crates/genright-core/src/`, `crates/genright-engine/src/`, `crates/genright-gen/src/` +1 dirs |
| `rightkit-local-gen` | crate | NEW | keyless local image gen: sd.cpp server runtime, memory modes, profiles, job store, artifacts | 15 | 9,532 | `crates/genright-engine/src/`, `crates/genright-gen/src/providers/`, `crates/genright-local-image/` +1 dirs |
| `rightkit-local-gen` | crate | NEW | local Wav2Lip talking-head lip-sync on ONNX | 1 | 1,244 | `crates/genright-gen/src/providers/` |
| `rightkit-local-gen` | crate | NEW | local MusicGen ONNX music generation | 1 | 952 | `crates/genright-gen/src/providers/` |
| `rightkit-local-gen` | crate | NEW | local Real-ESRGAN image upscale | 1 | 584 | `crates/genright-gen/src/providers/` |
| `rightkit-media-fetch` | crate | NEW | stock & archive acquisition (Pexels, Pixabay, Internet Archive) w/ licence facts | 1 | 1,362 | `crates/genright-gen/src/` |
| `rightkit-media-gen` | crate | NEW | generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, HeyGen avatars, EvoLink, Higgsfield, Muapi, NanoBanana, Replicate, Wan2GP, Blender), model catalogue & facts, routing, durable gen jobs, result descriptors, balances | 25 | 34,258 | `crates/genright-gen/src/`, `crates/genright-gen/src/providers/`, `crates/genright-studio/src/` |
| `rightkit-media-pipeline` | crate | NEW | named production pipelines: compose, dubbing w/ retiming, translation w/ terminology, fan-out multi-job steps, finishing handoff, campaign factory | 8 | 9,149 | `crates/genright-studio/src/`, `crates/genright-studio/src/pipeline_workflows/` |
| `rightkit-pipeline` | crate | NEW | brief run / delivery envelope handling between suite engines | 1 | 1,701 | `crates/genright-engine/src/` |
| `rightkit-prompt-recipes` | crate | NEW | prompt contracts & validator, recipe composer & catalogue, characters/LoRA lock, video quality templates, template swap, brand codes, KDP workflow | 10 | 12,135 | `crates/genright-studio/src/` |
| `rightkit-remote-compute` | crate | NEW | rented GPU workers (Vast): register/stage/run lifecycle, time-billed spend | 2 | 2,681 | `crates/genright-studio/src/model_management/` |
| `@rightkit/ax` | npm | EXTEND | agent plugin evals | 1 | 246 | `plugins/genright/evals/` |
| `@rightkit/media-ui` | npm | EXTEND | mask brush & export | 2 | 282 | `src/components/`, `src/lib/` |
| `@rightkit/qa` | npm | EXTEND | browser mock API for UI QA | 7 | 4,923 | `src/lib/`, `src/views/`, `src/views/panels/` |
| `@rightkit/react-shell` | npm | EXTEND | workspace frame, confirm, nav, error note | 8 | 788 | `src/components/`, `src/lib/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 179 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | API keys, balances, models, rentals, routing & spend reconcile panels | 19 | 3,617 | `src/components/`, `src/lib/`, `src/views/` |
| `@rightkit/theme` | npm | EXTEND | brand icon verification | 2 | 199 | `scripts/` |
| `rightkit-compose` | crate | EXTEND | keyless still finishing composite | 1 | 614 | `crates/genright-engine/src/` |
| `rightkit-daemon` | crate | EXTEND | installed suite-app discovery | 1 | 384 | `crates/genright-core/src/` |
| `rightkit-ffmpeg` | crate | EXTEND | pure-Rust media probe (dims, duration, codec) | 1 | 759 | `crates/genright-gen/src/` |
| `rightkit-ffmpeg` | crate | EXTEND | single H.264 encoder rule | 1 | 135 | `crates/genright-protocol/src/` |
| `rightkit-job-queue` | crate | EXTEND | caller leases & detached job runners | 2 | 501 | `crates/genright-engine/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: See docs/rightkit-exceptions/M12.md#2-device-identity-source (no owner implements rightkit_license::DeviceIdSo | 1 | 68 | `src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: See docs/rightkit-exceptions/M12.md#1-licence-identity-constants (the licence host and the signer's public key | 1 | 30 | `crates/genright-protocol/src/` |
| `rightkit-models` | crate | EXTEND | memory-aware governor for heavy local models, hardware fit, install, Blender/Wan2GP/sd.cpp doctor | 7 | 2,941 | `crates/genright-studio/src/model_management/` |
| `rightkit-motion` | crate | EXTEND | HyperFrames HTML composition render: loopback host, headless Chrome capture, encode | 7 | 2,619 | `crates/genright-engine/src/`, `crates/genright-engine/src/motion_assets/`, `crates/genright-studio/src/` |
| `rightkit-review` | crate | EXTEND | human-eyes review gate | 1 | 683 | `crates/genright-studio/src/` |
| `rightkit-spend` | crate | EXTEND | gen spend ledger GUI+CLI | 1 | 2,755 | `crates/genright-gen/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 4 | 472 | `src-tauri/src/`, `src-tauri/src/bin/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 3 | `src-tauri/` |
| `rightkit-tts` | crate | EXTEND | voice library: clones, presets, provider voices | 1 | 3,284 | `crates/genright-studio/src/` |
| `@rightkit/onboarding` | npm | ADOPT | onboarding, legal, licence plan, device allowance | 8 | 662 | `src/lib/`, `src/views/` |
| `@rightkit/tauri` | npm | ADOPT | adapters | 8 | 192 | `src/lib/` |
| `rightkit-agent-tools` | crate | ADOPT | agent tool profile | 1 | 302 | `src-tauri/src/` |
| `rightkit-app-settings` | crate | ADOPT | settings get/patch | 1 | 222 | `crates/genright-engine/src/` |
| `rightkit-asr` | crate | ADOPT | whisper ASR | 1 | 314 | `crates/genright-studio/src/pipeline_workflows/` |
| `rightkit-credentials` | crate | ADOPT | provider API keys | 2 | 379 | `crates/genright-gen/src/`, `crates/genright-protocol/src/` |
| `rightkit-ffmpeg` | crate | ADOPT | ffmpeg discovery | 2 | 77 | `crates/genright-protocol/src/`, `crates/genright-studio/src/` |
| `rightkit-fs` | crate | ADOPT | roots & path policy | 2 | 128 | `crates/genright-core/src/`, `crates/genright-protocol/src/` |
| `rightkit-http` | crate | ADOPT | outbound HTTP policy | 1 | 176 | `crates/genright-protocol/src/` |
| `rightkit-license` | crate | ADOPT | Pro gate & trial | 2 | 203 | `crates/genright-engine/src/`, `crates/genright-gen/src/` |
| `rightkit-logs` | crate | ADOPT | diagnostics events | 2 | 62 | `crates/genright-protocol/src/`, `src-tauri/src/` |
| `rightkit-models` | crate | ADOPT | model store glue | 1 | 67 | `crates/genright-protocol/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | engine host | 2 | 280 | `src-tauri/src/` |
| `rightkit-tts` | crate | ADOPT | local Qwen3-TTS voice | 7 | 1,054 | `crates/genright-gen/src/providers/`, `crates/genright-qts/`, `crates/genright-qts/examples/` +2 dirs |
| `none` | other | DROP | legacy Node Media Console server & runners superseded by Rust studio port (confirm) | 49 | 7,651 | `bin/`, `compat-lib/`, `scripts/` +2 dirs |
| `none` | other | DROP | archived one-off Blender lookdev scripts | 15 | 976 | `workers/blender/archive/fenrir-measurements/scripts/`, `workers/blender/archive/fenrir-v2/scripts/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 83 | 26,319 | `crates/genright-e2e/`, `crates/genright-e2e/src/`, `crates/genright-e2e/tests/` +2 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 3 | 714 | `crates/genright-engine/tests/`, `crates/genright-studio/examples/` |
| `app-shell` | other | SHELL | engine binary, CLI surface & app contract | 15 | 570 | `crates/genright-core/`, `crates/genright-engine/`, `crates/genright-gen/` +3 dirs |
| `app-shell` | other | SHELL | tauri entry | 1 | 59 | `src-tauri/` |
| `app-shell` | other | CONFIG | app manifests | 9 | 252 | `./`, `assets/brand/review/`, `assets/brand/review/v2/` +2 dirs |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 1 | 6,672 | `legal/` |
| `none` | other | NOTCODE | documentation pages | 2 | 589 | `docs/design/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `crates/genright-studio/data/motion/blank.html` | 59 |
| `src-tauri/Cargo.toml` | 59 |
| `crates/genright-engine/Cargo.toml` | 52 |
| `crates/genright-studio/data/motion/stat-reveal.html` | 45 |
| `crates/genright-gen/Cargo.toml` | 43 |
| `crates/genright-studio/data/motion/title-card.html` | 42 |
| `crates/genright-studio/Cargo.toml` | 38 |
| `crates/genright-studio/data/motion/vertical-hook.html` | 38 |
| `crates/genright-studio/data/motion/timer.html` | 37 |
| `crates/genright-studio/data/motion/end-card.html` | 36 |
| `crates/genright-studio/data/motion/caption-card.html` | 35 |
| `crates/genright-studio/data/motion/lower-third.html` | 35 |
| `crates/genright-studio/data/motion/callout.html` | 33 |
| `crates/genright-studio/data/motion/logo-reveal.html` | 33 |
| `crates/genright-protocol/Cargo.toml` | 30 |
| `crates/genright-core/Cargo.toml` | 14 |
