# RightKit package master plan

Date: 2026-11-01. Built by going through all 10 app repositories file by file (every tracked code file on `main`), mapping each to the RightKit package it should become or use, then merging overlaps across apps. Per-app tables: `docs/rightkit-inventory/<app>-rightkit-mapping.md` in each app repo (plus a JSON with every file).

Goal: no app code outside RightKit except a thin app shell (entry points, command tables, wiring). Package names are product-neutral; a package used by only one app today is still a RightKit package.

## Coverage by app

| App | Code lines | NEW | EXTEND | ADOPT | To Legion | Vendor | DROP | App recipes (stay) | App tests (stay) | Shell+config (stay) | Shell % |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| PostRight | 147,942 | 57,710 | 15,658 | 1,372 | 0 | 0 | 468 | 0 | 72,363 | 371 | 0.3% |
| ScrapeRight | 103,419 | 46,095 | 34,291 | 11,537 | 0 | 0 | 2,573 | 0 | 8,510 | 413 | 0.4% |
| MailRight | 113,155 | 80,218 | 26,927 | 3,664 | 0 | 0 | 164 | 0 | 1,868 | 314 | 0.3% |
| ViewRight | 363,273 | 83,202 | 19,440 | 1,823 | 0 | 208,933 | 13,663 | 0 | 34,140 | 2,072 | 0.6% |
| GenRight | 174,925 | 108,784 | 25,482 | 4,118 | 0 | 0 | 8,627 | 0 | 27,033 | 881 | 0.5% |
| CutRight | 210,742 | 94,577 | 23,802 | 1,332 | 0 | 6,675 | 513 | 42,787 | 40,422 | 634 | 0.3% |
| Pulse | 673,096 | 447,784 | 83,563 | 34,679 | 0 | 71,409 | 23,187 | 0 | 11,480 | 994 | 0.1% |
| HeardRight | 460,166 | 174,797 | 148,766 | 17,457 | 0 | 50,957 | 31,365 | 0 | 35,811 | 1,013 | 0.2% |
| CodeRight | 963,727 | 571,755 | 257,545 | 27,012 | 0 | 26,679 | 13,415 | 0 | 66,200 | 1,121 | 0.1% |
| Legion | 663,533 | 374,578 | 198,531 | 1,357 | 71,060 | 0 | 0 | 0 | 16,967 | 1,040 | 0.2% |
| **All** | **3,873,978** | **2,039,500** | **834,005** | **104,351** | **71,060** | **364,653** | **93,975** | **42,787** | **314,794** | **8,853** | **0.2%** |

Every capability goes to RightKit, single-app ones included. The app keeps only its look and composition of the shared shell (shell/config), declared per-app exceptions, and its journey tests. Unit tests move with their code; app journey/e2e tests stay with the app. Generated third-party notices, docs and data are not counted.

## Final list: new RightKit packages to create & publish

**86 new packages**: 64 crates, 16 npm packages, 6 Swift packages, all public like the rest of RightKit. Wave 2 = used by two or more apps, wave 3 = one app today (still RightKit, so the next app can find it). Grouped by family; within a family, ordered by size.

### Agent platform

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-agent-engine` | crate | 2 | Pulse, CodeRight | 129,815 | Swift | `app-shell`, `rightkit-agent-runtime` | always-on agent: task envelope & transitions, principals, intents, effects, device/approval binding, voice handoff; agent loop, providers, budget (Swift: RightKitAgent); task CLI; Swift agent UI/bridge; windows agent glue; agent loop, edit history & diffs, task graph, runtime middleware & bounded resources, attention, session storage, context folds/rehydrate, trajectory journal, wire protocol; interactive & headless session host (+1 more) |
| `@rightkit/agent-ui` | npm | 2 | MailRight, Pulse, CodeRight | 121,233 |  |  | agent approval cards, pairing & audit views; approval IPC client; remote approvals & owner settings; agent workbench UI: chat timeline, composer, sessions, runtime/goals/memory/review panels, approvals, plugins/skills, orchestration, bots, environments, state stores; computer-use & browser panels; remote web client |
| `rightkit-terminal-ui` | crate | 3 | CodeRight | 94,376 |  |  | terminal UI toolkit: composer, transcript, wrapping, panes, markdown render, image input, render metrics |
| `rightkit-agent-server` | crate | 3 | CodeRight | 91,539 |  | `app-shell` | OpenAPI client codegen & contract checks; agent CLI & daemon runtime: CLI definitions, run/exec input/output/exit codes, config, presence, recovery, remote setup, project binding, skill/plugin/recipe routes over the installed Legion; agent daemon HTTP API: sessions, runtime, contract/OpenAPI, tool invocations, run changes, health, workspace, teams, coordination; server & daemon client runtime |
| `rightkit-agent-orchestration` | crate | 2 | CodeRight, Legion | 89,148 |  | `app-shell`, `rightkit-work-router` | agent room; multi-agent coordination: subagents, teams, delegation, roster; run, budget & schedule commands; handoff packets, continuity gate, live state; capability routing runtime: plan, route, task, budget, grant, escalation, delivery, inventories, platform layers, skills/designer checks |
| `rightkit-evidence` | crate | 2 | CodeRight, Legion | 39,158 |  | `app-shell`, `rightkit-specs` | spec-governed artifacts & coverage; evidence, freeze, contract & decision commands; contracts, effect execution & receipts, decision store, evidence/SARIF/HTML reports, certification & governance lifecycle |
| `rightkit-agent-gateway` | crate | 2 | MailRight, Pulse | 35,378 |  | `rightkit-remote-control` | paired external agents over local daemon: method registry, gate, approval cards (send/forward/delete/attach), audit log, untrusted-content contract, redaction; remote tools for agents (files, app launch, clipboard, chat read/start/term, browser address), local approvals, audit trail; agent gateway: LLM provider & tool routing |
| `RightKitAgent` | Swift | 2 | Pulse, CodeRight | 34,981 | Swift | `RightKitRemoteUI` | phone remote-control screens; Swift agent client: local runtime bridge, chat library, code editor, diff walkthrough, plan review, markdown rendering, approvals |
| `rightkit-chat-bridge` | crate | 3 | Pulse | 28,614 | Swift |  | deliver messages into running Claude/Codex chats (hooks, UIA/AX typing), envelopes, hub heartbeat, chat skill install, cross-user boundary |
| `rightkit-agent-policy` | crate | 2 | CodeRight, Legion | 25,061 |  |  | pure permission decision engine, destructive-action guard, policy packs, audit; policy model, rules & enforcement |
| `rightkit-ai-accounts` | crate | 3 | Pulse | 18,113 | Swift | `app-shell`, `rightkit-ai-usage`, `rightkit-claude-accounts` | Claude Desktop/CLI account switching & Code-session sync across accounts; Claude/Codex plan usage, credentials, resets & credits (Swift providers); Claude sessions (Swift); Claude Desktop state on Windows |
| `rightkit-approvals` | crate | 2 | MailRight, GenRight, Pulse | 16,672 | Swift |  | user approval cards for agent send/forward/delete/attach; approval tokens & cards for paid drafts/deliveries (minted by shell, verified by engine); signed phone/owner approvals for privileged & reserved actions; Windows admin service; Swift approvals |
| `rightkit-scheduler` | crate | 3 | CodeRight | 9,315 |  |  | cron schedules & triggers for agent runs |
| `rightkit-github` | crate | 2 | CodeRight, Legion | 9,182 | Swift | `app-shell` | Swift forge connectors: GitHub, Cloudflare, Fly.io, git, PRs; GitHub App: auth, webhooks, PR/issue ops; GitHub delivery observation |
| `rightkit-bot-messaging` | crate | 3 | CodeRight | 6,489 |  |  | remote access: Telegram bot, web auth, remote gateway |
| `rightkit-agent-demo` | crate | 3 | CutRight | 4,434 | Swift |  | consented agent demo mode: agent drives a catalog app via rightkit-automation, recorded via rightkit-record, badge, kill hotkey, bundles |
| `@rightkit/terminal-ui` | npm | 3 | CodeRight | 1,370 |  |  | xterm terminal pane |

### Context, memory & code intelligence

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-code-audit` | crate | 3 | Legion | 254,890 | Swift, Gradle | `app-shell` | audit/verify commands & audit bench; audit CLI commands; repository audit engine: native providers (security, architecture, data, frontend, platform), DAG & scheduler, evidence issuers, findings history, review runtime, provider SDK, rule registry & qualification corpora |
| `rightkit-context` | crate | 3 | Pulse | 98,511 | Swift, Python |  | host side of context engine; context engine: protocol, client, provider SDK, federation, budget/lanes, runtime, schemas, hooks |
| `rightkit-repo-graph` | crate | 2 | Pulse, CodeRight | 37,879 |  |  | repository graph: structure, evidence, drift; tree-sitter skeletonization |
| `rightkit-memory` | crate | 2 | Pulse, CodeRight | 24,602 |  |  | durable memory store (Cortex): scopes, identity, telemetry; adaptive memory retrieval & backend (over context engine) |
| `rightkit-adapt` | crate | 2 | Pulse, CodeRight | 23,896 |  |  | governed behavioural learning (taste + insights); post-session learning pipeline |
| `rightkit-research` | crate | 2 | PostRight, Legion | 11,662 |  | `app-shell` | dated research digests & source readers (Reddit); research command; research pipeline (sources, claims, evidence) |

### Dictation & voice

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-dictation` | crate | 3 | HeardRight | 60,504 | Python |  | dictation runtime: mic capture & conditioning, session state machine, JSON-RPC sidecar worker, wake listener, recording lifecycle |
| `rightkit-text-polish` | crate | 3 | HeardRight | 18,564 | Swift |  | Swift polish clients (Apple Intelligence, BYOK); deterministic polish, LLM cleanup (L1-L3), vocabulary & learning, screen context |
| `RightKitDictation` | Swift | 3 | HeardRight | 10,514 | Swift |  | iOS dictation: recording session, warm capture, long-form, import queue, live activity |
| `rightkit-voice-commands` | crate | 3 | HeardRight | 9,784 | Swift |  | voice command recognition/classification, launch & OS command adapters |
| `@rightkit/dictation-ui` | npm | 2 | HeardRight, CodeRight | 9,765 |  | `@rightkit/overlay-ui`, `@rightkit/text-polish-ui` | dictation home/controls, banners, command palette, PTT, sounds, metrics; polish & vocabulary pages; recording pill webview UI; voice conversation |

### Video editing

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `@rightkit/video-ui` | npm | 3 | CutRight | 16,768 |  | `@rightkit/timeline-ui` | project, captions, colour, shorts, recipes, outputs, batch & finished-edit screens; video editor UI: timeline, preview, inspector; vendored hyperframes player bits; client lib & engine bindings |
| `rightkit-video-project` | crate | 3 | CutRight | 14,524 |  |  | video project layer: sources & integrity, revisions, portable bundle export/import, card slots, cache storage; media workflow methods: projects, transcription, render |
| `rightkit-autoedit` | crate | 3 | CutRight | 14,279 |  | `rightkit-edit-rules` | transcript-driven cut & editorial engines, edit plans, named variants, style measurement, mic alignment; scoped editing rules & resolution matrix, autonomy levels |
| `rightkit-timeline` | crate | 3 | CutRight | 8,011 |  | `rightkit-timeline-interchange` | FCPXML & OpenTimelineIO export; portable timeline core: tracks, clips, rational time, edits, undo |
| `rightkit-shorts` | crate | 3 | CutRight | 4,992 |  |  | shorts finder & scoring, on-screen-text moment search |
| `rightkit-reframe` | crate | 3 | CutRight | 4,222 |  |  | tracked 9:16 reframe: ONNX face detect, tracking, smoothed crop path |
| `rightkit-color` | crate | 3 | CutRight | 4,108 |  |  | colour grading & scopes |
| `rightkit-captions` | crate | 3 | CutRight | 3,177 |  |  | caption styles, per-language caption sets, placement, swatch preview |
| `rightkit-compositor` | crate | 3 | CutRight | 3,088 |  |  | native wgpu layer compositor, GPU effects & SDF masks (adapted from OpenCut, MIT) |
| `RightKitMedia` | Swift | 3 | CutRight | 1,859 | Swift |  | macOS media worker (Swift): AVFoundation render, audio finish, typography, Vision, motion compositor; Silero VAD & vision anchor |

### Generative media

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-media-gen` | crate | 3 | GenRight | 34,258 |  |  | generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, HeyGen avatars, EvoLink, Higgsfield, Muapi, NanoBanana, Replicate, Wan2GP, Blender), model catalogue & facts, routing, durable gen jobs, result descriptors, balances |
| `@rightkit/media-gen-ui` | npm | 3 | GenRight | 21,626 |  |  | generation studio UI: generate forms, gallery & library, jobs, recipes & templates, character/quality/video panels, voices, avatars, motion, production, pipelines, KDP, local image chat |
| `rightkit-local-gen` | crate | 3 | GenRight | 12,312 |  | `rightkit-lipsync`, `rightkit-local-image`, `rightkit-music-gen`, `rightkit-upscale` | keyless local image gen: sd.cpp server runtime, memory modes, profiles, job store, artifacts; local Wav2Lip talking-head lip-sync on ONNX; local MusicGen ONNX music generation; local Real-ESRGAN image upscale |
| `rightkit-prompt-recipes` | crate | 3 | GenRight | 12,135 |  |  | prompt contracts & validator, recipe composer & catalogue, characters/LoRA lock, video quality templates, template swap, brand codes, KDP workflow |
| `rightkit-media-pipeline` | crate | 3 | GenRight | 9,149 |  |  | named production pipelines: compose, dubbing w/ retiming, translation w/ terminology, fan-out multi-job steps, finishing handoff, campaign factory |
| `rightkit-blender-worker` | crate | 3 | GenRight | 4,386 | Python |  | Blender render worker (Python, runs inside Blender): scene, materials, product registration/review, finish profiles, contract |
| `rightkit-remote-compute` | crate | 3 | GenRight | 2,681 |  |  | rented GPU workers (Vast): register/stage/run lifecycle, time-billed spend |

### Content pipeline & social

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-pipeline` | crate | 2 | PostRight, GenRight, CutRight | 17,975 |  |  | dispatch of content briefs to another engine as external process; brief run / delivery envelope handling between suite engines; brief check/run, channel recipes, unattended recipe runs, durable batch delivery; cross-engine orchestration: call generation engine, follow jobs, place results |
| `@rightkit/social-ui` | npm | 3 | PostRight | 17,034 |  | `@rightkit/content-ui`, `@rightkit/social-client` | shared labels & scope select; TS client & types for social/content engine commands (generate from Rust); typed mock data source for UI QA; accounts, publish jobs, pacing, failures, inbox, DM threads screens; analytics, campaigns & links screens; content planning screens |
| `rightkit-social-publish` | crate | 3 | PostRight | 16,930 |  | `rightkit-social-accounts` | connected platform accounts, readiness, platform data-deletion requests; durable publish job phase machine + platform adapters (X, Bluesky, Mastodon, Threads, Instagram, Facebook, LinkedIn, Pinterest, TikTok, YouTube); per-account quota pacing & publish job wire types |
| `rightkit-social-analytics` | crate | 2 | PostRight, CutRight | 6,486 |  | `rightkit-link-tracking` | analytics pull/store, manual metric import, calendar optimisation proposals; campaigns, tracked short links & visit records; retention feedback import & analysis |
| `rightkit-content-plan` | crate | 3 | PostRight | 4,599 |  | `rightkit-content-briefs`, `rightkit-content-calendar` | brief & calendar entry cache, proposals; typed briefs, platform variants, hook tests, manual drafts |
| `rightkit-content-proof` | crate | 3 | PostRight | 3,820 |  |  | engine data assets (catalogues); acquisition chain gates & claim/proof semantics |
| `rightkit-social-inbox` | crate | 3 | PostRight | 3,070 |  |  | signed webhook inbox & keyword-comment DM automation |

### Ingest & library

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-library` | crate | 2 | PostRight, ScrapeRight, GenRight, HeardRight | 14,388 |  | `rightkit-text-enrich` | reusable media records with occurrence ids & expiry; non-LLM enrichment: keywords, links, source tags, text segmentation; local artifact library: history authority, SQLite FTS index, delete/restore; owned media library: import w/ provenance, asset facts (origin/evidence/rights), FTS search, artifact index; transcript history: SQLCipher store, key rotation, retention, recovery, export |
| `@rightkit/library-ui` | npm | 2 | ScrapeRight, HeardRight | 11,064 |  | `@rightkit/queue-ui` | job queue UI: paste bar, drop overlay, job cards, queue board, engine event hook; library list, transcript/article reader, find-in-document, workbench; transcript history & file transcribe |
| `rightkit-media-fetch` | crate | 2 | ScrapeRight, GenRight | 9,877 | Swift | `rightkit-source-detect`, `rightkit-stock-media` | yt-dlp download/normalize, playlists & channels, X/TikTok/Instagram post, carousel & image capture; URL/file job classifier, media-type table, URL & host helpers; fetch-pool vs compute-lane split; Swift port of detect; replace with UniFFI binding; stock & archive acquisition (Pexels, Pixabay, Internet Archive) w/ licence facts |
| `RightKitLibrary` | Swift | 2 | ScrapeRight, HeardRight | 6,056 | Swift | `RightKitLibraryUI` | Swift artifact library & markdown export; SwiftUI queue, library, result detail, receive views; SwiftUI history & settings views; Swift transcript library, insights, cloud sync |
| `rightkit-web-session` | crate | 2 | ScrapeRight, Pulse | 4,168 | Swift |  | signed-in web sessions: in-app sign-in webview, cookie import/export, profile dirs; agent browser logins on rightkit-browser; Swift browser logins |
| `rightkit-web-extract` | crate | 3 | ScrapeRight | 2,734 | Swift | `RightKitExtract` | article extraction ladder: native fetch w/ TLS fingerprint, browser render, readability, content filters; Swift extractors: article, carousel, media URL, PDF, OCR, WebView render, fetch policy |

### Documents

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `@rightkit/document-workspace` | npm | 3 | ViewRight | 31,171 |  |  | multi-format document workspace: tabs & panes, drafts, autosave, external-change watcher, find & cross-doc search, command palette, explorer/library sidebar, print pipeline, zoom, plain-text editor; workspace styles |
| `@rightkit/markdown-editor` | npm | 3 | ViewRight | 27,905 |  |  | markdown/MDX hybrid editor: projection, block & inline editing (code, math, mermaid, tables), citations, references, properties, export, safe HTML |
| `@rightkit/html-editor` | npm | 3 | ViewRight | 15,898 |  |  | HTML document view/authoring: sandboxed session, blocks, site-root resources |
| `rightkit-document-workspace` | crate | 3 | ViewRight | 3,147 |  |  | Rust side of document workspace: recent files & pins DB, explorer listing, markdown index/resource inspection, document IO |
| `@rightkit/docx-editor` | npm | 3 | ViewRight | 1,834 |  |  | DOCX view/edit integration |

### Mail & workspace

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-mail` | crate | 3 | MailRight | 27,603 |  |  | provider-neutral local mail mirror: schema, outbox & pending ops, sync apply/rollups, recipients, risk/trust signals, remote-image proxy & tracker blocking, attachment cache, engine method groups |
| `@rightkit/mail-ui` | npm | 3 | MailRight | 17,055 |  |  | inbox virtual list, reader & sandboxed email frame, compose editor & recipients, search routing/coverage/saved searches, image trust, attachment preview, account switcher, mail store |
| `rightkit-google-apis` | crate | 3 | MailRight | 8,778 |  |  | Google Workspace clients: Gmail sync/history/batch/MIME/compose/labels/filters/vacation/drafts, Calendar, Drive, People/Contacts, Tasks, Meet, budgets & backoff |
| `@rightkit/workspace-ui` | npm | 3 | MailRight | 2,722 |  |  | calendar grid & event forms, contacts, drive browser & preview, tasks, meet |

### System utilities

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-mesh` | crate | 2 | Pulse, HeardRight, CodeRight | 35,883 | Swift, Gradle | `app-shell` | device mesh (iroh): link, device circle, join tokens, relay & mailbox/admission, FFI for iOS/Android (already named rightkit-mesh but lives in app repo: move & publish); device CLI; device link module glue; paired computers (Swift); Swift mesh client; Swift devices (+3 more) |
| `rightkit-disk-cleanup` | crate | 3 | Pulse | 25,046 |  |  | bounded metadata scans, cleanup plans/rules w/ undo journal, duplicates, folder growth, snapshots, storage browser, media compression |
| `RightKitNotch` | Swift | 3 | Pulse | 22,385 | Swift | `app-shell` | macOS notch surface: notch window, design system, rings & cards, model; macOS notch app host: delegate, hub bridge & launcher, engine service, terminal command, l10n |
| `rightkit-launcher` | crate | 3 | Pulse | 22,237 | Swift |  | command launcher: calculator, dictionary, files, notes, rates, snippets, commands (Win32 + Swift) |
| `RightKitSystemUI` | Swift | 3 | Pulse | 21,612 | Swift |  | SwiftUI system hub sections: overview, storage, cleanup, monitor, apps, chat, activity, membrane |
| `rightkit-app-manager` | crate | 3 | Pulse | 14,470 | Swift | `app-shell` | installed-app inventory, leftovers, uninstall plans, updates, usage, startup items, quit/auto-quit, disk-image install; process control CLI |
| `rightkit-system-monitor` | crate | 3 | Pulse | 13,332 | Swift | `app-shell` | CPU/memory/GPU/disk gauges, process inspection, SMART drive health, metrics history, volumes & eject; process probe |
| `rightkit-clipboard-history` | crate | 3 | Pulse | 12,760 | Swift |  | clipboard history: capture, detect, images, paste, seal, search, strip, shelf, snippet expansion |
| `@rightkit/system-ui` | npm | 3 | Pulse | 7,889 |  |  | React system hub: overview, storage, cleanup, monitor, apps, chat, activity, membrane, drive health, charts |
| `rightkit-desktop-tweaks` | crate | 3 | Pulse | 2,168 | Swift | `app-shell` | Finder sync extension; Finder cut/paste, path field, window closer, dock-click minimize, window maximizer, event tap hub (Swift + Win) |
| `rightkit-app-lock` | crate | 2 | MailRight, Pulse | 2,013 | Swift | `app-shell` | app lock: OS-auth verification, serialized lock/unlock transitions, DB gate; Windows Hello owner check; owner unlock auth / app lock |
| `rightkit-privileged-helper` | crate | 3 | Pulse | 673 | Swift |  | privileged helper & elevation broker (macOS SMAppService helper, Windows elevation), trash policy |

### Foundation & tooling

| Package | Kind | Wave | Used by | Lines moving in | Also ships | Merged from | What it owns |
|---|---|---|---|---:|---|---|---|
| `rightkit-ml-lab` | crate | 3 | HeardRight | 53,827 | Swift, Python |  | model training & evaluation tooling: KWS/wake corpora & conditioning, ASR bakeoffs, polish-model training/eval, CoreML export |
| `rightkit-bench` | crate | 2 | ScrapeRight, MailRight, ViewRight, CodeRight | 27,521 | Swift, Python |  | benchmark corpora runner, scoring, receipts & claim gate; SQLite perf proof & probes, load driver; association & PDF bench matrices; PDF perf bench; agent benchmarks |
| `rightkit-engine-host` | crate | 2 | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse, HeardRight, Legion | 19,720 |  | `app-shell` | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client |
| `rightkit-apple-ops` | crate | 3 | Legion | 11,306 |  | `app-shell` | Apple ops CLI/MCP gate; Apple ops: App Store Connect API & upload, build analysis, preflight, profiling, SwiftUI traces |
| `rightkit-doctor` | crate | 2 | PostRight, MailRight, GenRight, CutRight, CodeRight, Legion | 4,166 |  | `app-shell` | capability health probes behind doctor command; doctor report; doctor command; doctor dimensions & probes |
| `@rightkit/config` | npm | 2 | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 1,057 |  |  | shared toolchain/lint/vite presets; shared presets |
| `@rightkit/waitlist-worker` | npm | 2 | MailRight, ViewRight | 529 |  |  | Cloudflare waitlist worker |

## Existing RightKit packages to extend

**74 existing packages** gain capability now living in apps.

`single` = only one app adds code here today; `shared` = several apps.

| Package | Wave | Apps | Used by | Lines moving in | What it gains |
|---|---|---|---|---:|---|
| `rightkit-workflow` | 1 | shared | CodeRight, Legion | 158,335 | autonomous workflow control plane: runs, leases, fences, node executors; workflow port (scenario/state machine runtime) |
| `rightkit-agent-tools` | 1 | shared | GenRight, CodeRight | 86,283 | agent tool profile; LSP API; coding tool suite: fs read/edit, shell, search, git, LSP, web, browser, MCP bridge, media |
| `@rightkit/qa` | 1 | shared | ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight, CodeRight, Legion | 55,827 | QA capture, visual diff, evidence, test inventory, isolation; mock IPC for UI QA; mock IPC data for UI QA; QA browser helpers & tooling; browser mock API for UI QA (+5 more) |
| `@rightkit/release` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 54,202 | manifest-consistency gate checks; release config logic moves into @rightkit/release; app keeps a JSON data config (P8); release pin check; bundled runtime staging & artifact verification; gates & sample validation (+7 more) |
| `rightkit-agents` | 1 | shared | PostRight, Pulse, CodeRight | 47,632 | headless drafting via installed Claude/Codex CLI; Claude Code/Codex transcript discovery & normalisation; vendor coding drivers (Claude Code, Codex, Cursor, Cline, Grok, ACP), resumable NDJSON & JSON-RPC transports, login routes; vendor turn |
| `rightkit-asr` | 1 | shared | ScrapeRight, GenRight, HeardRight | 34,995 | Parakeet/Whisper routing & engines (use rightkit-asr + rightkit-speech-client); API gap first: api-gap: rightkit-transcript 0.4.0 export::stamp is private; see docs/rightkit-exceptions/outside-M-rows.md#it; API gap first: api-gap: rightkit-speech-client 0.4.0 transcribe_via_service_policy lacks running-host, capability, window-fit; whisper ASR; prebuilt core xcframework: build from RightKit crates (+4 more) |
| `rightkit-llm` | 1 | shared | MailRight, CodeRight | 34,931 | BYOK AI actions & translation providers; provider clients, replay, heuristic model routing, provider setup presets, local inference runtime |
| `rightkit-overlay-win` | 1 | single | Pulse | 31,227 | Win32 notch surface toolkit: render, text layout, motion, cards, alerts, tool wheel; native Win32 notch app host: message loop, hub bridge & launch, lifecycle, placement, RAII, engine client |
| `rightkit-daemon` | 1 | shared | PostRight, MailRight, GenRight, CutRight, Pulse, CodeRight | 25,685 | headless server role (scheduler, public listener, remote host); heartbeat record already owned by rightkit-daemon; generic app CLI client over running app: arg parsing, error envelope & exit codes, pairing key, link; runtime service lifecycle, health snapshot, event hub w/ debounce bridge, lock budgets, repo bundle; installed suite-app discovery (+4 more) |
| `rightkit-tauri-shell` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 24,512 | build identity, icon embedding, tauri config restore; Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call); asset-protocol scope grants; API gap first: api-gap: rightkit-job-queue 0.5.1 cannot hold the first spawn until a first snapshot exists; see docs/rightkit; engine sidecar host (+18 more) |
| `@rightkit/settings-ui` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 22,923 | settings panel; spend allowances panel; store health/diagnostics panel; models section client; runtime/doctor sections & prefs (+6 more) |
| `rightkit-automation` | 1 | shared | Pulse, HeardRight | 21,483 | OS permission status & prompts; text delivery into focused app: paste, streaming insertion & reconcile, UIA/AX, terminal targets, TSF tip, focus continuity; system media playback pause/resume while dictating (Mac/Win) |
| `rightkit-app-settings` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight, Pulse, HeardRight, CodeRight | 20,320 | settings get/patch for settings-ui; persisted settings reader; API gap first: MOVE @rightkit/settings-ui and the shell log host (multi-file log tail preview with reveal, bogusyogi/rightkit; UI preference store; settings get/patch (+4 more) |
| `rightkit-overlay` | 1 | single | HeardRight | 17,672 | recording pill overlay (native Win/Mac, dispatch queue) |
| `@rightkit/react-shell` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 15,669 | app frame composition, splash, engine API client (app keeps a short composition); useAsync, state router, operator layout css; desktop chrome css, flash/toast, idle task, request gate, formatting; shell/theme/platform/update tests; app lock overlay (+3 more) |
| `rightkit-review` | 1 | shared | PostRight, GenRight, CutRight, Legion | 12,623 | deterministic text quality/approval gate with policy revisions; phone review & pause surface over daemon; human-eyes review gate; final-render QA gate & social packaging checks; independent review & minimal-diff review |
| `rightkit-hooks` | 1 | shared | CodeRight, Legion | 12,490 | lifecycle hook bus & runner; hook binary |
| `rightkit-spend` | 1 | shared | PostRight, GenRight, CutRight, Pulse | 11,027 | app spend layer over rightkit-spend ledger; gen spend ledger GUI+CLI; productivity/command ledger & preference policy; per-project spend budgets |
| `RightKitUI` | 1 | shared | ScrapeRight, Pulse, HeardRight, CodeRight | 10,601 | iOS app scaffold: app entry, content/tab views, sheets, menus, settings & account, update, haptics, shortcuts, widgets, file provider; SwiftUI settings sections; SwiftUI settings; onboarding |
| `@rightkit/pdf-ui` | 1 | shared | MailRight, ViewRight | 10,411 | pdf preview; PDF editor UI: annotate, text edit, OCR overlay, page ops |
| `rightkit-sync` | 1 | shared | ScrapeRight, HeardRight | 8,935 | CloudKit transport, phone-to-Mac queue relay, sync settings & runtime; CloudKit transcript sync scheduler |
| `rightkit-qa` | 1 | shared | PostRight, MailRight, HeardRight, CodeRight | 8,821 | fake editor engine & fake Claude/Codex CLI; mock server already rightkit-qa MockServer; test sandbox; process-wide test sandbox; test-only env switches (+3 more) |
| `@rightkit/theme` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 7,434 | generated; app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell; Swift brand tokens generated from brand.json; generated brand css; contrast check (+8 more) |
| `rightkit-sandbox` | 1 | single | CodeRight | 7,177 | execution backends, environments, worktree pool |
| `rightkit-compose` | 1 | shared | GenRight, CutRight | 6,293 | keyless still finishing composite; evaluated timeline -> compositor frame descriptors |
| `rightkit-job-queue` | 1 | shared | PostRight, ScrapeRight, GenRight, Pulse | 5,882 | per-command request/response schema validation; durable queue w/ fetch pool + single compute lane, quarantine, event sink; caller leases & detached job runners; activity leases |
| `rightkit-license` | 1 | shared | ScrapeRight, MailRight, ViewRight, GenRight, CutRight, HeardRight, CodeRight | 5,185 | licence config & headless lookup; API gap first: api-gap: scraperight is not in contracts/right-suite-api.v1.json, so right_suite(app) update auth refuses it; ; Apple-device Pro continuity bridge; licence gate & device identity; API gap first: MOVE rightkit-license. See docs/rightkit-exceptions/M12.md#b1-revocation-denylist-src-taurisrcpro_gaters-revok (+17 more) |
| `rightkit-record` | 1 | shared | Pulse, HeardRight | 5,095 | screenshots via WGC; screenshot capture for context |
| `rightkit-models` | 1 | shared | ScrapeRight, MailRight, GenRight, CutRight, HeardRight, CodeRight | 4,953 | shared model store ops, migration between suite apps, seeding; API gap first: api-gap: rightkit-models 0.5.1 has no bundled-model seed or staging/replaced sweep; see docs/rightkit-exceptio; API gap first: MOVE @rightkit/release. See docs/rightkit-exceptions/M19.md#b1-embedding-model-bundle-provisioning-script-and-; model store glue; memory-aware governor for heavy local models, hardware fit, install, Blender/Wan2GP/sd.cpp doctor (+7 more) |
| `@rightkit/onboarding` | 1 | shared | ScrapeRight, GenRight, Pulse, HeardRight, CodeRight | 4,328 | models intro & setup gate; onboarding, legal, licence plan, device allowance; onboarding/legal/licence; mic & calibration onboarding steps; onboarding pages (+1 more) |
| `rightkit-browser` | 1 | shared | ScrapeRight, CodeRight | 4,114 | API gap first: api-gap: rightkit-browser 0.3.6 BrowserPage has no user-agent or extra-headers setter; see docs/rightkit-excep; CDP browser backend & address pin; browser workspace for agents |
| `rightkit-db` | 1 | shared | PostRight, MailRight | 3,763 | SQLite store backup, verify & restore; compaction w/ rowid verify, background WAL checkpoints, verified pre-migration backup, migration gate, lock-order gate |
| `RightKitHubQA` | 1 | single | Pulse | 3,531 | hub fixtures, renderer, AX tour; view snapshots |
| `RightKitWatch` | 1 | single | HeardRight | 3,476 | watch recording capture, transfer & intents |
| `rightkit-tts` | 1 | single | GenRight | 3,284 | local Qwen3-TTS voice; voice library: clones, presets, provider voices |
| `rightkit-motion` | 1 | shared | GenRight, CutRight | 3,245 | HyperFrames HTML composition render: loopback host, headless Chrome capture, encode; contact sheets & thumbnails |
| `rightkit-ffmpeg` | 1 | shared | GenRight, CutRight, CodeRight | 3,213 | pure-Rust media probe (dims, duration, codec); ffmpeg discovery; single H.264 encoder rule; ffmpeg editing build staging & qualification; media tool adapters (+1 more) |
| `rightkit-sidecar` | 1 | shared | MailRight, CodeRight | 3,112 | process engine split: core child owning DB, IPC transport, event pump, supervision, trust boundary; stream transport |
| `rightkit-crash` | 1 | shared | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 3,019 | renderer crash capture; panic capture; crash; out-of-process minidump writer, user feedback, diagnostics transport |
| `rightkit-android` | 1 | shared | Pulse, HeardRight | 2,630 | Android app scaffold: activity, permissions, screens, QR scanner, mesh client |
| `rightkit-align` | 1 | single | CutRight | 2,477 | word timings + VAD for talking-head (on rightkit-asr/vad/align) |
| `rightkit-fs` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 2,444 | API gap first: api-gap, rightkit-logs 0.3.5 redacts structured JSON by key (redact.rs:51) and text by shape or name=value (sc; engine paths; API gap first: api-gap: rightkit-fs 0.4.7 AppDirs has no user Downloads folder; see docs/rightkit-exceptions/M8.md#item-1; downloads folder, blocking helper, Windows ACL hardening; roots & path policy (+2 more) |
| `rightkit-vault` | 1 | single | Pulse | 1,984 | agent logins & payment secrets |
| `rightkit-playback` | 1 | shared | ScrapeRight, CutRight | 1,819 | compatibility playback; native playback session over evaluated revision |
| `RightKitSpeech` | 1 | shared | ScrapeRight, HeardRight, CodeRight | 1,740 | on-device transcription; shared model install store; model download/install, CoreML validation; on-device ASR & KWS bridges; dictation |
| `rightkit-channel-memory` | 1 | single | PostRight | 1,618 | channel store & channel memory document (schema already in RK); fixtures |
| `rightkit-transcript` | 1 | single | ScrapeRight | 1,519 | transcript & article artifact writers, output path naming |
| `rightkit-identity` | 1 | single | Pulse | 1,360 | device identity & short-code pairing |
| `RightKitIntents` | 1 | single | HeardRight | 1,224 | app intents & voice commands |
| `rightkit-contracts` | 1 | single | PostRight | 1,218 | contract.check & delivery envelope IO at app boundary |
| `RightKitLiveActivity` | 1 | shared | HeardRight, CodeRight | 1,107 | recording live activity views; widgets |
| `rightkit-logs` | 1 | shared | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, CodeRight | 1,030 | API gap first: api-gap, rightkit-tauri-shell 0.6.0 logs feature covers write and collect, but clear is store.clear() on one f; structured logging & support bundles; API gap first: api-gap: the shell logs clear covers the renderer log only (no extra sinks) and the shell has no panic hook wi; API gap first: MOVE rightkit-logs (text-line redactor with stable key-token hashing, bogusyogi/rightkit#520). See docs/rightk; redaction & renderer logs (+5 more) |
| `rightkit-search` | 1 | single | MailRight | 944 | HTML->indexable text, FTS5 maintenance & background rebuild |
| `rightkit-send` | 1 | shared | ScrapeRight, Pulse | 891 | Swift binding/UI over rightkit-send (receive scratchpad, clipboard offer); nearby sharing glue & UI |
| `rightkit-docs` | 1 | single | ViewRight | 695 | document format classification table |
| `RightKitImportQueue` | 1 | single | ScrapeRight | 623 | universal serial job queue & runner; share extension intake |
| `@rightkit/ax` | 1 | shared | GenRight, Pulse | 489 | agent plugin evals; agent plugin launcher |
| `@rightkit/legal` | 1 | shared | ScrapeRight, MailRight, ViewRight, CutRight, CodeRight | 439 | legal notices; legal config; API gap first: api-gap: the shell legal receipt importer is Rust-side only; the renderer localStorage receipt cannot reach it; legal configs & gates; legal acceptance host (+4 more) |
| `RightKitHub` | 1 | single | Pulse | 437 | duplicate copy of RightKit's RightKitHub: delete; API gap first: see docs/rightkit-exceptions/M6.md#b8 |
| `rightkit-http` | 1 | shared | PostRight, ScrapeRight, GenRight, CodeRight | 409 | one-request HTTP layer already on rightkit-http; SSRF URL guard for non-rightkit-http transports; outbound HTTP policy; http |
| `rightkit-embed` | 1 | single | MailRight | 398 | background semantic indexing job |
| `rightkit-credentials` | 1 | shared | MailRight, GenRight, Pulse, HeardRight | 388 | API gap first: ADOPT rightkit-db 0.5.2. See docs/rightkit-exceptions/M9.md#b2-sqlcipher-key-rotation-and-pending-key-open-src; provider API keys; secure prompt; secrets |
| `rightkit-mcp` | 1 | shared | CodeRight, Legion | 332 | MCP client/server; API gap first: outside-M-rows.md H-1 strict percent-decode of untrusted URL text: rightkit-http 0.3.8 percent_decode and perc; stdio MCP transport |
| `@rightkit/outdated` | 1 | single | Pulse | 324 | upstream donor check |
| `rightkit-time` | 1 | shared | PostRight, MailRight | 302 | local wall time -> UTC for calendars (tz-aware); API gap first: MOVE rightkit-time. See docs/rightkit-exceptions/M15.md#b3-wall-clock-utc-stamps-src-taurisrcutc_stamprs: 1) n |
| `@rightkit/tauri` | 1 | shared | PostRight, ScrapeRight, MailRight, GenRight | 293 | thin adapters over existing RK packages; adapters over existing RK packages; API gap first: MOVE @rightkit/theme. See docs/rightkit-exceptions/M2.md#b3-theme-preset-catalogue-srclibthemets-themes-preset; adapters |
| `@rightkit/media-ui` | 1 | shared | ScrapeRight, GenRight | 282 | media player adapters; mask brush & export |
| `RightKitFS` | 1 | single | CodeRight | 203 | API gap first: M11.md I-3 synced-credential policy: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the lat; storage |
| `RightKitLicense` | 1 | shared | ScrapeRight, HeardRight, CodeRight | 183 | licence & Pro continuity config; licence & purchases; account, licence, purchases; API gap first: M12.md B5 iOS ContinuityBridgeClient: in the RightKit working tree but untagged; RightKitSwift 0.8.1 is the la |
| `@rightkit/i18n` | 1 | shared | PostRight, Pulse | 150 | money micro-units & instant formatting; i18n |
| `rightkit-pdf` | 1 | shared | ScrapeRight, ViewRight | 129 | API gap first: api-gap: rightkit-pdf 0.3.3 raster draws image XObjects only (no PDFium renderer, no text layer); see docs/rig; PDF editor adapter & font catalog |
| `rightkit-platform-rules` | 1 | single | PostRight | 107 | pinned platform API versions & sunset dates |
| `RightKitHTTP` | 1 | single | CodeRight | 67 | networking; API gap first: M7.md B3 iOS SSE frame parser and replay: RightKitSwift 0.8.1 has no SSE client. |
| `@rightkit/platform-ui` | 1 | single | ViewRight | 42 | API gap first: Browser-QA platform override seeded into __RIGHTKIT_PLATFORM__; platform-ui offers the override only on its /q; platform shim & test setup |

## Moves to Legion, not RightKit

Skills, doctrine, agent personas, skill manifests/loaders and host projection (Claude Code, Codex, AGENTS.md) are owned by Legion. RightKit is the code kit; Legion is the skills owner.

| From app | Lines | What |
|---|---:|---|
| Legion | 6,549 | Legion's own skill-corpus checks |
| Legion | 5,119 | catalog & inventory generators |
| Legion | 12,837 | host setup, binding & script projection |
| Legion | 2,456 | Legion's own skill/host projection commands |
| Legion | 18,364 | capability catalog & frontmatter, host detection/projection into Claude Code/Codex/AGENTS.md, plugin marketplace install |
| Legion | 6,445 | skill & designer checks |
| Legion | 19,290 | agent skill packs & doctrine (architect, audit, council, designer, iOS/macOS/Android dev, marketing, SEO, research, writing, etc.) |

## Already in RightKit: delete app copies & adopt

| Package | Apps still carrying own copy | Lines to delete |
|---|---|---:|
| `rightkit-mcp` | CodeRight, Legion | 11,995 |
| `rightkit-tauri-shell` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 10,008 |
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
| `RightKitAppleCredentials` | ScrapeRight, HeardRight, CodeRight | 1,236 |
| `RightKitCloudSync` | ScrapeRight | 1,212 |
| `rightkit-app-settings` | PostRight, ScrapeRight, MailRight, GenRight, Pulse, HeardRight, CodeRight | 1,176 |
| `RightKitUpdate` | Pulse, CodeRight | 1,155 |
| `rightkit-credentials` | MailRight, GenRight, Pulse, HeardRight | 1,131 |
| `rightkit-tts` | GenRight | 1,054 |
| `rightkit-fs` | PostRight, ScrapeRight, MailRight, GenRight, CutRight, Pulse | 1,019 |
| `rightkit-file-watch` | ViewRight, Pulse, CodeRight | 993 |
| `rightkit-ocr` | ScrapeRight, ViewRight | 987 |
| `rightkit-http` | PostRight, ScrapeRight, GenRight, CodeRight | 959 |
| `@rightkit/tauri` | PostRight, ScrapeRight, MailRight, GenRight | 839 |
| `rightkit-oauth` | CodeRight | 809 |
| `@rightkit/media-ui` | ScrapeRight, GenRight | 698 |
| `RightKitFS` | CodeRight | 643 |
| `RightKitHTTP` | CodeRight | 577 |
| `rightkit-onnx` | ScrapeRight, HeardRight, CodeRight | 509 |
| `rightkit-pdf` | ScrapeRight, ViewRight | 503 |
| `@rightkit/legal-ui` | ScrapeRight, MailRight | 438 |
| `rightkit-child-process` | ScrapeRight, Pulse | 402 |
| `@rightkit/release` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight, Legion | 367 |
| `rightkit-crash` | ScrapeRight, MailRight, CutRight, Pulse, HeardRight | 351 |
| `rightkit-agent-tools` | GenRight, CodeRight | 302 |
| `@rightkit/settings-ui` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, Pulse, HeardRight | 278 |
| `rightkit-browser` | ScrapeRight, CodeRight | 278 |
| `rightkit-playback` | ScrapeRight, CutRight | 276 |
| `@rightkit/app-update` | ScrapeRight | 218 |
| `rightkit-daemon` | PostRight, MailRight, GenRight, CutRight, Pulse, CodeRight | 192 |
| `@rightkit/license` | ScrapeRight | 192 |
| `rightkit-qa` | PostRight, MailRight, HeardRight, CodeRight | 181 |
| `@rightkit/issues-tracker` | PostRight, MailRight | 176 |
| `RightKitHotkeys` | Pulse | 151 |
| `@rightkit/pdf-ui` | MailRight, ViewRight | 144 |
| `@rightkit/platform-ui` | ViewRight | 118 |
| `@rightkit/theme` | PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight | 112 |
| `rightkit-ffmpeg` | GenRight, CutRight, CodeRight | 93 |
| `rightkit-cache` | Pulse | 89 |
| `RightKitImportQueue` | ScrapeRight | 57 |

## Vendored code: decisions

| Code | App | Lines | Recommendation |
|---|---|---:|---|
| unused docx-editor agents package | ViewRight | 13,663 | DROP: `none` |
| identical to published npm 1.9.0 (core, i18n): use the npm package | ViewRight | 157,859 | VENDOR-KEEP: `@eigenpal/docx-editor-core` |
| use published 1.9.x + small host-toolbar patch carried by @rightkit/docx-editor; upstream the patch as a PR | ViewRight | 51,074 | VENDOR-KEEP: `@eigenpal/docx-editor-react` |
| vendored hyperframes player bits | CutRight | 432 | VENDOR-OWN: `@rightkit/timeline-ui` |
| CSL style parser (MIT): keep as crates.io dep of typeset/citations | CutRight | 6,243 | VENDOR-KEEP: `rightkit-typeset` |
| hand-rolled zstd/json & vendored zstd: use crates/system lib | Pulse | 22,291 | DROP: `none` |
| vendored tree-sitter grammars; use crates.io grammar crates | Pulse | 71,409 | VENDOR-KEEP: `tree-sitter grammars` |
| prebuilt core xcframework: build from RightKit crates | HeardRight | 144 | DROP: `rightkit-asr` |
| ONNX Runtime binary via rightkit-onnx | HeardRight | 43,900 | VENDOR-KEEP: `rightkit-onnx` |
| sherpa-onnx: already rewritten in rightkit-asr (Mac & Windows tested); delete once the iOS build is tested | HeardRight | 9,378 | DROP: `rightkit-asr` |
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

## Decisions (resolved 2026-11-01 by Adrian, except where marked)

1. **Scope.** Every capability goes to RightKit, including ones a single app uses today. That way a new app can find it. "Product-specific" means only how each app looks & composes the shared shell, plus declared per-app exceptions. Example: ViewRight's devtools in the HTML iframe is an app declaration over a `rightkit-tauri-shell` option. Everything is public on crates.io/npm.
2. **DOCX (eigenpal).** Do not fork the 171k lines.
   - `core` and `i18n` are identical to the published npm 1.9.0, so use those packages.
   - `agents` is unused, so drop it.
   - `react` carries ViewRight's host-toolbar bridge. Use the published package plus that small patch, carried by `@rightkit/docx-editor`, and send the patch upstream as a PR (Apache-2.0, active repo). ViewRight's own DOCX integration (`src/documents/docx`) becomes `@rightkit/docx-editor`.
3. **sherpa-onnx.** It is already rewritten in `rightkit-asr`, and Mac & Windows are tested. Delete the iOS xcframework once the iOS build of `rightkit-asr` is tested.
4. **GenRight Node server.** Drop it; the suite runs no Node services.
5. **Tree-sitter grammars, WasmKit, ONNX Runtime, tauri-runtime-wry.** Recommendation: do not own them. Use the upstream crates, SwiftPM package and binary as plain dependencies, and upstream the Tauri patch. Owning them would add maintenance for no product gain, unless one of them needs changes we make.
6. **Publication.** The hold is lifted. Each package is its own unit with its own `Cargo.lock`, and it is published separately: compile locally, qualify on GitHub, then publish automatically. The go-ahead is given. `publish.yml` needs the local broker's signed receipts, so each unit's broker step runs on the Mac or Dell.
7. **Skills.** Legion owns its own skills. App-specific recipes and agent skills stay in their apps (CutRight and GenRight recipes, Pulse's chat skill). CodeRight's `skills`, `plugins` and `recipes` crates are deleted, because CodeRight uses the installed Legion.
8. **Shell.** At most 2% per app. Shell code that every app re-implements becomes shared packages (`rightkit-engine-host`, `rightkit-tauri-shell`, `RightKitUI`, `rightkit-android`, `@rightkit/react-shell`, `rightkit-daemon`).

