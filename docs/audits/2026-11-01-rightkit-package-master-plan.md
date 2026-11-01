# RightKit package master plan

Date: 2026-11-01. Built by going through all 10 app repositories file by file (every tracked code file on `main`), mapping each to the RightKit package it should become or use, then merging overlaps across apps. Per-app tables: `docs/rightkit-inventory/<app>-rightkit-mapping.md` in each app repo (plus a JSON with every file).

Goal: no app code outside RightKit except a thin app shell (entry points, command tables, wiring). Package names are product-neutral; a package used by only one app today is still a RightKit package.

## Coverage by app

| App | Code lines | NEW | EXTEND | ADOPT | Vendor | DROP | Stays (shell+config) | Stays % |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| PostRight | 147,942 | 113,565 | 18,763 | 12,871 | 0 | 468 | 2,275 | 1% |
| ScrapeRight | 118,952 | 41,676 | 29,557 | 35,445 | 0 | 2,573 | 9,701 | 8% |
| MailRight | 126,048 | 80,211 | 22,297 | 19,094 | 0 | 164 | 4,282 | 3% |
| ViewRight | 387,740 | 112,549 | 18,857 | 29,910 | 171,356 | 51,240 | 3,828 | 0% |
| GenRight | 181,597 | 105,787 | 24,815 | 37,122 | 0 | 8,627 | 5,246 | 2% |
| CutRight | 236,061 | 161,400 | 31,936 | 33,058 | 6,675 | 513 | 2,479 | 1% |
| Pulse | 673,096 | 430,867 | 59,190 | 41,330 | 71,409 | 23,187 | 47,113 | 6% |
| HeardRight | 471,514 | 181,017 | 151,772 | 36,038 | 60,335 | 21,987 | 20,365 | 4% |
| CodeRight | 990,438 | 617,993 | 258,926 | 56,642 | 26,679 | 2,320 | 27,878 | 2% |
| Legion | 663,533 | 423,145 | 190,578 | 1,357 | 0 | 0 | 48,453 | 7% |
| **All** | **3,996,921** | **2,268,210** | **806,691** | **302,867** | **336,454** | **111,079** | **171,620** | **4%** |

Lines include tests, which move with the code they test.

## Final list: new RightKit packages to create & publish

**88 new packages**: 65 crates, 17 npm packages, 6 Swift packages. Grouped by family; within a family, ordered by size.

### Agent platform

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-agent-engine` | crate | Pulse, CodeRight | 129,186 | Swift | `rightkit-agent-runtime` | always-on agent: task envelope & transitions, principals, intents, effects, device/approval binding, voice handoff; agent loop, providers, budget (Swift: RightKitAgent); Swift agent UI/bridge; windows agent glue; agent loop, edit history & diffs, task graph, runtime middleware & bounded resources, attention, session storage, context folds/rehydrate, trajectory journal, wire protocol; interactive & headless session host; embedded agent runtime for fork-less hosts (iOS) |
| `@rightkit/agent-ui` | npm | MailRight, Pulse, CodeRight | 121,136 |  |  | agent approval cards, pairing & audit views; remote approvals & owner settings; agent workbench UI: chat timeline, composer, sessions, runtime/goals/memory/review panels, approvals, plugins/skills, orchestration, bots, environments, state stores; computer-use & browser panels; remote web client |
| `rightkit-agent-server` | crate | CodeRight | 117,397 | Python |  | OpenAPI client codegen & contract checks; agent daemon HTTP API: sessions, runtime, contract/OpenAPI, tool invocations, run changes, health, workspace, teams, coordination; server & daemon client runtime; route contract & e2e tests; installed matrix & engine tests |
| `rightkit-terminal-ui` | crate | CodeRight | 94,376 |  |  | terminal UI toolkit: composer, transcript, wrapping, panes, markdown render, image input, render metrics |
| `rightkit-agent-orchestration` | crate | CodeRight, Legion | 94,340 |  | `rightkit-work-router` | agent room; multi-agent coordination: subagents, teams, delegation, roster; handoff packets, continuity gate, live state; capability routing runtime: plan, route, task, budget, grant, escalation, delivery, inventories, platform layers, skills/designer checks |
| `@rightkit/skills` | npm | CutRight, Legion | 62,077 | Python |  | agent skill packs (designer engine incl. vendored huashu, content specialists, cutaway editing scripts); agent skill packs & doctrine (architect, audit, council, designer, iOS/macOS/Android dev, marketing, SEO, research, writing, etc.) |
| `rightkit-skills` | crate | CodeRight, Legion | 56,700 |  |  | skill manifests & trigger DSL, plugin marketplace & loader, recipes, custom commands; catalog & inventory generators; host setup, binding & script projection; capability catalog & frontmatter, host detection/projection into Claude Code/Codex/AGENTS.md, plugin marketplace install |
| `rightkit-agent-gateway` | crate | MailRight, Pulse | 37,461 |  | `rightkit-remote-control` | paired external agents over local daemon: method registry, gate, approval cards (send/forward/delete/attach), audit log, untrusted-content contract, redaction; remote tools for agents (files, app launch, clipboard, chat read/start/term, browser address), local approvals, audit trail; agent gateway: LLM provider & tool routing |
| `RightKitAgent` | Swift | Pulse, CodeRight | 34,981 | Swift | `RightKitRemoteUI` | phone remote-control screens; Swift agent client: local runtime bridge, chat library, code editor, diff walkthrough, plan review, markdown rendering, approvals |
| `rightkit-evidence` | crate | CodeRight, Legion | 32,933 |  | `rightkit-specs` | spec-governed artifacts & coverage; contracts, effect execution & receipts, decision store, evidence/SARIF/HTML reports, certification & governance lifecycle |
| `rightkit-chat-bridge` | crate | Pulse | 28,614 | Swift |  | deliver messages into running Claude/Codex chats (hooks, UIA/AX typing), envelopes, hub heartbeat, chat skill install, cross-user boundary |
| `rightkit-agent-policy` | crate | CodeRight, Legion | 25,061 |  |  | pure permission decision engine, destructive-action guard, policy packs, audit; policy model, rules & enforcement |
| `rightkit-ai-accounts` | crate | Pulse | 17,592 | Swift | `rightkit-ai-usage`, `rightkit-claude-accounts` | Claude Desktop/CLI account switching & Code-session sync across accounts; Claude/Codex plan usage, credentials, resets & credits (Swift providers); Claude sessions (Swift) |
| `rightkit-approvals` | crate | GenRight, Pulse | 14,589 | Swift |  | approval tokens & cards for paid drafts/deliveries (minted by shell, verified by engine); signed phone/owner approvals for privileged & reserved actions; Windows admin service; Swift approvals |
| `rightkit-scheduler` | crate | CodeRight | 9,315 |  |  | cron schedules & triggers for agent runs |
| `rightkit-github` | crate | CodeRight | 8,586 | Swift |  | Swift forge connectors: GitHub, Cloudflare, Fly.io, git, PRs; GitHub App: auth, webhooks, PR/issue ops |
| `rightkit-bot-messaging` | crate | CodeRight | 6,489 |  |  | remote access: Telegram bot, web auth, remote gateway |
| `rightkit-agent-demo` | crate | CutRight | 4,434 | Swift |  | consented agent demo mode: agent drives a catalog app via rightkit-automation, recorded via rightkit-record, badge, kill hotkey, bundles |
| `@rightkit/terminal-ui` | npm | CodeRight | 1,370 |  |  | xterm terminal pane |

### Context, memory & code intelligence

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-code-audit` | crate | Legion | 255,630 | Swift, Gradle |  | audit/verify commands & audit bench; repository audit engine: native providers (security, architecture, data, frontend, platform), DAG & scheduler, evidence issuers, findings history, review runtime, provider SDK, rule registry & qualification corpora |
| `rightkit-context` | crate | Pulse | 98,511 | Swift, Python |  | host side of context engine; context engine: protocol, client, provider SDK, federation, budget/lanes, runtime, schemas, hooks |
| `rightkit-repo-graph` | crate | Pulse, CodeRight | 37,879 |  |  | repository graph: structure, evidence, drift; tree-sitter skeletonization |
| `rightkit-memory` | crate | Pulse, CodeRight | 24,602 |  |  | durable memory store (Cortex): scopes, identity, telemetry; adaptive memory retrieval & backend (over context engine) |
| `rightkit-adapt` | crate | Pulse, CodeRight | 23,896 |  |  | governed behavioural learning (taste + insights); post-session learning pipeline |
| `rightkit-research` | crate | PostRight, Legion | 11,229 |  |  | dated research digests & source readers (Reddit); research pipeline (sources, claims, evidence) |

### Dictation & voice

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-dictation` | crate | HeardRight | 63,039 | Python |  | dictation runtime: mic capture & conditioning, session state machine, JSON-RPC sidecar worker, wake listener, recording lifecycle |
| `rightkit-text-polish` | crate | HeardRight | 18,564 | Swift |  | Swift polish clients (Apple Intelligence, BYOK); deterministic polish, LLM cleanup (L1-L3), vocabulary & learning, screen context |
| `RightKitDictation` | Swift | HeardRight | 10,514 | Swift |  | iOS dictation: recording session, warm capture, long-form, import queue, live activity |
| `rightkit-voice-commands` | crate | HeardRight | 9,784 | Swift |  | voice command recognition/classification, launch & OS command adapters |
| `@rightkit/dictation-ui` | npm | HeardRight, CodeRight | 9,765 |  | `@rightkit/overlay-ui`, `@rightkit/text-polish-ui` | dictation home/controls, banners, command palette, PTT, sounds, metrics; polish & vocabulary pages; recording pill webview UI; voice conversation |

### Video editing

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-autoedit` | crate | CutRight | 22,020 |  | `rightkit-edit-rules` | transcript-driven cut & editorial engines, edit plans, named variants, style measurement, mic alignment; scoped editing rules & resolution matrix, autonomy levels |
| `rightkit-video-project` | crate | CutRight | 18,072 |  |  | video project layer: sources & integrity, revisions, portable bundle export/import, card slots, cache storage; media workflow methods: projects, transcription, render; project/bundle/step e2e tests |
| `@rightkit/video-ui` | npm | CutRight | 16,768 |  | `@rightkit/timeline-ui` | project, captions, colour, shorts, recipes, outputs, batch & finished-edit screens; video editor UI: timeline, preview, inspector; vendored hyperframes player bits; client lib & engine bindings |
| `rightkit-timeline` | crate | CutRight | 8,011 |  | `rightkit-timeline-interchange` | FCPXML & OpenTimelineIO export; portable timeline core: tracks, clips, rational time, edits, undo |
| `rightkit-shorts` | crate | CutRight | 7,998 |  |  | shorts finder & scoring, on-screen-text moment search |
| `rightkit-reframe` | crate | CutRight | 5,637 |  |  | tracked 9:16 reframe: ONNX face detect, tracking, smoothed crop path |
| `rightkit-captions` | crate | CutRight | 5,293 |  |  | caption styles, per-language caption sets, placement, swatch preview |
| `rightkit-color` | crate | CutRight | 5,105 |  |  | colour grading & scopes |
| `rightkit-compositor` | crate | CutRight | 3,088 |  |  | native wgpu layer compositor, GPU effects & SDF masks (adapted from OpenCut, MIT) |
| `RightKitMedia` | Swift | CutRight | 1,859 | Swift |  | macOS media worker (Swift): AVFoundation render, audio finish, typography, Vision, motion compositor; Silero VAD & vision anchor |

### Generative media

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-media-gen` | crate | GenRight | 34,258 |  |  | generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, HeyGen avatars, EvoLink, Higgsfield, Muapi, NanoBanana, Replicate, Wan2GP, Blender), model catalogue & facts, routing, durable gen jobs, result descriptors, balances |
| `@rightkit/media-gen-ui` | npm | GenRight | 21,626 |  |  | generation studio UI: generate forms, gallery & library, jobs, recipes & templates, character/quality/video panels, voices, avatars, motion, production, pipelines, KDP, local image chat |
| `rightkit-local-gen` | crate | GenRight | 12,312 |  | `rightkit-lipsync`, `rightkit-local-image`, `rightkit-music-gen`, `rightkit-upscale` | keyless local image gen: sd.cpp server runtime, memory modes, profiles, job store, artifacts; local Wav2Lip talking-head lip-sync on ONNX; local MusicGen ONNX music generation; local Real-ESRGAN image upscale |
| `rightkit-prompt-recipes` | crate | GenRight | 12,135 |  |  | prompt contracts & validator, recipe composer & catalogue, characters/LoRA lock, video quality templates, template swap, brand codes, KDP workflow |
| `rightkit-media-pipeline` | crate | GenRight | 9,149 |  |  | named production pipelines: compose, dubbing w/ retiming, translation w/ terminology, fan-out multi-job steps, finishing handoff, campaign factory |
| `rightkit-blender-worker` | crate | GenRight | 4,386 | Python |  | Blender render worker (Python, runs inside Blender): scene, materials, product registration/review, finish profiles, contract |
| `rightkit-remote-compute` | crate | GenRight | 2,681 |  |  | rented GPU workers (Vast): register/stage/run lifecycle, time-billed spend |

### Content pipeline & social

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-social-publish` | crate | PostRight | 44,162 |  | `rightkit-social-accounts` | connected platform accounts, readiness, platform data-deletion requests; durable publish job phase machine + platform adapters (X, Bluesky, Mastodon, Threads, Instagram, Facebook, LinkedIn, Pinterest, TikTok, YouTube); per-account quota pacing & publish job wire types |
| `rightkit-pipeline` | crate | PostRight, GenRight, CutRight | 23,848 |  |  | dispatch of content briefs to another engine as external process; brief run / delivery envelope handling between suite engines; brief check/run, channel recipes, unattended recipe runs, durable batch delivery; cross-engine orchestration: call generation engine, follow jobs, place results |
| `@rightkit/social-ui` | npm | PostRight | 17,034 |  | `@rightkit/content-ui`, `@rightkit/social-client` | shared labels & scope select; TS client & types for social/content engine commands (generate from Rust); typed mock data source for UI QA; accounts, publish jobs, pacing, failures, inbox, DM threads screens; analytics, campaigns & links screens; content planning screens |
| `rightkit-social-analytics` | crate | PostRight, CutRight | 11,440 |  | `rightkit-link-tracking` | analytics pull/store, manual metric import, calendar optimisation proposals; campaigns, tracked short links & visit records; retention feedback import & analysis |
| `rightkit-social-mock` | crate | PostRight | 11,160 |  |  | deterministic mock of social platform APIs + webhooks for QA |
| `rightkit-content-proof` | crate | PostRight | 10,163 |  |  | engine data assets (catalogues); acquisition chain gates & claim/proof semantics |
| `rightkit-content-plan` | crate | PostRight | 7,811 |  | `rightkit-content-briefs`, `rightkit-content-calendar` | brief & calendar entry cache, proposals; typed briefs, platform variants, hook tests, manual drafts |
| `rightkit-social-inbox` | crate | PostRight | 5,109 |  |  | signed webhook inbox & keyword-comment DM automation |

### Ingest & library

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-library` | crate | PostRight, ScrapeRight, GenRight, HeardRight | 19,055 |  | `rightkit-text-enrich` | reusable media records with occurrence ids & expiry; non-LLM enrichment: keywords, links, source tags, text segmentation; local artifact library: history authority, SQLite FTS index, delete/restore; owned media library: import w/ provenance, asset facts (origin/evidence/rights), FTS search, artifact index; transcript history: SQLCipher store, key rotation, retention, recovery, export |
| `@rightkit/library-ui` | npm | ScrapeRight, HeardRight | 11,064 |  | `@rightkit/queue-ui` | job queue UI: paste bar, drop overlay, job cards, queue board, engine event hook; library list, transcript/article reader, find-in-document, workbench; transcript history & file transcribe |
| `rightkit-media-fetch` | crate | ScrapeRight, GenRight | 9,877 | Swift | `rightkit-source-detect`, `rightkit-stock-media` | yt-dlp download/normalize, playlists & channels, X/TikTok/Instagram post, carousel & image capture; URL/file job classifier, media-type table, URL & host helpers; fetch-pool vs compute-lane split; Swift port of detect; replace with UniFFI binding; stock & archive acquisition (Pexels, Pixabay, Internet Archive) w/ licence facts |
| `RightKitLibrary` | Swift | ScrapeRight, HeardRight | 6,056 | Swift | `RightKitLibraryUI` | Swift artifact library & markdown export; SwiftUI queue, library, result detail, receive views; SwiftUI history & settings views; Swift transcript library, insights, cloud sync |
| `rightkit-web-session` | crate | ScrapeRight, Pulse | 4,168 | Swift |  | signed-in web sessions: in-app sign-in webview, cookie import/export, profile dirs; agent browser logins on rightkit-browser; Swift browser logins |
| `rightkit-web-extract` | crate | ScrapeRight | 2,734 | Swift | `RightKitExtract` | article extraction ladder: native fetch w/ TLS fingerprint, browser render, readability, content filters; Swift extractors: article, carousel, media URL, PDF, OCR, WebView render, fetch policy |

### Documents

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `@rightkit/docx-editor` | npm | ViewRight | 174,941 |  |  | DOCX view/edit integration; vendored DOCX editor (core + react): own & trim in RightKit |
| `@rightkit/document-workspace` | npm | ViewRight | 51,259 |  |  | workspace e2e journeys; multi-format document workspace: tabs & panes, drafts, autosave, external-change watcher, find & cross-doc search, command palette, explorer/library sidebar, print pipeline, zoom, plain-text editor; workspace styles |
| `@rightkit/markdown-editor` | npm | ViewRight | 31,850 |  |  | markdown/MDX hybrid editor: projection, block & inline editing (code, math, mermaid, tables), citations, references, properties, export, safe HTML |
| `@rightkit/html-editor` | npm | ViewRight | 19,461 |  |  | HTML document view/authoring: sandboxed session, blocks, site-root resources |
| `rightkit-document-workspace` | crate | ViewRight | 3,147 |  |  | Rust side of document workspace: recent files & pins DB, explorer listing, markdown index/resource inspection, document IO |

### Mail & workspace

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-mail` | crate | MailRight | 27,603 |  |  | provider-neutral local mail mirror: schema, outbox & pending ops, sync apply/rollups, recipients, risk/trust signals, remote-image proxy & tracker blocking, attachment cache, engine method groups |
| `@rightkit/mail-ui` | npm | MailRight | 17,152 |  |  | inbox virtual list, reader & sandboxed email frame, compose editor & recipients, search routing/coverage/saved searches, image trust, attachment preview, account switcher, mail store |
| `rightkit-google-apis` | crate | MailRight | 8,778 |  |  | Google Workspace clients: Gmail sync/history/batch/MIME/compose/labels/filters/vacation/drafts, Calendar, Drive, People/Contacts, Tasks, Meet, budgets & backoff |
| `@rightkit/workspace-ui` | npm | MailRight | 2,722 |  |  | calendar grid & event forms, contacts, drive browser & preview, tasks, meet |

### System utilities

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-mesh` | crate | Pulse, HeardRight, CodeRight | 34,952 | Swift, Gradle |  | device mesh (iroh): link, device circle, join tokens, relay & mailbox/admission, FFI for iOS/Android (already named rightkit-mesh but lives in app repo: move & publish); device link module glue; Swift mesh client; Swift devices; devices UI; device pairing (Swift) (+1 more) |
| `rightkit-disk-cleanup` | crate | Pulse | 25,046 |  |  | bounded metadata scans, cleanup plans/rules w/ undo journal, duplicates, folder growth, snapshots, storage browser, media compression |
| `rightkit-launcher` | crate | Pulse | 22,237 | Swift |  | command launcher: calculator, dictionary, files, notes, rates, snippets, commands (Win32 + Swift) |
| `RightKitSystemUI` | Swift | Pulse | 21,612 | Swift |  | SwiftUI system hub sections: overview, storage, cleanup, monitor, apps, chat, activity, membrane |
| `RightKitNotch` | Swift | Pulse | 17,668 | Swift |  | macOS notch surface: notch window, design system, rings & cards, model |
| `rightkit-app-manager` | crate | Pulse | 13,783 | Swift |  | installed-app inventory, leftovers, uninstall plans, updates, usage, startup items, quit/auto-quit, disk-image install |
| `rightkit-system-monitor` | crate | Pulse | 13,212 | Swift |  | CPU/memory/GPU/disk gauges, process inspection, SMART drive health, metrics history, volumes & eject |
| `rightkit-clipboard-history` | crate | Pulse | 12,760 | Swift |  | clipboard history: capture, detect, images, paste, seal, search, strip, shelf, snippet expansion |
| `@rightkit/system-ui` | npm | Pulse | 7,889 |  |  | React system hub: overview, storage, cleanup, monitor, apps, chat, activity, membrane, drive health, charts |
| `rightkit-desktop-tweaks` | crate | Pulse | 2,032 | Swift |  | Finder cut/paste, path field, window closer, dock-click minimize, window maximizer, event tap hub (Swift + Win) |
| `rightkit-app-lock` | crate | MailRight, Pulse | 1,746 | Swift |  | app lock: OS-auth verification, serialized lock/unlock transitions, DB gate; owner unlock auth / app lock |
| `rightkit-privileged-helper` | crate | Pulse | 673 | Swift |  | privileged helper & elevation broker (macOS SMAppService helper, Windows elevation), trash policy |

### Foundation & tooling

| Package | Kind | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---:|---|---|---|
| `rightkit-ml-lab` | crate | HeardRight | 53,827 | Swift, Python |  | model training & evaluation tooling: KWS/wake corpora & conditioning, ASR bakeoffs, polish-model training/eval, CoreML export |
| `rightkit-bench` | crate | ScrapeRight, MailRight, ViewRight, CodeRight | 27,521 | Swift, Python |  | benchmark corpora runner, scoring, receipts & claim gate; SQLite perf proof & probes, load driver; association & PDF bench matrices; PDF perf bench; agent benchmarks |
| `rightkit-doctor` | crate | PostRight, MailRight, CutRight, CodeRight | 2,262 |  |  | capability health probes behind doctor command; doctor report; doctor dimensions & probes |
| `@rightkit/config` | npm | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 1,057 |  |  | shared toolchain/lint/vite presets; shared presets |
| `@rightkit/waitlist-worker` | npm | MailRight, ViewRight | 529 |  |  | Cloudflare waitlist worker |

### Other

- `rightkit-apple-ops` (Legion, 10,150 lines): Apple ops: App Store Connect API & upload, build analysis, preflight, profiling, SwiftUI traces

## Existing RightKit packages to extend

**64 existing packages** gain capability now living in apps.

| Package | Used by | Lines moving in | What it gains |
|---|---|---:|---|
| `rightkit-workflow` | CodeRight, Legion | 158,335 | autonomous workflow control plane: runs, leases, fences, node executors; workflow port (scenario/state machine runtime) |
| `rightkit-agent-tools` | GenRight, CodeRight | 86,283 | agent tool profile; LSP API; coding tool suite: fs read/edit, shell, search, git, LSP, web, browser, MCP bridge, media |
| `@rightkit/qa` | ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight, Legion | 62,544 | QA capture, visual diff, evidence, test inventory, isolation; mock IPC for UI QA; css owner inventory, QA journeys; mock IPC data for UI QA; QA browser helpers & tooling (+7 more) |
| `rightkit-agents` | PostRight, Pulse, CodeRight | 47,873 | headless drafting via installed Claude/Codex CLI; Claude Code/Codex transcript discovery & normalisation; vendor coding drivers (Claude Code, Codex, Cursor, Cline, Grok, ACP), resumable NDJSON & JSON-RPC transports, login routes; vendor turn |
| `@rightkit/release` | PostRight, ScrapeRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 44,097 | manifest-consistency gate checks; release pin check; bundled runtime staging & artifact verification; gates & sample validation; server deploy scripts (+5 more) |
| `rightkit-llm` | MailRight, CodeRight | 34,931 | BYOK AI actions & translation providers; provider clients, replay, heuristic model routing, provider setup presets, local inference runtime |
| `rightkit-asr` | ScrapeRight, GenRight, HeardRight | 33,816 | Parakeet/Whisper routing & engines (use rightkit-asr + rightkit-speech-client); whisper ASR; prebuilt core xcframework: build from RightKit crates; sherpa-onnx xcframework headers: replace with RightKit Rust KWS/ASR (sherpa rewrite); Parakeet TDT core & ORT exec, CoreML, Whisper, KWS keyword spotting, VAD, calibration, file transcription (+2 more) |
| `RightKitHubQA` | ScrapeRight, Pulse, HeardRight, CodeRight | 28,091 | iOS E2E seeds/probes & tests; hub fixtures, renderer, AX tour; view snapshots; iOS tests & scripts; iOS tests |
| `rightkit-daemon` | PostRight, MailRight, GenRight, CutRight, CodeRight | 23,574 | headless server role (scheduler, public listener, remote host); heartbeat record already owned by rightkit-daemon; generic app CLI client over running app: arg parsing, error envelope & exit codes, pairing key, link; runtime service lifecycle, health snapshot, event hub w/ debounce bridge, lock budgets, repo bundle; installed suite-app discovery (+3 more) |
| `@rightkit/settings-ui` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 22,923 | settings panel; spend allowances panel; store health/diagnostics panel; models section client; runtime/doctor sections & prefs (+6 more) |
| `rightkit-automation` | Pulse, HeardRight | 22,376 | OS permission status & prompts; text delivery into focused app: paste, streaming insertion & reconcile, UIA/AX, terminal targets, TSF tip, focus continuity; system media playback pause/resume while dictating (Mac/Win) |
| `rightkit-overlay-win` | Pulse | 20,798 | Win32 notch surface toolkit: render, text layout, motion, cards, alerts, tool wheel |
| `rightkit-overlay` | HeardRight | 19,095 | recording pill overlay (native Win/Mac, dispatch queue) |
| `rightkit-app-settings` | PostRight, ScrapeRight, MailRight, GenRight, Pulse, CodeRight | 17,892 | settings get/patch for settings-ui; persisted settings reader; UI preference store; settings get/patch; cross-platform settings manifest + codegen (Rust/Swift/Win32/React) (+1 more) |
| `rightkit-review` | PostRight, GenRight, CutRight, Legion | 14,314 | deterministic text quality/approval gate with policy revisions; phone review & pause surface over daemon; human-eyes review gate; final-render QA gate & social packaging checks; independent review & minimal-diff review |
| `rightkit-hooks` | CodeRight, Legion | 12,490 | lifecycle hook bus & runner; hook binary |
| `rightkit-spend` | PostRight, GenRight, CutRight, Pulse | 11,963 | app spend layer over rightkit-spend ledger; gen spend ledger GUI+CLI; productivity/command ledger & preference policy; per-project spend budgets |
| `rightkit-sync` | ScrapeRight, HeardRight | 10,745 | CloudKit transport, phone-to-Mac queue relay, sync settings & runtime; CloudKit transcript sync scheduler |
| `@rightkit/react-shell` | PostRight, ScrapeRight, MailRight, GenRight, CodeRight | 10,496 | useAsync, state router, operator layout css; desktop chrome css, flash/toast, idle task, request gate, formatting; shell/theme/platform/update tests; app lock overlay; error boundary, find bar, shortcut registry & help overlay, dialogs, perf monitor (+2 more) |
| `@rightkit/pdf-ui` | MailRight, ViewRight | 10,411 | pdf preview; PDF editor UI: annotate, text edit, OCR overlay, page ops |
| `rightkit-compose` | GenRight, CutRight | 10,257 | keyless still finishing composite; evaluated timeline -> compositor frame descriptors; finishing/render tests |
| `rightkit-qa` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 10,030 | scenario gate harness (thin runner over rightkit-qa suite); fake editor engine & fake Claude/Codex CLI; mock server already rightkit-qa MockServer; UI scenarios run by rightkit-qa; native UI journeys & engine black-box smoke (+12 more) |
| `rightkit-sandbox` | CodeRight | 7,177 | execution backends, environments, worktree pool |
| `rightkit-job-queue` | PostRight, ScrapeRight, GenRight, Pulse | 5,882 | per-command request/response schema validation; engine contract tests (stay as app conformance via rightkit-qa); durable queue w/ fetch pool + single compute lane, quarantine, event sink; caller leases & detached job runners; activity leases |
| `rightkit-record` | Pulse, HeardRight | 5,095 | screenshots via WGC; screenshot capture for context |
| `@rightkit/theme` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight | 4,982 | generated; Swift brand tokens generated from brand.json; generated brand css; contrast check; brand contract check (+6 more) |
| `RightKitUI` | ScrapeRight, Pulse, HeardRight | 4,730 | SwiftUI settings sections; SwiftUI settings; onboarding |
| `@rightkit/onboarding` | ScrapeRight, GenRight, Pulse, HeardRight, CodeRight | 4,328 | models intro & setup gate; onboarding, legal, licence plan, device allowance; onboarding/legal/licence; mic & calibration onboarding steps; onboarding pages (+1 more) |
| `rightkit-db` | PostRight, MailRight | 4,233 | SQLite store backup, verify & restore; compaction w/ rowid verify, background WAL checkpoints, verified pre-migration backup, migration gate, lock-order gate |
| `rightkit-playback` | ScrapeRight, CutRight | 4,002 | compatibility playback; native playback session over evaluated revision; playback e2e tests |
| `rightkit-crash` | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 3,616 | renderer crash capture; panic capture; crash; out-of-process minidump writer, user feedback, diagnostics transport |
| `rightkit-models` | ScrapeRight, MailRight, GenRight, CutRight, HeardRight, CodeRight | 3,603 | shared model store ops, migration between suite apps, seeding; model fetch; model store glue; memory-aware governor for heavy local models, hardware fit, install, Blender/Wan2GP/sd.cpp doctor; model store adapter (+4 more) |
| `rightkit-align` | CutRight | 3,547 | word timings + VAD for talking-head (on rightkit-asr/vad/align) |
| `RightKitWatch` | HeardRight | 3,476 | watch recording capture, transfer & intents |
| `rightkit-browser` | ScrapeRight, CodeRight | 3,460 | CDP browser backend & address pin; browser workspace for agents |
| `rightkit-motion` | GenRight, CutRight | 3,417 | HyperFrames HTML composition render: loopback host, headless Chrome capture, encode; contact sheets & thumbnails |
| `rightkit-tts` | GenRight | 3,284 | local Qwen3-TTS voice; voice library: clones, presets, provider voices |
| `rightkit-sidecar` | MailRight, CodeRight | 3,112 | process engine split: core child owning DB, IPC transport, event pump, supervision, trust boundary; stream transport |
| `rightkit-channel-memory` | PostRight | 2,515 | channel store & channel memory document (schema already in RK); fixtures |
| `rightkit-ffmpeg` | GenRight, CutRight, CodeRight | 2,232 | pure-Rust media probe (dims, duration, codec); ffmpeg discovery; single H.264 encoder rule; ffmpeg editing build staging & qualification; media tool adapters |
| `rightkit-vault` | Pulse | 1,984 | agent logins & payment secrets |
| `rightkit-contracts` | PostRight | 1,760 | contract.check & delivery envelope IO at app boundary |
| `rightkit-fs` | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 1,747 | data root & private store dirs; engine paths; downloads folder, blocking helper, Windows ACL hardening; roots & path policy; paths (+1 more) |
| `RightKitSpeech` | ScrapeRight, HeardRight, CodeRight | 1,740 | on-device transcription; shared model install store; model download/install, CoreML validation; on-device ASR & KWS bridges; dictation |
| `rightkit-tauri-shell` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 1,731 | asset-protocol scope grants; engine sidecar host; build identity, tray, shell plugin, startup error window; menu, context menu, window buttons, WebView2 env, single instance, associations, print preview; path-grant broker, site-root & preview custom protocols, range-streamed binary reads, contained HTML assets (+6 more) |
| `rightkit-transcript` | ScrapeRight | 1,519 | transcript & article artifact writers, output path naming |
| `rightkit-identity` | Pulse | 1,360 | device identity & short-code pairing |
| `RightKitIntents` | HeardRight | 1,224 | app intents & voice commands |
| `RightKitLiveActivity` | HeardRight, CodeRight | 1,107 | recording live activity views; widgets |
| `rightkit-search` | MailRight | 944 | HTML->indexable text, FTS5 maintenance & background rebuild |
| `rightkit-send` | ScrapeRight, Pulse | 891 | Swift binding/UI over rightkit-send (receive scratchpad, clipboard offer); nearby sharing glue & UI |
| `rightkit-docs` | ViewRight | 695 | document format classification table |
| `RightKitImportQueue` | ScrapeRight | 623 | universal serial job queue & runner; share extension intake |
| `rightkit-license` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 527 | licence config & headless lookup; Apple-device Pro continuity bridge; licence gate & device identity; licence host; Pro gate & trial (+3 more) |
| `@rightkit/ax` | GenRight, Pulse | 489 | agent plugin evals; agent plugin launcher |
| `rightkit-http` | PostRight, ScrapeRight, GenRight, CodeRight | 409 | one-request HTTP layer already on rightkit-http; SSRF URL guard for non-rightkit-http transports; outbound HTTP policy; http |
| `rightkit-embed` | MailRight | 398 | background semantic indexing job |
| `@rightkit/outdated` | Pulse | 324 | upstream donor check |
| `@rightkit/legal` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 308 | generated third-party notices; legal notices; legal config; legal configs & gates; legal acceptance host (+3 more) |
| `@rightkit/media-ui` | ScrapeRight, GenRight | 282 | media player adapters; mask brush & export |
| `rightkit-time` | PostRight, MailRight | 233 | local wall time -> UTC for calendars (tz-aware); UTC stamps |
| `@rightkit/i18n` | PostRight, Pulse | 150 | money micro-units & instant formatting; i18n |
| `rightkit-credentials` | MailRight, GenRight, Pulse, HeardRight | 114 | OS credential store; provider API keys; secure prompt; secrets |
| `rightkit-platform-rules` | PostRight | 107 | pinned platform API versions & sunset dates |

## Already in RightKit: delete app copies & adopt

| Package | Apps still carrying own copy | Lines to delete |
|---|---|---:|
| `@rightkit/legal` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 123,503 |
| `rightkit-qa` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 56,780 |
| `rightkit-tauri-shell` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 13,521 |
| `rightkit-mcp` | CodeRight, Legion | 12,327 |
| `rightkit-models` | ScrapeRight, MailRight, GenRight, CutRight, HeardRight, CodeRight | 10,526 |
| `rightkit-send` | ScrapeRight, Pulse | 9,261 |
| `RightKitHub` | Pulse | 8,117 |
| `rightkit-license` | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 8,009 |
| `@rightkit/i18n` | PostRight, Pulse | 7,215 |
| `rightkit-logs` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, CodeRight | 6,419 |
| `rightkit-job-queue` | PostRight, ScrapeRight, GenRight, Pulse | 4,749 |
| `RightKitLicense` | ScrapeRight, HeardRight, CodeRight | 3,968 |
| `RightKitSpeech` | ScrapeRight, HeardRight, CodeRight | 3,925 |
| `rightkit-fsindex` | Pulse | 2,980 |
| `rightkit-asr` | ScrapeRight, GenRight, HeardRight | 2,803 |
| `@rightkit/onboarding` | ScrapeRight, GenRight, Pulse, HeardRight, CodeRight | 2,755 |
| `rightkit-hotkeys` | Pulse, HeardRight | 2,378 |
| `rightkit-fs` | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 1,716 |
| `rightkit-app-settings` | PostRight, ScrapeRight, MailRight, GenRight, Pulse, CodeRight | 1,522 |
| `rightkit-credentials` | MailRight, GenRight, Pulse, HeardRight | 1,405 |
| `RightKitDiagnostics` | ScrapeRight, HeardRight, CodeRight | 1,367 |
| `RightKitCloudSync` | ScrapeRight | 1,212 |
| `@rightkit/tauri` | PostRight, ScrapeRight, MailRight, GenRight | 1,132 |
| `rightkit-ffmpeg` | GenRight, CutRight, CodeRight | 1,074 |
| `rightkit-tts` | GenRight | 1,054 |
| `rightkit-file-watch` | ViewRight, Pulse, CodeRight | 993 |
| `rightkit-ocr` | ScrapeRight, ViewRight | 987 |
| `rightkit-http` | PostRight, ScrapeRight, GenRight, CodeRight | 959 |
| `rightkit-browser` | ScrapeRight, CodeRight | 932 |
| `RightKitAppleCredentials` | ScrapeRight, HeardRight | 915 |
| `RightKitFS` | CodeRight | 846 |
| `RightKitUpdate` | Pulse, CodeRight | 830 |
| `@rightkit/media-ui` | ScrapeRight, GenRight | 698 |
| `RightKitHTTP` | CodeRight | 644 |
| `rightkit-pdf` | ScrapeRight, ViewRight | 632 |
| `rightkit-onnx` | ScrapeRight, HeardRight, CodeRight | 509 |
| `@rightkit/legal-ui` | ScrapeRight, MailRight | 438 |
| `rightkit-child-process` | ScrapeRight, Pulse | 402 |
| `@rightkit/release` | PostRight, ScrapeRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 367 |
| `rightkit-crash` | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 351 |
| `rightkit-agent-tools` | GenRight, CodeRight | 302 |
| `@rightkit/settings-ui` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 278 |
| `rightkit-playback` | ScrapeRight, CutRight | 276 |
| `rightkit-oauth` | CodeRight | 230 |
| `@rightkit/app-update` | ScrapeRight | 218 |
| `rightkit-daemon` | PostRight, MailRight, GenRight, CutRight, CodeRight | 192 |
| `@rightkit/license` | ScrapeRight | 192 |
| `@rightkit/issues-tracker` | PostRight, MailRight | 176 |
| `@rightkit/platform-ui` | ViewRight | 160 |
| `RightKitHotkeys` | Pulse | 151 |
| `@rightkit/pdf-ui` | MailRight, ViewRight | 144 |
| `@rightkit/theme` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight | 112 |
| `rightkit-cache` | Pulse | 89 |
| `rightkit-time` | PostRight, MailRight | 69 |
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

1. **DOCX editor (ViewRight, 171k lines vendored).** Recommendation: own the `core` & `react` packages as `@rightkit/docx-editor`, drop vendored `agents` & `i18n` (51k). Alternative: keep upstream as an npm dependency & only own ViewRight's integration.
2. **Tree-sitter grammars (Pulse/Membrane, 71k lines vendored).** Recommendation: use crates.io grammar crates through `rightkit-repo-graph`; no RightKit copy.
3. **WasmKit (CodeRight iOS, 27k).** Recommendation: SwiftPM dependency, delete vendored copy.
4. **tauri-runtime-wry fork (HeardRight, 7k).** Recommendation: upstream the patch; until then, one pinned fork owned by `rightkit-tauri-shell`, not per app.
5. **sherpa-onnx & prebuilt xcframeworks (HeardRight iOS).** Recommendation: replace with RightKit's Rust KWS/ASR (`rightkit-asr`) built for iOS.
6. **ONNX Runtime binary (HeardRight iOS).** Keep as binary dependency, resolved only through `rightkit-onnx`.
7. **GenRight legacy Node server (`server/`, `compat-lib/`, `bin/`, 8k lines).** Mapped DROP as superseded by the Rust studio port; confirm nothing still calls it.
8. **Skill packs (`@rightkit/skills`).** Legion's doctrine & skills and CutRight's designer engine (incl. vendored huashu) become one content package; `rightkit-skills` is the crate that loads & projects them.
9. **`rightkit-mesh`.** Already named as a RightKit crate but lives inside the Pulse repo; move it to RightKit & publish.
10. **Single-app packages.** 50+ new packages have one consumer today (e.g. `rightkit-code-audit`, `rightkit-terminal-ui`, video editing family). They are listed as RightKit packages per the factory goal; the family grouping above is the order to extract them in.
