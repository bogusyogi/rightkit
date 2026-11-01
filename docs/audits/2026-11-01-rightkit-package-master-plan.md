# RightKit package master plan

Date: 2026-11-01. Built by going through all 10 app repositories file by file (every tracked code file on `main`), mapping each to the RightKit package it should become or use, then merging overlaps across apps. Per-app tables: `docs/rightkit-inventory/<app>-rightkit-mapping.md` in each app repo (plus a JSON with every file).

Goal: no app code outside RightKit except a thin app shell (entry points, command tables, wiring). Package names are product-neutral; a package used by only one app today is still a RightKit package.

## Coverage by app

| App | Code lines | NEW | EXTEND | ADOPT | To Legion | Vendor | DROP | Product (stays) | App tests (stay) | Shell+config (stay) | Stays % |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| PostRight | 147,942 | 11,554 | 14,823 | 1,372 | 0 | 0 | 468 | 45,453 | 71,997 | 2,275 | 32% |
| ScrapeRight | 103,419 | 41,676 | 30,085 | 11,537 | 0 | 0 | 2,573 | 0 | 7,847 | 9,701 | 9% |
| MailRight | 113,155 | 32,831 | 23,636 | 3,664 | 0 | 0 | 164 | 47,380 | 1,198 | 4,282 | 45% |
| ViewRight | 363,273 | 3,247 | 17,643 | 1,879 | 0 | 171,356 | 51,240 | 79,955 | 34,125 | 3,828 | 23% |
| GenRight | 174,925 | 62,877 | 24,828 | 4,118 | 0 | 0 | 8,627 | 42,910 | 26,319 | 5,246 | 27% |
| CutRight | 211,021 | 39,752 | 22,856 | 1,332 | 42,787 | 6,675 | 513 | 54,565 | 40,062 | 2,479 | 27% |
| Pulse | 673,096 | 260,961 | 59,627 | 34,354 | 1,758 | 71,409 | 23,187 | 168,148 | 6,539 | 47,113 | 31% |
| HeardRight | 460,166 | 21,230 | 136,497 | 17,457 | 0 | 60,335 | 21,987 | 153,193 | 28,886 | 20,581 | 37% |
| CodeRight | 964,275 | 480,216 | 252,076 | 26,112 | 11,095 | 26,679 | 2,320 | 72,352 | 56,262 | 37,163 | 11% |
| Legion | 663,533 | 105,460 | 190,578 | 1,357 | 62,055 | 0 | 0 | 251,964 | 3,666 | 48,453 | 45% |
| **All** | **3,874,805** | **1,059,804** | **772,649** | **103,182** | **117,695** | **336,454** | **111,079** | **915,920** | **276,901** | **181,121** | **28%** |

Product-specific domain logic and UI stays in the app (P13). Unit tests move with their code; app journey/e2e tests stay with the app. Generated third-party notices, docs and data are not counted. Stays % = product + shell/config, excluding tests.

## Final list: new RightKit packages to create & publish

**46 new packages**: 37 crates, 6 npm packages, 3 Swift packages, all public like the rest of RightKit. Wave 2 = used by two or more apps, wave 3 = one app today. Grouped by family; within a family, ordered by size.

### Agent platform

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-agent-engine` | crate | 2 | Pulse, CodeRight | 129,186 | Swift | `rightkit-agent-runtime` | always-on agent: task envelope & transitions, principals, intents, effects, device/approval binding, voice handoff; agent loop, providers, budget (Swift: RightKitAgent); Swift agent UI/bridge; windows agent glue; agent loop, edit history & diffs, task graph, runtime middleware & bounded resources, attention, session storage, context folds/rehydrate, trajectory journal, wire protocol; interactive & headless session host; embedded agent runtime for fork-less hosts (iOS) |
| `@rightkit/agent-ui` | npm | 2 | MailRight, Pulse, CodeRight | 121,233 |  |  | agent approval cards, pairing & audit views; approval IPC client; remote approvals & owner settings; agent workbench UI: chat timeline, composer, sessions, runtime/goals/memory/review panels, approvals, plugins/skills, orchestration, bots, environments, state stores; computer-use & browser panels; remote web client |
| `rightkit-terminal-ui` | crate | 3 | CodeRight | 94,376 |  |  | terminal UI toolkit: composer, transcript, wrapping, panes, markdown render, image input, render metrics |
| `rightkit-agent-orchestration` | crate | 2 | CodeRight, Legion | 87,895 |  | `rightkit-work-router` | agent room; multi-agent coordination: subagents, teams, delegation, roster; handoff packets, continuity gate, live state; capability routing runtime: plan, route, task, budget, grant, escalation, delivery, inventories, platform layers, skills/designer checks |
| `rightkit-agent-gateway` | crate | 2 | MailRight, Pulse | 35,378 |  | `rightkit-remote-control` | paired external agents over local daemon: method registry, gate, approval cards (send/forward/delete/attach), audit log, untrusted-content contract, redaction; remote tools for agents (files, app launch, clipboard, chat read/start/term, browser address), local approvals, audit trail; agent gateway: LLM provider & tool routing |
| `RightKitAgent` | Swift | 2 | Pulse, CodeRight | 34,981 | Swift | `RightKitRemoteUI` | phone remote-control screens; Swift agent client: local runtime bridge, chat library, code editor, diff walkthrough, plan review, markdown rendering, approvals |
| `rightkit-evidence` | crate | 2 | CodeRight, Legion | 32,933 |  | `rightkit-specs` | spec-governed artifacts & coverage; contracts, effect execution & receipts, decision store, evidence/SARIF/HTML reports, certification & governance lifecycle |
| `rightkit-agent-policy` | crate | 2 | CodeRight, Legion | 25,061 |  |  | pure permission decision engine, destructive-action guard, policy packs, audit; policy model, rules & enforcement |
| `rightkit-approvals` | crate | 2 | MailRight, GenRight, Pulse | 16,672 | Swift |  | user approval cards for agent send/forward/delete/attach; approval tokens & cards for paid drafts/deliveries (minted by shell, verified by engine); signed phone/owner approvals for privileged & reserved actions; Windows admin service; Swift approvals |
| `rightkit-scheduler` | crate | 3 | CodeRight | 9,315 |  |  | cron schedules & triggers for agent runs |
| `rightkit-github` | crate | 3 | CodeRight | 8,586 | Swift |  | Swift forge connectors: GitHub, Cloudflare, Fly.io, git, PRs; GitHub App: auth, webhooks, PR/issue ops |
| `rightkit-bot-messaging` | crate | 3 | CodeRight | 6,489 |  |  | remote access: Telegram bot, web auth, remote gateway |
| `@rightkit/terminal-ui` | npm | 3 | CodeRight | 1,370 |  |  | xterm terminal pane |

### Context, memory & code intelligence

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-context` | crate | 3 | Pulse | 98,511 | Swift, Python |  | host side of context engine; context engine: protocol, client, provider SDK, federation, budget/lanes, runtime, schemas, hooks |
| `rightkit-repo-graph` | crate | 2 | Pulse, CodeRight | 37,879 |  |  | repository graph: structure, evidence, drift; tree-sitter skeletonization |
| `rightkit-memory` | crate | 2 | Pulse, CodeRight | 24,602 |  |  | durable memory store (Cortex): scopes, identity, telemetry; adaptive memory retrieval & backend (over context engine) |
| `rightkit-adapt` | crate | 2 | Pulse, CodeRight | 23,896 |  |  | governed behavioural learning (taste + insights); post-session learning pipeline |
| `rightkit-research` | crate | 2 | PostRight, Legion | 10,619 |  |  | dated research digests & source readers (Reddit); research pipeline (sources, claims, evidence) |

### Dictation & voice

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `@rightkit/dictation-ui` | npm | 2 | HeardRight, CodeRight | 9,765 |  | `@rightkit/overlay-ui`, `@rightkit/text-polish-ui` | dictation home/controls, banners, command palette, PTT, sounds, metrics; polish & vocabulary pages; recording pill webview UI; voice conversation |

### Video editing

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-timeline` | crate | 3 | CutRight | 8,011 |  | `rightkit-timeline-interchange` | FCPXML & OpenTimelineIO export; portable timeline core: tracks, clips, rational time, edits, undo |
| `rightkit-reframe` | crate | 3 | CutRight | 4,222 |  |  | tracked 9:16 reframe: ONNX face detect, tracking, smoothed crop path |
| `rightkit-color` | crate | 3 | CutRight | 4,108 |  |  | colour grading & scopes |
| `rightkit-captions` | crate | 3 | CutRight | 3,177 |  |  | caption styles, per-language caption sets, placement, swatch preview |
| `rightkit-compositor` | crate | 3 | CutRight | 3,088 |  |  | native wgpu layer compositor, GPU effects & SDF masks (adapted from OpenCut, MIT) |
| `RightKitMedia` | Swift | 3 | CutRight | 1,859 | Swift |  | macOS media worker (Swift): AVFoundation render, audio finish, typography, Vision, motion compositor; Silero VAD & vision anchor |

### Generative media

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-media-gen` | crate | 3 | GenRight | 34,258 |  |  | generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, HeyGen avatars, EvoLink, Higgsfield, Muapi, NanoBanana, Replicate, Wan2GP, Blender), model catalogue & facts, routing, durable gen jobs, result descriptors, balances |
| `rightkit-local-gen` | crate | 3 | GenRight | 12,312 |  | `rightkit-lipsync`, `rightkit-local-image`, `rightkit-music-gen`, `rightkit-upscale` | keyless local image gen: sd.cpp server runtime, memory modes, profiles, job store, artifacts; local Wav2Lip talking-head lip-sync on ONNX; local MusicGen ONNX music generation; local Real-ESRGAN image upscale |
| `rightkit-blender-worker` | crate | 3 | GenRight | 4,386 | Python |  | Blender render worker (Python, runs inside Blender): scene, materials, product registration/review, finish profiles, contract |
| `rightkit-remote-compute` | crate | 3 | GenRight | 2,681 |  |  | rented GPU workers (Vast): register/stage/run lifecycle, time-billed spend |

### Content pipeline & social

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-pipeline` | crate | 2 | PostRight, GenRight, CutRight | 17,975 |  |  | dispatch of content briefs to another engine as external process; brief run / delivery envelope handling between suite engines; brief check/run, channel recipes, unattended recipe runs, durable batch delivery; cross-engine orchestration: call generation engine, follow jobs, place results |
| `rightkit-social-analytics` | crate | 2 | PostRight, CutRight | 6,486 |  | `rightkit-link-tracking` | analytics pull/store, manual metric import, calendar optimisation proposals; campaigns, tracked short links & visit records; retention feedback import & analysis |

### Ingest & library

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-library` | crate | 2 | PostRight, ScrapeRight, GenRight, HeardRight | 14,388 |  | `rightkit-text-enrich` | reusable media records with occurrence ids & expiry; non-LLM enrichment: keywords, links, source tags, text segmentation; local artifact library: history authority, SQLite FTS index, delete/restore; owned media library: import w/ provenance, asset facts (origin/evidence/rights), FTS search, artifact index; transcript history: SQLCipher store, key rotation, retention, recovery, export |
| `@rightkit/library-ui` | npm | 2 | ScrapeRight, HeardRight | 11,064 |  | `@rightkit/queue-ui` | job queue UI: paste bar, drop overlay, job cards, queue board, engine event hook; library list, transcript/article reader, find-in-document, workbench; transcript history & file transcribe |
| `rightkit-media-fetch` | crate | 2 | ScrapeRight, GenRight | 9,877 | Swift | `rightkit-source-detect`, `rightkit-stock-media` | yt-dlp download/normalize, playlists & channels, X/TikTok/Instagram post, carousel & image capture; URL/file job classifier, media-type table, URL & host helpers; fetch-pool vs compute-lane split; Swift port of detect; replace with UniFFI binding; stock & archive acquisition (Pexels, Pixabay, Internet Archive) w/ licence facts |
| `RightKitLibrary` | Swift | 2 | ScrapeRight, HeardRight | 6,056 | Swift | `RightKitLibraryUI` | Swift artifact library & markdown export; SwiftUI queue, library, result detail, receive views; SwiftUI history & settings views; Swift transcript library, insights, cloud sync |
| `rightkit-web-session` | crate | 2 | ScrapeRight, Pulse | 4,168 | Swift |  | signed-in web sessions: in-app sign-in webview, cookie import/export, profile dirs; agent browser logins on rightkit-browser; Swift browser logins |
| `rightkit-web-extract` | crate | 3 | ScrapeRight | 2,734 | Swift | `RightKitExtract` | article extraction ladder: native fetch w/ TLS fingerprint, browser render, readability, content filters; Swift extractors: article, carousel, media URL, PDF, OCR, WebView render, fetch policy |

### Mail & workspace

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-google-apis` | crate | 3 | MailRight | 8,778 |  |  | Google Workspace clients: Gmail sync/history/batch/MIME/compose/labels/filters/vacation/drafts, Calendar, Drive, People/Contacts, Tasks, Meet, budgets & backoff |

### System utilities

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-mesh` | crate | 2 | Pulse, HeardRight, CodeRight | 34,952 | Swift, Gradle |  | device mesh (iroh): link, device circle, join tokens, relay & mailbox/admission, FFI for iOS/Android (already named rightkit-mesh but lives in app repo: move & publish); device link module glue; Swift mesh client; Swift devices; devices UI; device pairing (Swift) (+1 more) |
| `rightkit-system-monitor` | crate | 3 | Pulse | 13,212 | Swift |  | CPU/memory/GPU/disk gauges, process inspection, SMART drive health, metrics history, volumes & eject |
| `rightkit-app-lock` | crate | 2 | MailRight, Pulse | 1,746 | Swift |  | app lock: OS-auth verification, serialized lock/unlock transitions, DB gate; owner unlock auth / app lock |

### Foundation & tooling

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-bench` | crate | 2 | ScrapeRight, MailRight, ViewRight, CodeRight | 27,521 | Swift, Python |  | benchmark corpora runner, scoring, receipts & claim gate; SQLite perf proof & probes, load driver; association & PDF bench matrices; PDF perf bench; agent benchmarks |
| `rightkit-apple-ops` | crate | 3 | Legion | 10,150 |  |  | Apple ops: App Store Connect API & upload, build analysis, preflight, profiling, SwiftUI traces |
| `rightkit-doctor` | crate | 2 | PostRight, MailRight, CutRight, CodeRight | 2,262 |  |  | capability health probes behind doctor command; doctor report; doctor dimensions & probes |
| `@rightkit/config` | npm | 2 | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 1,057 |  |  | shared toolchain/lint/vite presets; shared presets |
| `@rightkit/waitlist-worker` | npm | 2 | MailRight, ViewRight | 529 |  |  | Cloudflare waitlist worker |

## Existing RightKit packages to extend

**73 existing packages** gain capability now living in apps.

Check: `check` = only one app adds code here; confirm it is generic, otherwise it is product code and stays in the app.

| Package | Wave | Check | Used by | Lines moving in | What it gains |
|---|---|---|---|---:|---|
| `rightkit-workflow` | 1 | shared | CodeRight, Legion | 158,335 | autonomous workflow control plane: runs, leases, fences, node executors; workflow port (scenario/state machine runtime) |
| `rightkit-agent-tools` | 1 | shared | GenRight, CodeRight | 86,283 | agent tool profile; LSP API; coding tool suite: fs read/edit, shell, search, git, LSP, web, browser, MCP bridge, media |
| `@rightkit/qa` | 1 | shared | ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight, Legion | 55,827 | QA capture, visual diff, evidence, test inventory, isolation; mock IPC for UI QA; mock IPC data for UI QA; QA browser helpers & tooling; browser mock API for UI QA (+5 more) |
| `rightkit-agents` | 1 | shared | PostRight, Pulse, CodeRight | 47,632 | headless drafting via installed Claude/Codex CLI; Claude Code/Codex transcript discovery & normalisation; vendor coding drivers (Claude Code, Codex, Cursor, Cline, Grok, ACP), resumable NDJSON & JSON-RPC transports, login routes; vendor turn |
| `@rightkit/release` | 1 | shared | PostRight, ScrapeRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 44,071 | manifest-consistency gate checks; release pin check; bundled runtime staging & artifact verification; gates & sample validation; server deploy scripts (+5 more) |
| `rightkit-asr` | 1 | shared | ScrapeRight, GenRight, HeardRight | 34,995 | Parakeet/Whisper routing & engines (use rightkit-asr + rightkit-speech-client); API gap first: api-gap: rightkit-transcript 0.4.0 export::stamp is private; see docs/rightkit-exceptions/outside-M-rows.md#it; API gap first: api-gap: rightkit-speech-client 0.4.0 transcribe_via_service_policy lacks running-host, capability, window-fit; whisper ASR; prebuilt core xcframework: build from RightKit crates (+4 more) |
| `rightkit-llm` | 1 | shared | MailRight, CodeRight | 34,931 | BYOK AI actions & translation providers; provider clients, replay, heuristic model routing, provider setup presets, local inference runtime |
| `@rightkit/settings-ui` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 22,923 | settings panel; spend allowances panel; store health/diagnostics panel; models section client; runtime/doctor sections & prefs (+6 more) |
| `rightkit-daemon` | 1 | shared | PostRight, MailRight, GenRight, CutRight, CodeRight | 22,118 | headless server role (scheduler, public listener, remote host); heartbeat record already owned by rightkit-daemon; generic app CLI client over running app: arg parsing, error envelope & exit codes, pairing key, link; runtime service lifecycle, health snapshot, event hub w/ debounce bridge, lock budgets, repo bundle; installed suite-app discovery (+3 more) |
| `rightkit-automation` | 1 | shared | Pulse, HeardRight | 21,483 | OS permission status & prompts; text delivery into focused app: paste, streaming insertion & reconcile, UIA/AX, terminal targets, TSF tip, focus continuity; system media playback pause/resume while dictating (Mac/Win) |
| `rightkit-overlay-win` | 1 | check | Pulse | 20,798 | Win32 notch surface toolkit: render, text layout, motion, cards, alerts, tool wheel |
| `rightkit-app-settings` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight, Pulse, HeardRight, CodeRight | 19,669 | settings get/patch for settings-ui; persisted settings reader; API gap first: MOVE @rightkit/settings-ui and the shell log host (multi-file log tail preview with reveal, bogusyogi/rightkit; UI preference store; settings get/patch (+3 more) |
| `rightkit-overlay` | 1 | check | HeardRight | 17,672 | recording pill overlay (native Win/Mac, dispatch queue) |
| `rightkit-review` | 1 | shared | PostRight, GenRight, CutRight, Legion | 12,623 | deterministic text quality/approval gate with policy revisions; phone review & pause surface over daemon; human-eyes review gate; final-render QA gate & social packaging checks; independent review & minimal-diff review |
| `rightkit-hooks` | 1 | shared | CodeRight, Legion | 12,490 | lifecycle hook bus & runner; hook binary |
| `rightkit-spend` | 1 | shared | PostRight, GenRight, CutRight, Pulse | 11,027 | app spend layer over rightkit-spend ledger; gen spend ledger GUI+CLI; productivity/command ledger & preference policy; per-project spend budgets |
| `@rightkit/react-shell` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight, CodeRight | 10,496 | useAsync, state router, operator layout css; desktop chrome css, flash/toast, idle task, request gate, formatting; shell/theme/platform/update tests; app lock overlay; error boundary, find bar, shortcut registry & help overlay, dialogs, perf monitor (+2 more) |
| `@rightkit/pdf-ui` | 1 | shared | MailRight, ViewRight | 10,411 | pdf preview; PDF editor UI: annotate, text edit, OCR overlay, page ops |
| `rightkit-sync` | 1 | shared | ScrapeRight, HeardRight | 8,935 | CloudKit transport, phone-to-Mac queue relay, sync settings & runtime; CloudKit transcript sync scheduler |
| `rightkit-qa` | 1 | shared | PostRight, MailRight, HeardRight, CodeRight | 8,682 | fake editor engine & fake Claude/Codex CLI; mock server already rightkit-qa MockServer; test sandbox; process-wide test sandbox; test-only env switches (+2 more) |
| `rightkit-sandbox` | 1 | check | CodeRight | 7,177 | execution backends, environments, worktree pool |
| `rightkit-compose` | 1 | shared | GenRight, CutRight | 6,293 | keyless still finishing composite; evaluated timeline -> compositor frame descriptors |
| `rightkit-job-queue` | 1 | shared | PostRight, ScrapeRight, GenRight, Pulse | 5,882 | per-command request/response schema validation; durable queue w/ fetch pool + single compute lane, quarantine, event sink; caller leases & detached job runners; activity leases |
| `rightkit-license` | 1 | shared | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 5,185 | licence config & headless lookup; API gap first: api-gap: scraperight is not in contracts/right-suite-api.v1.json, so right_suite(app) update auth refuses it; ; Apple-device Pro continuity bridge; licence gate & device identity; API gap first: MOVE rightkit-license. See docs/rightkit-exceptions/M12.md#b1-revocation-denylist-src-taurisrcpro_gaters-revok (+17 more) |
| `rightkit-record` | 1 | shared | Pulse, HeardRight | 5,095 | screenshots via WGC; screenshot capture for context |
| `@rightkit/theme` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight | 4,982 | generated; Swift brand tokens generated from brand.json; generated brand css; contrast check; brand contract check (+6 more) |
| `RightKitUI` | 1 | shared | ScrapeRight, Pulse, HeardRight | 4,730 | SwiftUI settings sections; SwiftUI settings; onboarding |
| `rightkit-models` | 1 | shared | ScrapeRight, MailRight, GenRight, CutRight, HeardRight, CodeRight | 4,662 | shared model store ops, migration between suite apps, seeding; API gap first: api-gap: rightkit-models 0.5.1 has no bundled-model seed or staging/replaced sweep; see docs/rightkit-exceptio; API gap first: MOVE @rightkit/release. See docs/rightkit-exceptions/M19.md#b1-embedding-model-bundle-provisioning-script-and-; model store glue; memory-aware governor for heavy local models, hardware fit, install, Blender/Wan2GP/sd.cpp doctor (+6 more) |
| `rightkit-tauri-shell` | 1 | shared | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 4,490 | asset-protocol scope grants; API gap first: api-gap: rightkit-job-queue 0.5.1 cannot hold the first spawn until a first snapshot exists; see docs/rightkit; engine sidecar host; build identity, tray, shell plugin, startup error window; API gap first: MOVE rightkit-tauri-shell. See docs/rightkit-exceptions/M5.md#b1-start-up-failure-message-box-src-taurisrcstar (+16 more) |
| `@rightkit/onboarding` | 1 | shared | ScrapeRight, GenRight, Pulse, HeardRight, CodeRight | 4,328 | models intro & setup gate; onboarding, legal, licence plan, device allowance; onboarding/legal/licence; mic & calibration onboarding steps; onboarding pages (+1 more) |
| `rightkit-browser` | 1 | shared | ScrapeRight, CodeRight | 4,114 | API gap first: api-gap: rightkit-browser 0.3.6 BrowserPage has no user-agent or extra-headers setter; see docs/rightkit-excep; CDP browser backend & address pin; browser workspace for agents |
| `rightkit-db` | 1 | shared | PostRight, MailRight | 3,763 | SQLite store backup, verify & restore; compaction w/ rowid verify, background WAL checkpoints, verified pre-migration backup, migration gate, lock-order gate |
| `RightKitHubQA` | 1 | check | Pulse | 3,531 | hub fixtures, renderer, AX tour; view snapshots |
| `RightKitWatch` | 1 | check | HeardRight | 3,476 | watch recording capture, transfer & intents |
| `rightkit-tts` | 1 | check | GenRight | 3,284 | local Qwen3-TTS voice; voice library: clones, presets, provider voices |
| `rightkit-motion` | 1 | shared | GenRight, CutRight | 3,245 | HyperFrames HTML composition render: loopback host, headless Chrome capture, encode; contact sheets & thumbnails |
| `rightkit-ffmpeg` | 1 | shared | GenRight, CutRight, CodeRight | 3,213 | pure-Rust media probe (dims, duration, codec); ffmpeg discovery; single H.264 encoder rule; ffmpeg editing build staging & qualification; media tool adapters (+1 more) |
| `rightkit-sidecar` | 1 | shared | MailRight, CodeRight | 3,112 | process engine split: core child owning DB, IPC transport, event pump, supervision, trust boundary; stream transport |
| `rightkit-crash` | 1 | shared | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 3,019 | renderer crash capture; panic capture; crash; out-of-process minidump writer, user feedback, diagnostics transport |
| `rightkit-align` | 1 | check | CutRight | 2,477 | word timings + VAD for talking-head (on rightkit-asr/vad/align) |
| `rightkit-fs` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 2,444 | API gap first: api-gap, rightkit-logs 0.3.5 redacts structured JSON by key (redact.rs:51) and text by shape or name=value (sc; engine paths; API gap first: api-gap: rightkit-fs 0.4.7 AppDirs has no user Downloads folder; see docs/rightkit-exceptions/M8.md#item-1; downloads folder, blocking helper, Windows ACL hardening; roots & path policy (+2 more) |
| `rightkit-vault` | 1 | check | Pulse | 1,984 | agent logins & payment secrets |
| `rightkit-playback` | 1 | shared | ScrapeRight, CutRight | 1,819 | compatibility playback; native playback session over evaluated revision |
| `RightKitSpeech` | 1 | shared | ScrapeRight, HeardRight, CodeRight | 1,740 | on-device transcription; shared model install store; model download/install, CoreML validation; on-device ASR & KWS bridges; dictation |
| `rightkit-channel-memory` | 1 | check | PostRight | 1,618 | channel store & channel memory document (schema already in RK); fixtures |
| `rightkit-transcript` | 1 | check | ScrapeRight | 1,519 | transcript & article artifact writers, output path naming |
| `rightkit-identity` | 1 | check | Pulse | 1,360 | device identity & short-code pairing |
| `RightKitIntents` | 1 | check | HeardRight | 1,224 | app intents & voice commands |
| `rightkit-contracts` | 1 | check | PostRight | 1,218 | contract.check & delivery envelope IO at app boundary |
| `RightKitLiveActivity` | 1 | shared | HeardRight, CodeRight | 1,107 | recording live activity views; widgets |
| `rightkit-logs` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, CodeRight | 1,030 | API gap first: api-gap, rightkit-tauri-shell 0.6.0 logs feature covers write and collect, but clear is store.clear() on one f; structured logging & support bundles; API gap first: api-gap: the shell logs clear covers the renderer log only (no extra sinks) and the shell has no panic hook wi; API gap first: MOVE rightkit-logs (text-line redactor with stable key-token hashing, bogusyogi/rightkit#520). See docs/rightk; redaction & renderer logs (+5 more) |
| `rightkit-search` | 1 | check | MailRight | 944 | HTML->indexable text, FTS5 maintenance & background rebuild |
| `rightkit-send` | 1 | shared | ScrapeRight, Pulse | 891 | Swift binding/UI over rightkit-send (receive scratchpad, clipboard offer); nearby sharing glue & UI |
| `rightkit-docs` | 1 | check | ViewRight | 695 | document format classification table |
| `RightKitImportQueue` | 1 | check | ScrapeRight | 623 | universal serial job queue & runner; share extension intake |
| `@rightkit/ax` | 1 | shared | GenRight, Pulse | 489 | agent plugin evals; agent plugin launcher |
| `@rightkit/legal` | 1 | shared | ScrapeRight, MailRight, ViewRight, CutRight, CodeRight | 439 | legal notices; legal config; API gap first: api-gap: the shell legal receipt importer is Rust-side only; the renderer localStorage receipt cannot reach it; legal configs & gates; legal acceptance host (+4 more) |
| `RightKitHub` | 1 | check | Pulse | 437 | duplicate copy of RightKit's RightKitHub: delete; API gap first: see docs/rightkit-exceptions/M6.md#b8 |
| `rightkit-http` | 1 | shared | PostRight, ScrapeRight, GenRight, CodeRight | 409 | one-request HTTP layer already on rightkit-http; SSRF URL guard for non-rightkit-http transports; outbound HTTP policy; http |
| `rightkit-embed` | 1 | check | MailRight | 398 | background semantic indexing job |
| `rightkit-credentials` | 1 | shared | MailRight, GenRight, Pulse, HeardRight | 388 | API gap first: ADOPT rightkit-db 0.5.2. See docs/rightkit-exceptions/M9.md#b2-sqlcipher-key-rotation-and-pending-key-open-src; provider API keys; secure prompt; secrets |
| `rightkit-mcp` | 1 | shared | CodeRight, Legion | 332 | MCP client/server; API gap first: outside-M-rows.md H-1 strict percent-decode of untrusted URL text: rightkit-http 0.3.8 percent_decode and perc; stdio MCP transport |
| `@rightkit/outdated` | 1 | check | Pulse | 324 | upstream donor check |
| `rightkit-time` | 1 | shared | PostRight, MailRight | 302 | local wall time -> UTC for calendars (tz-aware); API gap first: MOVE rightkit-time. See docs/rightkit-exceptions/M15.md#b3-wall-clock-utc-stamps-src-taurisrcutc_stamprs: 1) n |
| `@rightkit/tauri` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight | 293 | thin adapters over existing RK packages; adapters over existing RK packages; API gap first: MOVE @rightkit/theme. See docs/rightkit-exceptions/M2.md#b3-theme-preset-catalogue-srclibthemets-themes-preset; adapters |
| `@rightkit/media-ui` | 1 | shared | ScrapeRight, GenRight | 282 | media player adapters; mask brush & export |
| `RightKitFS` | 1 | check | CodeRight | 203 | API gap first: M11.md I-3 synced-credential policy: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the lat; storage |
| `RightKitLicense` | 1 | shared | ScrapeRight, HeardRight, CodeRight | 183 | licence & Pro continuity config; licence & purchases; account, licence, purchases; API gap first: M12.md B5 iOS ContinuityBridgeClient: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the la |
| `@rightkit/i18n` | 1 | shared | PostRight, Pulse | 150 | money micro-units & instant formatting; i18n |
| `rightkit-pdf` | 1 | shared | ScrapeRight, ViewRight | 129 | API gap first: api-gap: rightkit-pdf 0.3.3 raster draws image XObjects only (no PDFium renderer, no text layer); see docs/rig; PDF editor adapter & font catalog |
| `rightkit-platform-rules` | 1 | check | PostRight | 107 | pinned platform API versions & sunset dates |
| `RightKitHTTP` | 1 | check | CodeRight | 67 | networking; API gap first: M7.md B3 iOS SSE frame parser and replay: RightKitSwift 0.8.1 has no SSE client. |
| `@rightkit/platform-ui` | 1 | check | ViewRight | 42 | API gap first: Browser-QA platform override seeded into __RIGHTKIT_PLATFORM__; platform-ui offers the override only on its /q; platform shim & test setup |

## Moves to Legion, not RightKit

Skills, doctrine, agent personas, skill manifests/loaders and host projection (Claude Code, Codex, AGENTS.md) are owned by Legion. RightKit is the code kit; Legion is the skills owner.

| From app | Lines | What |
|---|---:|---|
| CutRight | 42,787 | agent skill packs (designer engine incl. vendored huashu, content specialists, cutaway editing scripts) |
| Pulse | 1,758 | chat skill install into Claude/Codex (host projection) |
| CodeRight | 11,095 | skill manifests & trigger DSL, plugin marketplace & loader, recipes, custom commands |
| Legion | 5,119 | catalog & inventory generators |
| Legion | 12,837 | host setup, binding & script projection |
| Legion | 18,364 | capability catalog & frontmatter, host detection/projection into Claude Code/Codex/AGENTS.md, plugin marketplace install |
| Legion | 6,445 | skill & designer checks |
| Legion | 19,290 | agent skill packs & doctrine (architect, audit, council, designer, iOS/macOS/Android dev, marketing, SEO, research, writing, etc.) |

## Stays in the app: product-specific code

Single-app product domain logic and UI (P13 app-specific). Earlier drafts proposed these as packages; they stay in the app.

| App | Lines | What |
|---|---:|---|
| PostRight | 15,268 | was `rightkit-social-publish` |
| PostRight | 9,874 | was `@rightkit/social-client` |
| PostRight | 4,330 | was `@rightkit/content-ui` |
| PostRight | 3,820 | was `rightkit-content-proof` |
| PostRight | 3,127 | was `rightkit-content-briefs` |
| PostRight | 3,070 | was `rightkit-social-inbox` |
| PostRight | 2,830 | was `@rightkit/social-ui` |
| PostRight | 1,662 | was `rightkit-social-accounts` |
| PostRight | 1,472 | was `rightkit-content-calendar` |
| MailRight | 27,603 | was `rightkit-mail` |
| MailRight | 17,055 | was `@rightkit/mail-ui` |
| MailRight | 2,722 | was `@rightkit/workspace-ui` |
| ViewRight | 31,171 | was `@rightkit/document-workspace` |
| ViewRight | 27,905 | was `@rightkit/markdown-editor` |
| ViewRight | 15,898 | was `@rightkit/html-editor` |
| ViewRight | 3,147 | was `rightkit-document-workspace` |
| ViewRight | 1,834 | was `@rightkit/docx-editor` |
| GenRight | 21,626 | was `@rightkit/media-gen-ui` |
| GenRight | 12,135 | was `rightkit-prompt-recipes` |
| GenRight | 9,149 | was `rightkit-media-pipeline` |
| CutRight | 14,524 | was `rightkit-video-project` |
| CutRight | 12,104 | was `@rightkit/video-ui` |
| CutRight | 11,894 | was `rightkit-autoedit` |
| CutRight | 4,992 | was `rightkit-shorts` |
| CutRight | 4,434 | was `rightkit-agent-demo` |
| CutRight | 4,232 | was `@rightkit/timeline-ui` |
| CutRight | 2,385 | was `rightkit-edit-rules` |
| Pulse | 26,856 | was `rightkit-chat-bridge` |
| Pulse | 25,046 | was `rightkit-disk-cleanup` |
| Pulse | 22,237 | was `rightkit-launcher` |
| Pulse | 21,612 | was `RightKitSystemUI` |
| Pulse | 17,668 | was `RightKitNotch` |
| Pulse | 13,783 | was `rightkit-app-manager` |
| Pulse | 12,760 | was `rightkit-clipboard-history` |
| Pulse | 11,684 | was `rightkit-claude-accounts` |
| Pulse | 7,889 | was `@rightkit/system-ui` |
| Pulse | 5,908 | was `rightkit-ai-usage` |
| Pulse | 2,032 | was `rightkit-desktop-tweaks` |
| Pulse | 673 | was `rightkit-privileged-helper` |
| HeardRight | 60,504 | was `rightkit-dictation` |
| HeardRight | 53,827 | was `rightkit-ml-lab` |
| HeardRight | 18,564 | was `rightkit-text-polish` |
| HeardRight | 10,514 | was `RightKitDictation` |
| HeardRight | 9,784 | was `rightkit-voice-commands` |
| CodeRight | 72,352 | was `rightkit-agent-server` |
| Legion | 251,964 | was `rightkit-code-audit` |

## Already in RightKit: delete app copies & adopt

| Package | Apps still carrying own copy | Lines to delete |
|---|---|---:|
| `rightkit-mcp` | CodeRight, Legion | 11,995 |
| `rightkit-tauri-shell` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 10,064 |
| `rightkit-models` | ScrapeRight, MailRight, GenRight, CutRight, HeardRight, CodeRight | 9,467 |
| `rightkit-send` | ScrapeRight, Pulse | 9,261 |
| `RightKitHub` | Pulse | 7,680 |
| `@rightkit/i18n` | PostRight, Pulse | 7,215 |
| `rightkit-logs` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, CodeRight | 5,389 |
| `RightKitSpeech` | ScrapeRight, HeardRight, CodeRight | 3,925 |
| `RightKitLicense` | ScrapeRight, HeardRight, CodeRight | 3,785 |
| `rightkit-license` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 3,351 |
| `rightkit-fsindex` | Pulse | 2,980 |
| `@rightkit/onboarding` | ScrapeRight, GenRight, Pulse, HeardRight, CodeRight | 2,755 |
| `rightkit-hotkeys` | Pulse, HeardRight | 2,378 |
| `rightkit-asr` | ScrapeRight, GenRight, HeardRight | 1,624 |
| `RightKitDiagnostics` | ScrapeRight, HeardRight, CodeRight | 1,367 |
| `@rightkit/legal` | ScrapeRight, MailRight, ViewRight, CutRight, CodeRight | 1,256 |
| `RightKitCloudSync` | ScrapeRight | 1,212 |
| `rightkit-app-settings` | PostRight, ScrapeRight, MailRight, GenRight, Pulse, HeardRight, CodeRight | 1,176 |
| `rightkit-credentials` | MailRight, GenRight, Pulse, HeardRight | 1,131 |
| `rightkit-tts` | GenRight | 1,054 |
| `rightkit-fs` | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 1,019 |
| `rightkit-file-watch` | ViewRight, Pulse, CodeRight | 993 |
| `rightkit-ocr` | ScrapeRight, ViewRight | 987 |
| `rightkit-http` | PostRight, ScrapeRight, GenRight, CodeRight | 959 |
| `RightKitAppleCredentials` | ScrapeRight, HeardRight | 915 |
| `@rightkit/tauri` | PostRight, ScrapeRight, MailRight, GenRight | 839 |
| `RightKitUpdate` | Pulse, CodeRight | 830 |
| `@rightkit/media-ui` | ScrapeRight, GenRight | 698 |
| `RightKitFS` | CodeRight | 643 |
| `RightKitHTTP` | CodeRight | 577 |
| `rightkit-onnx` | ScrapeRight, HeardRight, CodeRight | 509 |
| `rightkit-pdf` | ScrapeRight, ViewRight | 503 |
| `@rightkit/legal-ui` | ScrapeRight, MailRight | 438 |
| `rightkit-child-process` | ScrapeRight, Pulse | 402 |
| `@rightkit/release` | PostRight, ScrapeRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 367 |
| `rightkit-crash` | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 351 |
| `rightkit-agent-tools` | GenRight, CodeRight | 302 |
| `@rightkit/settings-ui` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 278 |
| `rightkit-browser` | ScrapeRight, CodeRight | 278 |
| `rightkit-playback` | ScrapeRight, CutRight | 276 |
| `rightkit-oauth` | CodeRight | 230 |
| `@rightkit/app-update` | ScrapeRight | 218 |
| `rightkit-daemon` | PostRight, MailRight, GenRight, CutRight, CodeRight | 192 |
| `@rightkit/license` | ScrapeRight | 192 |
| `rightkit-qa` | PostRight, MailRight, HeardRight, CodeRight | 181 |
| `@rightkit/issues-tracker` | PostRight, MailRight | 176 |
| `RightKitHotkeys` | Pulse | 151 |
| `@rightkit/pdf-ui` | MailRight, ViewRight | 144 |
| `@rightkit/platform-ui` | ViewRight | 118 |
| `@rightkit/theme` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight | 112 |
| `rightkit-ffmpeg` | GenRight, CutRight, CodeRight | 93 |
| `rightkit-cache` | Pulse | 89 |
| `RightKitImportQueue` | ScrapeRight | 57 |

## Vendored code: decisions

| Code | App | Lines | Recommendation |
|---|---|---:|---|
| vendored docx-editor agent & i18n packages not needed (use @rightkit/i18n) | ViewRight | 51,240 | DROP: `none` |
| vendored DOCX editor (core + react): own & trim in RightKit | ViewRight | 171,356 | VENDOR-OWN: `@rightkit/docx-editor` |
| vendored hyperframes player bits | CutRight | 432 | VENDOR-OWN: `@rightkit/timeline-ui` |
| CSL style parser (MIT): keep as crates.io dep of typeset/citations | CutRight | 6,243 | VENDOR-KEEP: `rightkit-typeset` |
| hand-rolled zstd/json & vendored zstd: use crates/system lib | Pulse | 22,291 | DROP: `none` |
| vendored tree-sitter grammars; use crates.io grammar crates | Pulse | 71,409 | VENDOR-KEEP: `tree-sitter grammars` |
| prebuilt core xcframework: build from RightKit crates | HeardRight | 144 | DROP: `rightkit-asr` |
| ONNX Runtime binary via rightkit-onnx | HeardRight | 43,900 | VENDOR-KEEP: `rightkit-onnx` |
| sherpa-onnx xcframework headers: replace with RightKit Rust KWS/ASR (sherpa rewrite) | HeardRight | 9,378 | VENDOR-OWN: `rightkit-asr` |
| vendored copy of rightkit-asr: use published crate | HeardRight | 21,843 | DROP: `rightkit-asr` |
| patched Tauri runtime fork: upstream the patch, then use crates.io | HeardRight | 7,057 | VENDOR-KEEP: `tauri-runtime-wry` |
| vendored WasmKit: use SwiftPM dependency | CodeRight | 26,679 | VENDOR-KEEP: `WasmKit` |

## Merges made across apps

| Merged away | Into | Why |
|---|---|---|
| `rightkit-agent-runtime` | `rightkit-agent-engine` | always-on agent task contracts & agent loop are one engine |
| `rightkit-remote-control` | `rightkit-agent-gateway` | remote agent tools + approvals/audit = same gateway as MailRight agent methods |
| `rightkit-work-router` | `rightkit-agent-orchestration` | capability routing, budgets, escalation & handoff are orchestration |
| `rightkit-claude-accounts` | `rightkit-ai-accounts` | Claude/Codex accounts, session sync & usage meters |
| `rightkit-ai-usage` | `rightkit-ai-accounts` | Claude/Codex accounts, session sync & usage meters |
| `rightkit-social-accounts` | `rightkit-social-publish` | accounts are publish prerequisites |
| `rightkit-link-tracking` | `rightkit-social-analytics` | links & visits are analytics |
| `rightkit-content-briefs` | `rightkit-content-plan` | briefs, variants, hooks, drafts & calendar |
| `rightkit-content-calendar` | `rightkit-content-plan` | briefs, variants, hooks, drafts & calendar |
| `rightkit-text-gate` | `rightkit-review` | deterministic text gate is a review check |
| `rightkit-source-detect` | `rightkit-media-fetch` | classification is the fetch front door |
| `rightkit-stock-media` | `rightkit-media-fetch` | stock acquisition is media fetch |
| `rightkit-text-enrich` | `rightkit-library` | enrichment runs on library ingest |
| `rightkit-media-probe` | `rightkit-ffmpeg` | pure-Rust probe beside ffprobe |
| `rightkit-local-image` | `rightkit-local-gen` | local generative runtimes |
| `rightkit-lipsync` | `rightkit-local-gen` | local generative runtimes |
| `rightkit-music-gen` | `rightkit-local-gen` | local generative runtimes |
| `rightkit-upscale` | `rightkit-local-gen` | local generative runtimes |
| `rightkit-voice-library` | `rightkit-tts` | voice library belongs with TTS |
| `rightkit-timeline-interchange` | `rightkit-timeline` | FCPXML/OTIO export of the timeline |
| `rightkit-edit-rules` | `rightkit-autoedit` | rules drive autoedit |
| `rightkit-media-control` | `rightkit-automation` | system media keys are OS automation |
| `rightkit-service-host` | `rightkit-daemon` | service lifecycle/health belongs to daemon host |
| `rightkit-specs` | `rightkit-evidence` | specs & coverage are evidence artefacts |
| `@rightkit/mesh-ui` | `@rightkit/settings-ui` | devices/pairing are settings sections |
| `@rightkit/social-client` | `@rightkit/social-ui` | one social/content ops UI package |
| `@rightkit/content-ui` | `@rightkit/social-ui` | one social/content ops UI package |
| `@rightkit/queue-ui` | `@rightkit/library-ui` | ingest queue + library |
| `@rightkit/overlay-ui` | `@rightkit/dictation-ui` | dictation pill, polish & vocabulary UI |
| `@rightkit/text-polish-ui` | `@rightkit/dictation-ui` | dictation pill, polish & vocabulary UI |
| `@rightkit/timeline-ui` | `@rightkit/video-ui` | one video editor UI package |
| `RightKitLibraryUI` | `RightKitLibrary` | library w/ SwiftUI target |
| `RightKitRemoteUI` | `RightKitAgent` | phone remote control of agents |
| `RightKitSend` | `rightkit-send` | Swift binding target of the existing send crate |
| `RightKitExtract` | `rightkit-web-extract` | Swift extractor target of web-extract |

## What stays in apps

Each app keeps only entry points (Tauri `main`/`lib`, iOS/Android app entry, engine binary & command table, CLI definitions), app manifests and release config. Largest leftovers are CodeRight's CLI (`main_runtime_*`, `cli_definitions*`), Legion's CLI commands, Pulse's per-platform entries (Windows `main.rs`, Mac `AppDelegate`, hub) and HeardRight's `app_run.rs`. Each app's mapping file lists them.

## Decisions for Adrian

1. **D1, vendored code.** Each row of the vendored table:
   - DOCX: fork & trim (proposed) or keep upstream.
   - Tree-sitter grammars: crates.io.
   - WasmKit: SwiftPM.
   - tauri-runtime-wry: upstream the patch.
   - sherpa-onnx & prebuilt xcframeworks: replace with `rightkit-asr` built for iOS.
   - ONNX Runtime: binary via `rightkit-onnx`.
   - GenRight legacy Node server (8.6k lines): drop once confirmed unused.
2. **D2, borderline extensions.** 23 existing packages gain code from only one app (marked `check`). Confirm each is generic. Otherwise it is product code and stays in the app.
3. **D3, publication hold.** `docs/GATE-CONTRACT.md` records a hold on publication. Either lift it or schedule the waves after it.
4. **D4, Legion as a skills distributor.** CodeRight's skills, plugins & recipes crates move to Legion. Decide how CodeRight consumes Legion's crates (names & route).

Settled: everything extracted is public RightKit (crates.io/npm). Product-specific logic & UI stay in apps (P13). Skills live in Legion. Every NEW/EXTEND package gets an EGM-reviewed design doc before its code moves.

