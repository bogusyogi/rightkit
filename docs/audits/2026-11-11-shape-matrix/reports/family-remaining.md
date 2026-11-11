# Family review: remaining packages

Read-only sweep of the 24 packages no family review covered. Depth is light unless stated; `deep` means code was read on both sides. Each claim is verified by reading or grep unless marked suspicion or not verified. Nothing was built or run.

Paths: `S` = `<scratch>`; app trees are under `S/survey/<app>`; RightKit is `/home/user/rightkit`. Planned lines are from `S/coverage-rows.json`. Registry state is from `S/registry.json` and `S/npm-registry.json`.

Limits: Legion source is not available. The surveyed CodeRight tree is not the commit its inventory was made from (inventory commit 0a85471d; 646 of 3,193 listed paths are absent), so CodeRight line counts are the plan's, not the tree's.

## Summary

| Shape | Count | Packages |
|---|---|---|
| best | 6 | `@rightkit/hooks`, `@rightkit/issues-tracker`, `rightkit-contrast`, `rightkit-signal`, `RightKitFS`, `RightKitHotkeys` |
| gaps | 15 | `@rightkit/blender-worker`, `@rightkit/build-control-policy`, `@rightkit/outdated`, `@rightkit/send-ui`, `@rightkit/sync-worker`, `rightkit-align`, `rightkit-android`, `rightkit-platform-rules`, `rightkit-policy-rules`, `rightkit-sysmem`, `rightkit-dictation`, `RightKitHTTP`, `RightKitHub`, `RightKitUpdate`, `RightKitSwift` |
| wrong | 3 | `@rightkit/ml-lab`, `@rightkit/dictation-ui`, `rightkit-owner-approvals` |
| missing | 0 | none |

Main findings:

1. `@rightkit/dictation-ui` is the wrong shape. It is HeardRight main's webview source copied file for file (83 twins, 542 of 10,555 lines differ) including licence, auto-update, theme, legal, sidebar and title-bar code that other packages own, with CodeRight's unrelated voice loop beside it. No app depends on it, and it is already on the npm registry at 0.1.0.
2. `RightKitSwift` is in use by four apps at 0.8.1, but 62% of the Swift lines the plan maps (92,690 of 148,357) point at six modules that do not exist (Agent, SystemUI, Notch, Dictation, Library, Media), and 26,478 lines of app tests point at `RightKitHubQA`, which is desktop Hub fakes.
3. `RightKitHub` is Pulse's code moved unchanged, yet Pulse still builds from its local copy, which contains a module (`RightKitPresentation`, 888 lines) that RightKitSwift lacks.
4. `RightKitUpdate`'s Mac installer lacks Pulse's swap journal, install lock and finish-after-crash step; Pulse would lose them by adopting it.
5. `rightkit-sysmem` claims to be the one home for the platform memory calls; nothing depends on it and six other RightKit crates declare the same FFI.
6. `@rightkit/sync-worker` has contradictory status: the workspace file says parked, its README says live and supported.
7. Duplicates inside RightKit: nearby-sharing UI (`send-ui` and `system-ui/NearbySettings.tsx`), agent policy (`rightkit-policy-rules` and `rightkit-agent-policy`), platform limits (`rightkit-platform-rules` and three video crates), approvals (`rightkit-owner-approvals` and three others, per family-agent-platform.md).
8. Plan mapping errors: CutRight's ASR crate under `rightkit-align`; generated `issues.html` under `@rightkit/issues-tracker`; CodeRight credential stores under `RightKitFS`; CodeRight daemon client and URL-callback handler under `RightKitHTTP`; Pulse's Fn-as-Command tweak under `RightKitHotkeys`; PostRight's `api_versions.rs` under `rightkit-platform-rules`.
9. Six are ready as they are: `@rightkit/hooks`, `@rightkit/issues-tracker`, `rightkit-contrast`, `rightkit-signal`, `RightKitFS`, `RightKitHotkeys`.

## npm packages

### @rightkit/blender-worker

1. **Exists as:** packages/blender-worker (@rightkit/blender-worker, Python source, GPL-3.0-or-later)
2. **Planned feeds (lines):** genright 4,386
3. **Holds today:** Not re-reviewed. Verdict copied from the row `rightkit-blender-worker` in matrix-rows-video-generative.json (Generative media family). That row's basis: GenRight workers/blender moved 2026-11-01 (npm wrapper around Python)
4. **Across apps:** Copied row; see family-video-generative.md. On the npm registry at 0.1.1 (npm-registry.json).
5. **Shape: gaps** (reference: genright; depth light; confidence medium). Job protocol, schemas and scene authoring are a usable worker. It also ships one product's geometry and look development.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | genright | fits | 3,216 of 3,493 distinctive lines present in RightKit (coverage tgt shows 0 only because the package name differs from the planned crate name) |
   | cutright | n/a |  |
   | heardright | n/a |  |
   | viewright | n/a |  |

7. **Boundary problems:** 
   - fenrir_geometry.py, fenrir registration reference, recipes/samples/fenrir-sample-product.json, stonewash.py + build_stonewash_library, editorial_look/premium_look/approved_materials: 296 "fenrir" and 164 "stonewash" occurrences; job.schema.json mentions fenrir
   - Python package named `renderright`
   - GPL-3.0 package beside MIT/Apache crates; providers/local_blender.rs (2,040 lines, in media-gen) is its MIT-side driver: licence boundary is the process boundary and should be stated
   - Planned as cargo `rightkit-blender-worker`; exists as npm
8. **Before publication:** 
   - Split generic worker (contract, worker, scene, pngcheck, install, qualify, schemas) from product look-dev (fenrir, stonewash, product_* modules) which becomes a host-supplied assets/recipes pack via RIGHTKIT_BLENDER_ASSETS_DIR
   - Fix the plan name to the npm package

### @rightkit/ml-lab

1. **Exists as:** packages/ml-lab
2. **Planned feeds (lines):** heardright 53,827
3. **Holds today:** Not re-reviewed. Verdict copied from the row `rightkit-ml-lab` in matrix-rows-context-tooling.json (Foundation & tooling family). That row's basis: HeardRight training and evaluation scripts copied with their directory layout
4. **Across apps:** Copied row; see family-context-tooling.md. Marked private, not on the registry.
5. **Shape: wrong** (reference: heardright; depth deep; confidence high). A private, unbuilt file dump of one product's experiments: no entry points, no dependency manifest, machine paths, dated experiment folders. Not a shared package and not publishable.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | wrong | moving the scripts here gives nothing; they still refer to heardright-engine source files and D:\Claude\heardright paths |

7. **Boundary problems:** 
   - 60 files hold machine paths (D:\Claude 86 times, E:\heardright_archive, /Users/<name>)
   - 183 files refer to heardright; schema ids heardright.l1.*
   - scripts read ../../heardright-engine/src/*.rs which is not in the package
   - 23 PowerShell scripts delete or quarantine training data on a specific machine
   - no requirements.txt or pyproject for 158 Python files (torch, numpy)
   - score-asr-wer.mjs and the ASR adapter harness overlap the bench receipt shape
   - plan lists it as a crate; it is an npm package marked private
8. **Before publication:** 
   - do not publish
   - keep experiments (bakeoff/*, tools/zephyr_*, local-model-training) in a HeardRight research repository
   - extract only reusable pieces: WER scoring and ASR adapter harness to the bench package, CoreML/ONNX conversion to a model-tools package with a pinned Python manifest

### @rightkit/build-control-policy

1. **Exists as:** packages/build-control-policy (private, 0.1.5-phase1, 4,655 lines of .mjs)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** RightKit's own agent and build admission tooling (ownership write hooks, session hook, heavy-write guard, host gate); no app code
4. **Across apps:** No app implements this; it is suite tooling. Three apps call the `rightkit-host-gate` bin from their scripts.
5. **Shape: gaps** (reference: none; depth light; confidence medium). Correctly private and host-installed, not a dependency. But three apps' package.json scripts call its `rightkit-host-gate` bin, and the package carries the list of the owner's private repositories and machine roots in source.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | tauri-app-next/package.json scripts call rightkit-host-gate (main only) [verified by grep] |
   | pulse | fits | package.json:5-20, every dev/test/package/release script goes through rightkit-host-gate [verified] |
   | coderight | fits | apps/coderight-tauri/package.json:8-9 [verified] |
   | viewright | n/a | no reference found |

7. **Boundary problems:** 
   - src/ownership-repositories.json names the private app repositories (bogusyogi/coderight, cutright, genright, heardright, mailright, ...) [verified]
   - Machine paths in host-gate.mjs, ownership-write-hook.mjs, install-policy-assets.mjs, index.mjs (D:\.rightkit-managed, ~/Library/Application Support/RightKit) [verified by grep]
   - agent-heavy-write-guard.mjs is 2,279 of 4,655 lines: a macOS write guard, a different job from build admission [suspicion that it should be its own unit; not read]
   - Not in any app's dependencies: the bin reaches apps only through the managed host install
8. **Before publication:** 
   - Do not publish; keep private and say so in the matrix
   - Record that app scripts depend on a host-installed bin, so a fresh machine needs the install step before any app script runs
   - Version string 0.1.5-phase1: settle a normal version before anything else pins it

### @rightkit/dictation-ui

1. **Exists as:** packages/dictation-ui (0.1.0, on the npm registry, 11,683 lines)
2. **Planned feeds (lines):** heardright 8,675, coderight 1,090
3. **Holds today:** HeardRight main's webview front end copied file for file (83 files have a same-path twin in heardright-main/tauri-app-next/src; 542 of 10,555 lines differ, mostly `.js` import suffixes and renamed helpers), plus CodeRight's voice-conversation loop under src/voice (3 files)
4. **Across apps:** HeardRight's pill/pages and CodeRight's voice loop are different capabilities, so there is no behaviour conflict to settle. HeardRight: recording pill window driven by engine events (pill/usePillEvents.ts, 569 lines) and settings pages over Tauri commands. CodeRight (as captured in src/voice): `useMicRecorder` opens getUserMedia + MediaRecorder with a level meter and silence detection (voice/hooks/useMicRecorder.ts:158-186); `useVoiceConversation` runs listen, transcribe (host callback `onTranscribeAudio`), submit, speak (window.speechSynthesis by default) with barge-in (voice/hooks/useVoiceConversation.ts:22-56). CodeRight's tree today replaced this with host dictation (apps/coderight-tauri/src/hooks/useDictation.ts:43-46). Neither earlier HeardRight review (hr-review-dictation-chain.md, hr-review-settings-onboarding-shell.md) mentions this package: the first covers the Rust crates only, the second shows main using @rightkit/settings-ui and react-shell directly.
5. **Shape: wrong** (reference: heardright; depth deep; confidence high). It is one app's whole front end (sidebar, title bar, licence, auto-update, theme, legal, settings registry, pill, pages) with a second app's unrelated voice loop beside it. No app depends on it; HeardRight main keeps the same files, and CodeRight's current tree no longer has the voice files.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | wrong | main does not depend on it and holds the same 83 files; adopting means importing its own src back, through a Vite alias `@rightkit-host` to its own legal files. LIVE lacks 8 of the files (settingsRegistry, fieldStore, modelUpdate, pushToTalk, ...) |
   | coderight | wrong | to use src/voice it must install HeardRight's shell with @rightkit/license, app-update, settings-ui, legal. Its current composer dictation (hooks/useDictation.ts, lib/voice/dictation.ts) asks the host (`voice_availability`, state `needs_heardright`) and has no mic loop of its own |
   | scraperight | gaps | reaches it only through @rightkit/library-ui, which imports PageShell, ErrorBanner, fieldStore, historyUtils, AppIcon, invoke from here (see family-content-ingest-mail.md) |
   | viewright | n/a |  |
   | pulse | n/a |  |

7. **Boundary problems:** 
   - Holds code other packages own: lib/license*, lib/license/* (@rightkit/license), lib/autoUpdates.ts and heardRightAutoUpdateClient.ts (@rightkit/app-update), lib/theme.ts (@rightkit/theme), lib/legal.ts (@rightkit/legal), Sidebar/TitleBar/WindowControls (@rightkit/react-shell), settings rows (@rightkit/settings-ui) [verified by file list]
   - HeardRight identifiers in the package: theme code `heardright`, storage keys `heardright.theme`, `heardright.skipDeleteHistoryConfirm`, `heardright.historySourceSelection.v1`, event `heardright://renderer-event` (lib/theme.ts:12,91; pages/history/historyUtils.ts:4,36; lib/tauriBridge.ts:14) [verified]
   - 59 distinct Tauri command names are invoked directly; there is no client interface a second host could implement [verified by grep]
   - Generated barrel leaves 8 modules out because of export-name collisions (README)
   - History helpers (pages/history/AppIcon, historyUtils) live here while the History page lives in @rightkit/library-ui
   - src/voice (useMicRecorder, useVoiceConversation, VoiceConversation) is a browser MediaRecorder + speechSynthesis conversation loop for an agent chat; it shares nothing with the dictation pill or pages
   - The CodeRight files the plan maps here (inventory commit 0a85471d) are absent from the surveyed CodeRight tree; 646 of 3,193 inventory paths are missing there
8. **Before publication:** 
   - Redraw as three units: (a) dictation pill overlay (pill/*, hint catalog) with an event/command interface; (b) dictation pages (Home, Dictation, Vocabulary, Polish, replacements, snippets) over a typed `DictationClient` the host supplies; (c) voice conversation (src/voice) moved to the agent UI family or its own package
   - Delete the licence, auto-update, theme, legal, sidebar, title-bar and window-control copies; use the owning packages
   - Storage keys, event names and theme code from host configuration, not `heardright.*` literals
   - Move history helpers next to the History page in @rightkit/library-ui and drop that package's dependency on this one
   - Decide what happens to the published 0.1.0 (deprecate once the split packages exist)
   - Then HeardRight deletes its 83 twins; until then this package is a duplicate of HeardRight's src

### @rightkit/hooks

1. **Exists as:** packages/hooks (0.1.2, on the registry, 102 lines); crates/rightkit-hooks (Rust pair)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Fresh RightKit implementation: normalise a hook payload, run modules in order under a deadline
4. **Across apps:** Single implementation. Only RightKit's e2e-app depends on it.
5. **Shape: best** (reference: none; depth light; confidence medium). Small, neutral, no policy and no app names. Apps that have hook code use the Rust crate; nothing app-side maps to the npm package.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | n/a | uses the crate rightkit-hooks =0.2.0 (core/Cargo.toml:46, membrane-runtime/Cargo.toml:27), not this package |
   | coderight | n/a | engine/crates/hooks is Rust |
   | heardright | n/a |  |

7. **Boundary problems:** 
   - rightkit-ownership.json still carries the note `stdin repair remains P1` while the README lists no known issue [verified]
8. **Before publication:** 
   - Resolve or delete the stale P1 note

### @rightkit/issues-tracker

1. **Exists as:** packages/issues-tracker (0.1.0, on the registry, 454 lines, plain .mjs)
2. **Planned feeds (lines):** postright 88, mailright 88
3. **Holds today:** Fresh RightKit tool (`right-issues` writes issues.html from GitHub issues)
4. **Across apps:** Five app trees pin the same version; no app has its own implementation.
5. **Shape: best** (reference: none; depth light; confidence high). One zero-dependency CLI that every repository runs the same way. The lines the plan maps to it are the generated issues.html pages, not app source.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | main package.json:6 pins 0.1.0; issues.html present |
   | pulse | fits | package.json:29 and pnpm-workspace.yaml:3; .rightkit/issue-groups.json supplies groups |
   | postright | fits | package.json:42 |
   | mailright | fits | package.json:39 |
   | scraperight | fits | package.json:34 |
   | coderight | gaps | rightkit-mandatory.json M24 says not adopted because 0.1.0 is not on the registry; the registry snapshot shows it is, so the note is stale |

7. **Boundary problems:** 
   - Plan rows for postright and mailright (88 lines each, ADOPT) are generated output files, not code to move
   - README says `staged and never published`; npm-registry.json shows 0.1.0 on the registry
8. **Before publication:** 
   - Correct the README status line
   - Drop the two generated-file rows from the plan

### @rightkit/outdated

1. **Exists as:** packages/outdated (tree 0.3.1; registry 0.3.0; 4,757 lines TS)
2. **Planned feeds (lines):** pulse 324
3. **Holds today:** Fresh RightKit tool (`right-update`: check, apply, gate, land for cargo, pnpm, swiftpm, RightKit and external-tool pins)
4. **Across apps:** Pulse's poller compares recorded donor commits with upstream heads and prints a report; right-update compares pinned dependency versions and can apply and land them. Different inputs, same intent.
5. **Shape: gaps** (reference: none; depth light; confidence medium). Right design for dependency pins and already configured in two app trees. Pulse's mapped scripts do a different job (polling donor repositories recorded in upstream.lock.json), which the package has no source kind for.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | fits | .rightkit/update.json present on main |
   | scraperight | fits | .rightkit/update.json with cargo, pnpm, swiftpm, rightkit and a yt-dlp tool pin |
   | pulse | gaps | scripts/upstream-check.mjs, upstream-report.mjs (2 of 233 distinctive lines in the package): git ls-remote of donor repositories against a lock file; no .rightkit/update.json |

7. **Boundary problems:** 
   - README header says version 0.2.0; package.json says 0.3.1; registry has 0.3.0
   - pulse scripts/swiftright-step7-moves.sh (50 lines) is mapped here and is a one-off move script
8. **Before publication:** 
   - Decide whether a `donor` source kind (repository + recorded commit) belongs here; if yes add it so Pulse's poller can go, if no remap those files to stay with the app
   - Publish 0.3.1 and fix the README version

### @rightkit/send-ui

1. **Exists as:** packages/send-ui (0.1.0, not on the registry, 675 lines)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Fresh RightKit implementation over rightkit-send's single JSON command (`createSendClient`, `useSend`, `SendPanel`)
4. **Across apps:** Pulse view: per-action Tauri commands. send-ui: one `send_invoke` command carrying `{command,args}` (src/client.ts:30-41). Behaviour parity not verified.
5. **Shape: gaps** (reference: pulse; depth light; confidence medium). The design (one host command, typed client, no per-action wrapper) is right for a second app. But Pulse's own nearby-sharing view sits in @rightkit/system-ui as a second implementation, and this one has never been built or tested.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | hub/src/views/NearbySettings.tsx (628 lines) is in @rightkit/system-ui (263 of 267 distinctive lines) and calls share_state plus chat_* commands; not on send-ui |
   | heardright | n/a |  |
   | viewright | n/a |  |

7. **Boundary problems:** 
   - Two nearby-sharing screens in RightKit: send-ui SendPanel and system-ui/src/views/NearbySettings.tsx [verified]
   - Pulse's view mixes chat settings (chat_set_enabled, chat_install_skill, ...) into the nearby page (system-ui NearbySettings.tsx:117-210)
   - CHANGELOG: no unit tests, not built here
8. **Before publication:** 
   - Build and test it against rightkit-send commands
   - Check parity with Pulse's view (devices, incoming requests, per-device trust), then delete NearbySettings from system-ui; the chat rows go to the chat settings package
   - Publish after that

### @rightkit/sync-worker

1. **Exists as:** packages/sync-worker (private, 0.1.0, Cloudflare Worker, 1,164 lines TS)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Fresh RightKit server for rightkit-sync-http/1 (D1 + R2), written for HeardRight's non-Apple sync
4. **Across apps:** Single implementation; no app-side server code.
5. **Shape: gaps** (reference: heardright; depth light; confidence medium). A deployable, not a publishable package, and its status is contradictory: pnpm-workspace.yaml says parked on 2026-10-16 and excluded from install and build, while its README and crates/rightkit-sync say the service is live and supported (ruling rightkit#967).
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | named as first consumer; no HttpBackend or rightkit-sync-http use found in heardright-main/tauri-app-next .rs or .toml [verified by grep] |

7. **Boundary problems:** 
   - pnpm-workspace.yaml:3-4 (`Parked ... not installed, built or published`) against README:7 and crates/rightkit-sync/README.md:21 (`live and supported`)
   - wrangler.toml has ALLOWED_APPS = "heardright" and a placeholder bucket name `REPLACE_WITH_R2_BUCKET_NAME`, which does not read as a deployed service [suspicion]
   - No app name in src (0 occurrences of heardright)
8. **Before publication:** 
   - Owner decision: parked or supported. Make the workspace file, this README and the rightkit-sync README say the same thing
   - Keep private; never publish to npm. If supported, put it back in the workspace so it is built and tested

## Cargo crates

### rightkit-align

1. **Exists as:** crates/rightkit-align (0.1.0, publish = false, 1,363 lines)
2. **Planned feeds (lines):** cutright 2,477
3. **Holds today:** Fresh RightKit implementation written to CutRight's contract request (docs/plans/2026-10-11-cutright-contract-asks...); no app code in it
4. **Across apps:** No app implements script-to-word alignment today: CutRight asked for it, GenRight declares the gap.
5. **Shape: gaps** (reference: cutright; depth light; confidence medium). Product-neutral script-to-ASR word alignment with a versioned contract; the API is sound. It has no consumer, no real-audio fixtures, and the CutRight code the plan maps to it is transcription, not alignment.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | cutright | gaps | crates/cutright-speech (2,477 lines: ONNX/CoreML TDT, windowing, VAD, word timing) is ASR; window.rs matches rightkit-asr (19 lines) and dictation-inference (15), words.rs matches 1 line here. Its mic alignment already uses rightkit_audio::align_mic (cutright-project/src/audio_sync.rs:11) |
   | genright | gaps | dubbing.rs:17,451 reports word alignment `unavailable: needs ... rightkit-align`; would adopt once published |
   | heardright | n/a |  |

7. **Boundary problems:** 
   - Plan maps cutright-speech here; it belongs with rightkit-asr, rightkit-vad and rightkit-transcript
   - Two things called alignment: waveform offset (rightkit-audio align_mic) and script-to-word (this crate); the names should say which
8. **Before publication:** 
   - Remap cutright-speech to the ASR family
   - Add fixtures on real narration audio (README: pending on CutRight audio)
   - Set publish = true and publish so GenRight's dubbing can use it

### rightkit-android

1. **Exists as:** crates/rightkit-android (0.3.1, on crates.io, 1,123 lines Rust + Gradle module)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Fresh RightKit implementation from HeardRight's Android plan; README: written without compiling or running anything
4. **Across apps:** Single implementation.
5. **Shape: gaps** (reference: heardright; depth light; confidence medium). Right boundary (the one crate that links JNI and UniFFI; app exports stay in the app's cdylib) and no app names in the API. It has never been built, and its only consumer is not wired.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | main has tauri-app-next/heardright-android with `Cargo.toml.pending`; lib.rs:88 says the bridge is not wired yet |
   | pulse | gaps | android/meshlink Kotlin wrapper has no RightKit home (family-system-utilities.md, RightKitMeshLink action: add the Kotlin wrapper here) |

7. **Boundary problems:** 
   - Published at 0.3.1 while unverified by any build
   - minSdk 28 is the crate's own choice (README)
8. **Before publication:** 
   - Build it once for arm64-v8a and x86_64 and run the bridge in an emulator before the next publish
   - Decide whether the mesh Kotlin wrapper lives here or beside RightKitMeshLink

### rightkit-contrast

1. **Exists as:** crates/rightkit-contrast (0.1.0, on crates.io, 97 lines, no dependencies)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Fresh RightKit implementation of the WCAG maths already in @rightkit/theme color.ts and RightKitUI.RightKitContrast
4. **Across apps:** App contrast code found is JavaScript dev scripts (coderight scripts/check-contrast.mjs, mailright scripts/check-theme-contrast.ts) and belongs with @rightkit/theme, not this crate.
5. **Shape: best** (reference: none; depth light; confidence high). Five pure functions on an Rgb type, std only. Nothing app-specific.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | CHANGELOG cites Pulse ownership-exception rows 31 and 32 as the reason; no rightkit-contrast dependency in the surveyed tree yet |
   | viewright | n/a | contrast code is TypeScript (src/settings/themes.test.ts), served by @rightkit/theme |
   | heardright | n/a | LIVE src/lib/theme.ts is TypeScript |

7. **Boundary problems:** 
   - Same maths in three languages (theme, RightKitUI, this crate): keep one shared test vector file so they cannot drift [suggestion]
8. **Before publication:** none

### rightkit-owner-approvals

1. **Exists as:** crates/rightkit-owner-approvals (0.1.1, on crates.io, 4,508 lines)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Pulse's signed owner approvals (approval_sig.rs, admin_sig.rs, protected_guard/*), per family-agent-platform.md
4. **Across apps:** Not re-read here. family-agent-platform.md compared Pulse (owner-only decision bound to a digest of what was shown, signed variants), MailRight (SQL store, boot-bound) and GenRight (file store, nonce) and chose Pulse's rules as the reference.
5. **Shape: wrong** (reference: pulse; depth light; confidence medium). Conclusion taken from family-agent-platform.md (section `rightkit-approvals (with rightkit-owner-approvals)`, lines 269-293): approvals exist as three unconnected implementations in four crates; this crate should become the `signed` proof of a redrawn rightkit-approvals core, with its redact.rs and audit.rs moved out.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | its own code; the pending queue it works with is in rightkit-remote-gateway/src/approvals.rs |
   | mailright | wrong | its approval store is inside rightkit-agent-gateway and shares nothing with this |
   | genright | wrong | its approvals are rightkit-approvals (launch-secret HMAC), unrelated |

7. **Boundary problems:** 
   - redact.rs masks secrets in output and audit.rs is an audit log: neither is approvals (family-agent-platform.md:175,195,286)
   - Depended on by rightkit-mesh, rightkit-app-lock, rightkit-remote-gateway, rightkit-web-session [verified in Cargo.toml], so a fold-in touches four crates
   - The plan has no row with this name; Pulse's 13,104 planned lines are under rightkit-approvals
8. **Before publication:** 
   - Follow the rightkit-approvals actions in matrix-rows-agent-platform.json: one core (request, states, single-use claim, store trait, proof trait); this crate becomes the signed proof or a `-signed` sub-crate
   - Move redact.rs and audit.rs to their owners
   - Do not publish further versions under this name until that decision is made

### rightkit-platform-rules

1. **Exists as:** crates/rightkit-platform-rules (0.1.1, on crates.io, 1,195 lines + assets/platform-rules.json)
2. **Planned feeds (lines):** postright 107
3. **Holds today:** Fresh RightKit implementation; PostRight is the consumer it was written for
4. **Across apps:** PostRight is the only app that calls it. CutRight's limits are the same kind of fact held as code; compared in family-video-generative.md, not re-read here.
5. **Shape: gaps** (reference: postright; depth light; confidence medium). Catalog, revision hash and check are the right API and PostRight already uses them. The bundled data covers eight social platforms (29 rules, none for TikTok) and three other RightKit crates still hold platform limits in code.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | postright | fits | crates/engine/Cargo.toml:47 pins =0.1.0; used in engine/src/variants.rs:37 and api_versions.rs:6. Its comment waits for an unpublished 0.2.0 |
   | cutright | gaps | render_qa PlatformRules, timeline presets and captions safe_zones hold YouTube/Shorts/Reels/TikTok limits as compiled data (family-video-generative.md:131-145, 688) |
   | genright | n/a |  |

7. **Boundary problems:** 
   - postright engine/src/api_versions.rs (107 lines) is mapped here but matches rightkit-social-publish/src/api_versions.rs (22 of 25 distinctive lines); it is already owned there
   - Platform facts also in rightkit-timeline Preset, rightkit-captions safe_zones.json and rightkit-review render_qa (family-video-generative.md:688)
   - rightkit-social-publish ships a second data file (data/platform-rules.json) merged on top of the bundled seed: two places to edit a limit
   - README says `Not yet published (0.1.0)`; registry has 0.1.1
8. **Before publication:** 
   - Add video rules (aspect, duration, title/description/tag lengths, safe zones) and TikTok so the video crates can drop their compiled limits
   - State which file is the source for a limit: the bundled seed or social-publish's overrides
   - Fix the README status; publish the version PostRight is waiting for

### rightkit-policy-rules

1. **Exists as:** crates/rightkit-policy-rules (0.1.1, on crates.io, 6,538 lines with tests)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Legion's legion-policy-model and legion-policy, moved 2026-11-01 (README). Legion source is not available, so this was not compared
4. **Across apps:** Not compared: Legion source unavailable.
5. **Shape: gaps** (reference: legion; depth light; confidence low). A policy model plus evaluator with precedence and frozen scopes is a reasonable shared unit, but it still carries Legion's internal port folders and comments, and RightKit has a second agent policy crate (CodeRight's rightkit-agent-policy) with overlapping matching code.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | legion | fits | its own code; not verified, source unavailable |
   | coderight | gaps | its policy is rightkit-agent-policy; family-agent-platform.md:246-249 found about 300 matching lines and asks for the overlap to be checked |
   | pulse | gaps | remote-gateway term.rs has its own command allow/deny policy (family-agent-platform.md:245) |

7. **Boundary problems:** 
   - Module names from Legion's work items: rules/wf_port/wf067, rules/arcane_port, analysis/wf_port/u06; 28 comment lines refer to legion crates (for example rules/wf_port/wf067/invocation_proof.rs:4-22 describes a duplicate of legion_arcane) [verified]
   - Two policy crates: rightkit-policy-rules (Legion) and rightkit-agent-policy (CodeRight)
   - Depends on rightkit-evidence-contracts; used by rightkit-evidence-commands and rightkit-code-audit-commands
8. **Before publication:** 
   - Decide the relation to rightkit-agent-policy: one evaluator, or a stated split (rule packs here, command classification there)
   - Rename the port folders by what they do and remove comments about Legion crates
   - Resolve the invocation-proof duplicate the comment describes

### rightkit-signal

1. **Exists as:** crates/rightkit-signal (0.1.0, on crates.io, 613 lines)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Pulse hub's notch wake-up signalling (watch_notch, win_bridge) moved in shape and generalised (CHANGELOG)
4. **Across apps:** Only Pulse has this capability in Rust. Its Windows and Mac halves were not diffed against the crate.
5. **Shape: best** (reference: pulse; depth light; confidence medium). One small API (post, Watcher, Lease, LeaseMonitor) over Darwin notifications and Windows named events, with names supplied by the caller. Nothing Pulse-specific.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | donor; still has its own calls in hub/src-tauri/src/lib.rs, win_bridge.rs, windows/src/bridge.rs and mac HubBridge.swift, no rightkit-signal dependency in the surveyed tree |
   | heardright | n/a | Darwin notifications are used from Swift on iOS (MainAppDispatcher.swift, HeardRightAppIntents.swift), served by RightKitIntents |
   | viewright | n/a |  |

7. **Boundary problems:** 
   - Re-exported as rightkit_tauri_shell::signal (tauri-shell/src/lib.rs:27), so Tauri apps get it without a direct dependency
   - The Swift side posts the same Darwin names from RightKitIntents and Pulse's HubBridge; the name format rule (validate_name) exists only in Rust [suspicion]
8. **Before publication:** none

### rightkit-sysmem

1. **Exists as:** crates/rightkit-sysmem (0.1.0, on crates.io, 311 lines, no dependencies)
2. **Planned feeds (lines):** none in the plan under this name
3. **Holds today:** Ember's `lightcraft-sysmem` crate (CHANGELOG names Ember as donor; Ember is not one of the surveyed apps)
4. **Across apps:** Five places read physical memory with the same Win32 call and two release allocator pages with the same macOS call; the crate matches them in behaviour but none use it.
5. **Shape: gaps** (reference: none; depth light; confidence medium). A zero-dependency leaf for platform memory calls is a fine idea, but nothing depends on it and the same FFI is still declared in six other RightKit crates. One of its three functions is a monitor colour profile lookup, not memory.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | own FFI copies: engine/src/disk_index.rs:815 (malloc_zone_pressure_relief), core/src/lib.rs:807 and windows/src/sensors.rs:884 (GlobalMemoryStatusEx) |
   | genright | gaps | genright-studio/src/model_management/governor.rs:177 (GlobalMemoryStatusEx) |
   | heardright | gaps | LIVE heardright-telemetry/src/system_metrics.rs:119 via windows-sys |
   | viewright | n/a | no use of these calls found |

7. **Boundary problems:** 
   - README says `this is the one place they live`; the same extern declarations are in rightkit-fsindex/src/index.rs:483, rightkit-qa/src/pool.rs:171, rightkit-system-monitor/src/lib.rs:878, rightkit-remote-compute/src/models/governor.rs:177, rightkit-models/src/hardware.rs:627, and rightkit-logs uses windows-sys for it [verified]
   - No crate or app depends on rightkit-sysmem [verified]
   - window_monitor_profile (ICC profile of a window's monitor) is display colour, unrelated to the crate name
   - family-system-utilities.md:188 already notes the overlap with rightkit-system-monitor
8. **Before publication:** 
   - Make the six RightKit crates call it, or delete it and let rightkit-system-monitor own memory reads
   - Move window_monitor_profile to a display or colour crate, or rename the crate to what it is (platform FFI leaf)
   - Add available memory alongside total, since every other copy reads both [suspicion from the governor and sensors call sites]

### rightkit-dictation

1. **Exists as:** crates/rightkit-dictation-core; crates/rightkit-dictation-telemetry; crates/rightkit-dictation-app-runtime; crates/rightkit-dictation-models; crates/rightkit-dictation-text-pipeline; crates/rightkit-dictation-command-host; crates/rightkit-dictation-inference; crates/rightkit-dictation-runtime; crates/rightkit-dictation-engine; crates/rightkit-dictation-tauri; crates/rightkit-dictation-ffi
2. **Planned feeds (lines):** heardright 63,039
3. **Holds today:** Summary row. Forks of HeardRight main's engine crates, renamed with host configuration added (hr-review-dictation-chain.md, section 7; per-crate rows in matrix-rows-aa-heardright.json)
4. **Across apps:** Not re-reviewed; see hr-review-dictation-chain.md.
5. **Shape: gaps** (reference: heardright; depth light; confidence medium). Taken from hr-review-dictation-chain.md: no crate is the wrong shape and the module boundaries are HeardRight's own. Two crates are ready (telemetry, app-runtime); nine have listed gaps (engine CLI, renamed wire strings, model-host rename bug, KWS reconciliation, brand repair in the C entry point). Main does not use them above the core layer.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | about 62,000 lines in ten crates have no consumer in main, which runs its own copy; 20,128 of 38,011 distinctive planned lines are present |
   | coderight | n/a | delegates dictation to HeardRight (lib/voice/dictation.ts `needs_heardright`) |

7. **Boundary problems:** 
   - The planned single name does not exist; it is eleven crates
   - All but rightkit-dictation-core are on crates.io at the tree version; core is 0.2.4 in the tree against 0.2.3 published (registry.json)
   - Until HeardRight switches, every crate is a duplicate of a HeardRight crate
8. **Before publication:** 
   - Carry out actions A1 to A11 of hr-review-dictation-chain.md (per-crate lists are in matrix-rows-aa-heardright.json)
   - Replace the plan's single `rightkit-dictation` row with the eleven crate names

## Swift modules

### RightKitFS

1. **Exists as:** swift/RightKitSwift/Sources/RightKitFS (564 lines)
2. **Planned feeds (lines):** coderight 846
3. **Holds today:** Fresh RightKit implementation mirroring rightkit-fs and rightkit-daemon naming rules (app root, containment, atomic write, verified copy, append record)
4. **Across apps:** Read the call sites in all three apps: each constructs `RightKitAppRoot(app:...)` and uses `atomicWrite`; ScrapeRight also `copyVerified`. CodeRight alone passes `fileProtection` and an App Group; Pulse alone passes `baseOverride`. All three are covered by the existing initialisers.
5. **Shape: best** (reference: none; depth deep; confidence high). Three apps already build on the same type, `RightKitAppRoot`, with the same calls and no adapters beyond choosing a name and a protection class.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | coderight | fits | iOS: Storage/RightKitFSAdapter.swift:10-19 (two roots with different file protection), ComposerDraftStore.swift:67, WorkspaceHistory.swift:46, SharedWorkspacesLocation.swift:19 (App Group) |
   | pulse | fits | Mac: Notch/Sources/App/PulseStateRoot.swift:22, Hub/Sources/PulseHub/FileNotchBridge.swift:70 (baseOverride) |
   | scraperight | fits | iOS: ScrapeRightApp.swift:28, Queue/QueueStore.swift:85,295 (copyVerified, atomicWrite), Library/ArtifactStore.swift:49 |
   | heardright | n/a | not imported on main; shared state goes through RightKitStore |

7. **Boundary problems:** 
   - Plan maps CodeRight Storage/KeychainStore.swift (181) and AppleSyncedCredentialStore.swift (203) here; they are credential stores (RightKitCredentials, RightKitAppleCredentials)
   - `suiteDirectory = "Orthic Labs"` is fixed in the module (RightKitFS.swift, static let): a suite decision every adopter inherits
8. **Before publication:** none

### RightKitHotkeys

1. **Exists as:** swift/RightKitSwift/Sources/RightKitHotkeys (760 lines)
2. **Planned feeds (lines):** pulse 151
3. **Holds today:** Fresh RightKit implementation shaped on Pulse's Mac notch (Carbon hot-key registrar, event-tap hub with handler chain, keystroke planner)
4. **Across apps:** Only Pulse has Swift hotkey code.
5. **Shape: best** (reference: pulse; depth light; confidence medium). Registrar, combo parsing, event-tap hub and keystroke posting are generic and Pulse already uses the registrar. What remains is on Pulse's side.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | fits | imports it in LauncherHotKey.swift, WindowHotKeys.swift, WindowShortcut.swift, SettingsHotKey.swift. Still keeps its own Conveniences/EventTapHub.swift (168 lines, no RightKit import) beside RightKitEventTapHub |
   | heardright | n/a | Mac hotkeys are in Rust (rightkit-hotkeys crate); the pill package has no hotkey code |

7. **Boundary problems:** 
   - Plan maps Keyboard/FnCommand.swift (151 lines) here as ADOPT: it is a Pulse feature (Fn acts as Command for ten letters and the arrows) and belongs with the Mac tweaks module that family-system-utilities.md proposes, registered on RightKitEventTapHub
   - Pulse's EventTapHub.swift duplicates RightKitEventTapHub [file read at header level only]
8. **Before publication:** none

### RightKitHTTP

1. **Exists as:** swift/RightKitSwift/Sources/RightKitHTTP (786 lines)
2. **Planned feeds (lines):** coderight 644
3. **Holds today:** Fresh RightKit implementation mirroring rightkit-http (client, retry policy, address policy, redirect guard, streamed download)
4. **Across apps:** No second implementation of a general HTTP client in app Swift; CodeRight's client is a daemon RPC client.
5. **Shape: gaps** (reference: none; depth light; confidence medium). Sound client used inside RightKitSwift (LLM, Update, OAuth, Account), but no app imports it, and the one app with networking mapped to it needs server-sent events, which it does not parse.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | coderight | gaps | iOS Networking/SSEFrameParser.swift (67) has no counterpart here; DaemonClient.swift (245) and ConnectionLifecycle.swift belong to RightKitDaemonClient; CallbackURL/CallbackURLHandler.swift (255) is URL-scheme handling, not HTTP |
   | heardright | n/a | not declared on main (hr-review-ios-swift.md:49) |
   | pulse | n/a |  |
   | scraperight | n/a |  |

7. **Boundary problems:** 
   - Four of five mapped CodeRight files are not HTTP client code
   - RightKitDaemonClient and RightKitURLPolicy exist as separate modules and are the likelier owners [not read]
8. **Before publication:** 
   - Add an SSE frame parser over the existing `stream` transport (HTTPTransport.swift:39), taking CodeRight's parser as the reference
   - Remap DaemonClient and CallbackURLHandler in the plan

### RightKitHub

1. **Exists as:** swift/RightKitSwift/Sources/RightKitHub (3,673 lines); swift/RightKitSwift/Sources/RightKitHubQA
2. **Planned feeds (lines):** pulse 8,117
3. **Holds today:** Pulse's SwiftUI hub base moved unchanged: 29 files, 27 identical, HubSettingsView.swift and HubExports.swift differ by 19 lines; RightKitHubQA's 3 files identical
4. **Across apps:** Single implementation (Pulse's), compared file by file with the RightKit copy. No other app has a native hub.
5. **Shape: gaps** (reference: pulse; depth deep; confidence high). Shell, sidebar, search, settings model, theme and platform-services protocol with no Pulse product strings: a second Mac or iOS app could adopt it. But Pulse still builds from its own local copy, and the copy includes a module (RightKitPresentation) that RightKitSwift does not have.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | 68 files import RightKitHub from the local path package pulse/swift/RightKitHub (mac/Notch/project.yml:69, ios/project.yml:29, mac/Hub/Package.swift:21), not from the RightKitSwift pin; 11 files import RightKitPresentation |
   | heardright | n/a | imports neither RightKitHub nor RightKitHubQA (hr-review-ios-swift.md:292) |
   | coderight | n/a | 6,899 lines of its tests are mapped to RightKitHubQA, which is desktop Hub fakes |

7. **Boundary problems:** 
   - Pulse's package also holds RightKitPresentation (TextDocument.swift 628, CompactText, ResponseDocument, ResponsePlan: 888 lines); no such module exists in RightKitSwift [verified]
   - RightKitEngineClient differs from Pulse's copy: AttachedEngine.swift 91 of 525 lines, AttachedTransport.swift 19 of 437; RightKitEngine differs by 12 lines
   - The plan's 8,117 lines under RightKitHub include RightKitEngineClient (1,200), RightKitEngine (945), RightKitPresentation (888), the C header, native Rust and scripts; the Hub module itself is about 3,660
   - Platform/HubPlatformServices.swift:2 conditionally imports SwiftRightHost for Windows: a dependency on a Windows Swift host that is not part of the package [not followed]
8. **Before publication:** 
   - Add RightKitPresentation to RightKitSwift (or decide it belongs to the agent UI family) so Pulse can drop the local package
   - Reconcile AttachedEngine.swift with Pulse's copy and say which side is current
   - Pulse then replaces `path: ../../swift/RightKitHub` with the RightKitSwift products
   - Split the plan row into Hub, EngineClient, Engine and Presentation

### RightKitUpdate

1. **Exists as:** swift/RightKitSwift/Sources/RightKitUpdate (1,234 lines)
2. **Planned feeds (lines):** pulse 703, coderight 127
3. **Holds today:** RightKit rewrite shaped on Pulse's Mac updater (the Developer ID requirement text and relauncher match Pulse's lines) with feeds, channels, tiers and boot probation added
4. **Across apps:** Pulse: one enum-namespaced flow fixed to /Applications/Pulse.app and GitHub releases, with crash-safe swap. RightKit: protocols `UpdateFeed`, `UpdateVerifier`, `UpdateInstalling`, plus `BootProbation` (roll-back decision after a bad first boot) that Pulse lacks. CodeRight iOS: no installer, awareness only.
5. **Shape: gaps** (reference: pulse; depth deep; confidence medium). Feed protocol (GitHub release, static manifest, App Store lookup), verifier protocol and installer protocol are the right boundaries for both apps. The Mac installer is weaker than Pulse's: it swaps with replaceItemAt and has no swap journal, install lock or finish-after-crash step. No app imports the module.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | pulse | gaps | mac/Notch/Sources/Updater/UpdateInstaller.swift:58-163 keeps a swap journal, takes an install lock, swaps with renamex_np and finishes a pending swap at next launch; MacInstall.swift:279 has none of these [verified by grep]. Bundle id and team id are constants in Pulse (UpdateVerifier.swift:7-8) and parameters here |
   | coderight | fits | iOS Updates/UpdateAwareness.swift compares a static latest version and links to the App Store (placeholder id); AppStoreLookupFeed covers it. WhatsNewView.swift (58) is a view and stays out |
   | heardright | n/a | HeardRight/AppUpdateController.swift (115) was flagged `check against RightKitUpdate`, not verified (hr-review-ios-swift.md:377) |

7. **Boundary problems:** 
   - The Rust side has rightkit-app-update and @rightkit/app-update: three update owners across languages; the manifest and tier vocabulary should be one contract [not compared]
   - SemanticVersion exists here and in Pulse (34 lines) and is mapped ADOPT
8. **Before publication:** 
   - Port Pulse's journalled swap, install lock and finishPendingSwap into MacAppInstaller before Pulse adopts it
   - Keep verifier identifiers as parameters (already so)
   - Check the manifest and tier names against rightkit-app-update

### RightKitSwift

1. **Exists as:** swift/RightKitSwift (VERSION 0.9.1, unreleased; 39 RightKit modules, one C target, 2 executables)
2. **Planned feeds (lines):** pulse 58,785, heardright 46,179, coderight 36,435, scraperight 5,099, cutright 1,859
3. **Holds today:** Mostly rewritten RightKit code (line matching finds little app code in it), except RightKitHub, HubQA, Engine and EngineClient which are Pulse's files
4. **Across apps:** Per-app use read from project.yml, Package.swift and import lines. Swift files exist in HeardRight, Pulse, CodeRight, ScrapeRight and CutRight only.
5. **Shape: gaps** (reference: heardright; depth light; confidence medium). The package is in real use by four apps at one pinned version, and the foundation modules fit. But 62% of the Swift lines the plan maps (92,690 of 148,357) point at six modules that do not exist, and another 26,478 point at RightKitHubQA, which is the wrong home.
6. **Per app:**

   | App | Fit | Note |
   |---|---|---|
   | heardright | gaps | main imports 13 products on iOS at 0.8.1 and one on the Mac pill at 0.9.0; LIVE uses RightKitUI only, from the retired mirror at 0.1.1; about 6,700 lines of own copies remain (hr-review-ios-swift.md) |
   | pulse | gaps | pins 0.8.1 by URL github.com/bogusyogi/rightkit; imports Hub, Accessibility, Overlay, EngineClient, Watch, Hotkeys, Diagnostics, AppleCredentials, License, FS, DB, Credentials, Store; Hub/Engine/Presentation come from a local path package |
   | coderight | gaps | iOS and Mac helper pin 0.8.1 by URL github.com/Orthic-Labs/rightkit; imports Credentials, License, FS, AppleCredentials, UI, Store, Purchases, LiveActivity, Diagnostics, CloudSync; 30,723 planned lines wait on RightKitAgent |
   | scraperight | fits | iOS pins 0.8.1; imports License, CloudSync, Purchases, FS, Speech, ImportQueue, AppleCredentials, Diagnostics, Artifacts; 1,072 lines wait on RightKitLibrary |
   | cutright | gaps | 15 Swift files, 1,859 lines, all mapped to RightKitMedia, which does not exist |
   | viewright | n/a | no Swift |
   | genright | n/a | no Swift |
   | mailright | n/a | no Swift |
   | postright | n/a | no Swift |

7. **Boundary problems:** 
   - Planned but missing modules: RightKitAgent 34,981 (coderight 30,723, pulse 4,258), RightKitSystemUI 21,612 (pulse), RightKitNotch 17,668 (pulse), RightKitDictation 10,514 (heardright), RightKitLibrary 6,056 (heardright 4,984, scraperight 1,072), RightKitMedia 1,859 (cutright)
   - RightKitHubQA receives 26,478 planned lines of app tests (heardright 16,048, coderight 6,899, pulse 3,531); it is three files of desktop Hub fakes (hr-review-ios-swift.md:288-292)
   - RightKitWatch is an FSEvents file watcher; HeardRight's 3,476 Apple Watch lines are mapped to it (hr-review-ios-swift.md)
   - RightKitPresentation is imported by Pulse and absent from the package
   - RightKitMeshLink is a separate package outside RightKitSwift's versioning (family-system-utilities.md:66)
   - Two repository URLs for the same pin: bogusyogi/rightkit (Pulse) and Orthic-Labs/rightkit (CodeRight, ScrapeRight, HeardRight) [verified]; whether both stay valid is not verified
   - RightKitHTTP and RightKitUpdate are imported by no app
8. **Before publication:** 
   - Release 0.9.1 (tree is ahead of every app's pin)
   - Create or reassign the six missing modules before any app is told to move code: Agent and Dictation first, they carry the most lines
   - Remove app tests from the RightKitHubQA and RightKitWatch targets in the plan; app tests stay with the app
   - Add RightKitPresentation; bring RightKitMeshLink under the same version
   - One repository URL in every app manifest

## Not done

- Nothing was compiled or run.
- `@rightkit/blender-worker`, `@rightkit/ml-lab`, `rightkit-owner-approvals` and `rightkit-dictation` were not re-read; their verdicts are taken from the reviews named in each section.
- `rightkit-policy-rules` could not be compared with Legion.
- Behaviour parity was not checked for `send-ui` against Pulse's view, `rightkit-signal` against Pulse's bridge code, or `@rightkit/dictation-ui` against the LIVE HeardRight build.
- For the light-depth packages the evidence is manifests, READMEs, changelogs, declarations and call sites, not function bodies.
