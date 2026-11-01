# Design: RightKit as the single home for all suite code

Status: draft for review, 2026-11-01. Author: Claude for Adrian.

## Problem

The Orthic Labs suite has ten app repositories (PostRight, ScrapeRight, MailRight, ViewRight, GenRight, CutRight, Pulse, HeardRight, CodeRight, Legion) and one shared kit, RightKit (`/home/user/rightkit`: 69 Rust crates under `crates/`, 26 npm packages under `packages/`, 33 Swift modules under `swift/RightKitSwift/Sources`, one Android bridge). Today about 4.0 million lines of code live in the apps and most of it is not in RightKit. The same capability is often written more than once (four apps keep their own media/transcript library, three apps run their own agent approval flow, two apps each have a policy engine). New apps cannot be assembled from existing pieces.

## Goal

Every piece of app code becomes a RightKit package (Rust crate, npm package, Swift package or Gradle module), so that a new app is assembled from RightKit pieces. Each app keeps only a thin shell: entry points, command tables, wiring and manifests. Skills, doctrine, agent personas and skill tooling are the one exception: they live in Legion, the suite's skills owner, not in RightKit.

Non-goals: rewriting behaviour, changing product scope, choosing release dates.

## Inputs

- Master plan with the full package list, merges and decisions: [`2026-11-01-rightkit-package-master-plan.md`](../2026-11-01-rightkit-package-master-plan.md).
- Per-package data (consumers, lines, capabilities): [`packages.json`](packages.json).
- Per-app mapping of every code file: `/home/user/<app>/docs/rightkit-inventory/<app>-rightkit-mapping.md` and `<app>-rightkit-files.json` for app in postright, scraperight, mailright, viewright, genright, CutRight, pulse, heardright, coderight, legion.
- Mapping rules and scripts (re-runnable): [`rules/`](rules/), [`tools/`](tools/).

## Method

1. List every tracked code file on each app's `main`.
2. Assign each file, by ordered path rules per app, one action: NEW (new RightKit package), EXTEND (existing RightKit package gains it), ADOPT (RightKit already ships it; delete the app copy), LEGION (skills; goes to Legion), VENDOR-OWN / VENDOR-KEEP (vendored third-party code), DROP, SHELL (stays), CONFIG (stays), NOTCODE.
3. A script checks that every file matched exactly one rule.
4. Packages with overlapping scope across apps were merged (33 merges, listed in the master plan with reasons).

## Result

| Measure | Value |
|---|---:|
| Code lines in apps | 3,996,921 |
| Moving to new RightKit packages | about 2.2M |
| Moving into existing RightKit packages | about 0.8M |
| Already in RightKit, app copy to delete | about 0.3M |
| Moving to Legion (skills) | about 0.06M |
| Staying in apps as shell/config | 171,620 (4%) |
| New packages | 86 (64 crates, 16 npm, 6 Swift) |
| Existing packages extended | 64 |

New packages are grouped into families: agent platform, context & code intelligence, dictation & voice, video editing, generative media, content pipeline & social, ingest & library, documents, mail & workspace, system utilities, foundation & tooling. The master plan lists each package, its consumers, size and contents.

## Key design decisions

1. **One package per capability, named without an app name**, even when only one app uses it today (e.g. `rightkit-timeline`, used only by CutRight).
2. **Tests move with the code they test.**
3. **UI is packaged too.** Screens become npm packages (`@rightkit/agent-ui`, `@rightkit/social-ui`, `@rightkit/video-ui`, ...) and SwiftUI packages; apps compose them.
4. **Engine wire types travel with their package**; the app's command table stays in the app shell.
5. **Vendored code**: own and trim where we change it (DOCX editor), use the plain upstream dependency where we do not (tree-sitter grammars, WasmKit, ONNX Runtime binary).
6. **Swift and Kotlin code** for a capability ship as a target of the same RightKit package family (e.g. `rightkit-mesh` has Rust core, Swift and Android bindings).
7. **Skills live in Legion.** All skill content and skill loaders/projection go to Legion; RightKit has no skills package.

## Risks

- 50+ new packages have a single consumer today; extraction cost may exceed reuse benefit for some.
- Several large packages (`rightkit-code-audit` 256k lines, `rightkit-agent-engine` 129k, `rightkit-agent-server` 117k, `rightkit-context` 99k) may need splitting.
- Publishing cadence: every app change now needs a RightKit release first.
- Rule-based mapping may misplace individual files; per-file JSON allows correction.

## Implementation Plan

Each extraction step follows the same procedure: create the package in RightKit, move code and its tests, publish a version, switch the app to the published pin, delete the app copy and its ownership exception, and pass the app's RightKit gate.

### Wave 0: no new design needed

1. Delete app copies of code RightKit already ships (ADOPT rows, 55 packages, about 0.3M lines). Each app's mapping table lists the rows.
2. Move `rightkit-mesh` (and `rightkit-mesh-ffi`, MeshLink Swift, Android meshlink) from the Pulse repo into RightKit and publish.
3. Create `@rightkit/config` (shared tsconfig, lint, vite, rustfmt, deny, toolchain presets) and `rightkit-doctor`.
4. Resolve vendored code per the decisions table.
5. Move skills into Legion (CutRight `skills/` and `cutaway/`, Pulse `membrane/skills`, CodeRight `crates/skills`, `crates/plugins`, `crates/recipes`).

### Wave 1: packages shared by two or more apps

`rightkit-library` (PostRight, ScrapeRight, GenRight, HeardRight), `rightkit-pipeline` (PostRight, GenRight, CutRight), `rightkit-agent-gateway` + `rightkit-approvals` (MailRight, Pulse, GenRight), `rightkit-app-lock`, `rightkit-web-session`, `rightkit-media-fetch`, `rightkit-research`, `rightkit-bench`, `rightkit-social-analytics`, `@rightkit/library-ui`, `@rightkit/agent-ui`, `@rightkit/dictation-ui`, `@rightkit/waitlist-worker`. Each needs a short design note reconciling the differing app implementations before code moves.

### Wave 2: agent platform

`rightkit-agent-engine`, `rightkit-agent-server`, `rightkit-agent-orchestration`, `rightkit-agent-policy`, `rightkit-evidence`, `rightkit-terminal-ui`, `rightkit-scheduler`, `rightkit-bot-messaging`, `rightkit-github`, `rightkit-chat-bridge`, `rightkit-ai-accounts`, `RightKitAgent`; extensions to `rightkit-workflow`, `rightkit-agents`, `rightkit-llm`, `rightkit-agent-tools`, `rightkit-hooks`. Sources: CodeRight, Legion, Pulse.

### Wave 3: context and code intelligence

`rightkit-context`, `rightkit-memory`, `rightkit-adapt`, `rightkit-repo-graph`, `rightkit-code-audit`. Sources: Pulse (Membrane), CodeRight, Legion.

### Wave 4: single-app domain families

Video (CutRight), generative media (GenRight), content & social (PostRight), documents (ViewRight), mail & workspace (MailRight), dictation (HeardRight), system utilities (Pulse), ingest (ScrapeRight). Order by which app ships next.

### Done when

For each app: its mapping file shows only SHELL, CONFIG and NOTCODE rows; `.rightkit/ownership-exceptions.json` is empty; the app gate passes against published RightKit pins.
