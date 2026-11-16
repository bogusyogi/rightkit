# RightKit audit against the owner's model

Read-only audit. Scope: `/home/user/rightkit` (origin/main; the two unmerged commits were ignored, a clean `git archive origin/main` was used for all counts), 52 npm packages under `packages/`, 228 crates under `crates/`, and the nine apps on origin/main and on their current work branch (`claude/friendly-knuth-w7ixqv` in eight repos; CutRight has only `main`). Swift packages (`swift/`, `Package.swift`) were out of scope. Working files: `(scratch folder of the audit session, not committed) ` (`inventory.json`, `app_usage.json`, `registry.json`, `classes.json`, `product_names_hits.txt`, `product_names_mandatory_shared.txt`, `tools/*.py`).

Registry: crates.io and npm were both reachable; every repo package/crate was looked up (3 npm packages and 3 crates are `unpublished`: `@rightkit/build-control-policy`, `ml-lab`, `sync-worker`, `rightkit-align`, `rightkit-mesh-ffi`, `rightkit-sync-keys`).

Method notes (apply to every number below):
- "Src lines" = lines of `.rs .ts .tsx .css .mjs .js` under the package/crate, excluding `node_modules`, `dist`, `target`, `*.d.ts`, tests/fixtures directories and `*.test.*`. Python (blender-worker, 3,977 lines), Swift and JSON data are not in those counts unless stated.
- App use = a dependency declared in any `package.json` / `Cargo.toml` of the app (dev deps included, vendored/third_party excluded) on `origin/main` and/or on the work branch. `(m)` = default branch only, `(w)` = work branch only. `TPL` = `desktop-app-template`, `E2E` = `e2e-app` (a test harness that lists 93 crates; it is not counted as an app).
- Class rules, applied in this order: owner of an M-row -> `MANDATORY`; content-based reading -> `MIXED` / `APP-LOOK` / `TOOLING`; otherwise by use: 2+ apps (directly, or carried through dependents that 2+ apps use) -> `SHARED-CAPABILITY`; exactly one app -> `APP-CAPABILITY`; no app and no used package/crate -> `UNUSED` (the reason column says which product area it belongs to). `UNUSED` therefore does not mean worthless; it means no consumer exists today.
- Where I judged a split inside a package (CAP / HRL / GEN in section C) the judgement is mine from reading file heads, exports and names; "overlap not checked line by line" is marked where that is the case.

## 0. Headline findings

1. **`@rightkit/dictation-ui` (16,123 lines, published 0.2.7) is HeardRight's whole screen set.** By my per-file split: 7,499 lines are capability that belongs in RightKit (the pill is 2,762 of them), 4,515 are HeardRight look and composition (pages, onboarding flow, sidebar footer, feature reel, Tailwind class tokens) that should live in the app, and 4,075 are generic UI that duplicates or should be part of mandatory packages (react-shell, settings-ui, license, app-update, onboarding). The package's own CHANGELOG says the shell footer exists "so an app can delete its own copy. Markup, classes, wording, native calls and call order equal HeardRight's shipped sidebar footer" (`packages/dictation-ui/CHANGELOG.md:9`).
2. **HeardRight's work branch has already done the move**: `tauri-app-next/src` shrinks from 22,265 lines (default branch, 0 imports of dictation-ui) to 4,872 lines with 70 imports (work branch, 44 commits ahead; e.g. `241d8e9 refactor(shell): app shell runs on @rightkit/dictation-ui/shell 0.2.7`, `49c668e ... setup flow runs on @rightkit/dictation-ui/onboarding`, `6e45c18 Use dictation-ui Dictation, Polish, Home, Vocabulary and Transcribe pages`). Under the owner's model that branch moves HeardRight's look out of the app.
3. **The same pattern exists in other packages**, in different sizes: `@rightkit/video-ui` `./app` (CutRight's whole UI, 10,035 lines; CutRight's own `apps/cutright/src` is now 681 lines), `@rightkit/mail-ui` (MailRight client UI 19,080 lines, of which 4,846 are CSS in `styles/`; MailRight `src` is 4,412 lines), the seven `@rightkit/agent-ui-*` packages (about 103k lines; CodeRight `apps/coderight-tauri/src` drops 96,196 -> 72,327 on the work branch), `@rightkit/document-workspace` `src/app` (17,214 lines of ViewRight shell composition, not yet consumed), `@rightkit/social-ui` `screens/` (7,032 lines, PostRight pages, not yet consumed), `@rightkit/library-ui` `queue/` (7,385 lines of ScrapeRight screens), `@rightkit/system-ui` `views/` (4,270, Pulse), `@rightkit/workspace-ui` `views/` (2,359, MailRight).
4. **Documents that state the opposite model** (conflicts with the owner's model; see section 9): the 2026-11-01 port commission "every capability in every app moves into RightKit; each app keeps only a thin shell (<=2%)", the per-app mapping tables that put HeardRight's pages and onboarding pages into `@rightkit/dictation-ui`, `video-ui/CHANGELOG.md:5` "CutRight's UI belongs in RightKit, not in the app", `docs/plans/2026-11-11-linux-handoff.md:47` "no exceptions, the code moves into RightKit".
5. **Mandatory-set rows are mostly adopted by only two apps in their own declarations.** Only HeardRight, CodeRight (`adopted` map) and GenRight (`rows` map) list adoption; ViewRight, CutRight, MailRight, ScrapeRight declare only not-applicable rows and PostRight lists M1-M20 only in a `$comment` (adoption is visible only through dependencies); Pulse declares only M22. M21, M23, M24, M25 have no selected owner ("never fails"), although `@rightkit/issues-tracker` 0.1.0 is published and 5 apps use it.
6. **`@rightkit/react-shell` + `settings-ui` + `license` + `app-update` do not cover generic things that HeardRight re-implemented** (settings search -> palette bridge, tab-strip settings page, document-level hover tooltip, licence lifecycle dialog, account sign-in and device list, sidebar update line, speech-model update dialog, optimistic settings coordinator). Details in section 5.
7. **Product names inside MANDATORY/SHARED packages**: 2,110 matching lines (1,478 in MANDATORY, 632 in SHARED; 1,157 are code/data and 953 comments). 647 of the MANDATORY ones are in `packages/legal-ui`, almost all in screenshot/axe artefacts under `.preview/` and `preview/`; real code examples: `@rightkit/tauri` exports `createHeardRightTauriClient`, `createCodeRightTauriClient` ... (`packages/tauri/src/index.ts:41-45`), `rightkit-speech-client` has a `HeardRightDictation` enum variant (`crates/rightkit-speech-client/src/capture.rs:27`), `rightkit-models` has an app-name list (`crates/rightkit-models/src/rightapps.rs:17-20`). Section 4.

## Counts per class

npm packages (52):

| Class | Packages | Src lines |
|---|---|---|
| MANDATORY | 16 | 134,008 |
| SHARED-CAPABILITY | 4 | 10,704 |
| APP-CAPABILITY | 5 | 24,635 |
| APP-LOOK | 2 | 9,998 |
| MIXED | 12 | 204,275 |
| TOOLING | 7 | 19,219 |
| UNUSED | 6 | 66,810 |
| **Total** | 52 | 469,649 |

Crates (228):

| Class | Crates | Src lines |
|---|---|---|
| MANDATORY | 17 | 235,043 |
| SHARED-CAPABILITY | 66 | 380,296 |
| APP-CAPABILITY | 68 | 431,391 |
| TOOLING | 6 | 24,770 |
| UNUSED | 71 | 693,429 |
| **Total** | 228 | 1,764,929 |

(`MIXED` appears only on the npm side: 12 packages, 204,275 lines in total, of which the app-look share is estimated per package in section 3. For a crate I found no case where one crate mixes capability and app look; the nearest are `rightkit-launcher-win` and `rightkit-terminal-ui`, classed by use, see section 3.)

## 1. Deliverable A: Mandatory rows

Source of truth: `packages/release/ownership-mandatory-set.json` (rows M1..M23 desktop, pendingRows M21 M23 M24 M25 and M18 for the cli profile, profiles desktop/apple/cli, amendments v2.1 and v2.2). `defaultApps` lists ten apps (the nine audited plus `ember`; Legion is on the `cli` profile). "Repo ver" is the version in the repo manifest; "Registry latest" is crates.io / npm `latest`; "Row min" is the `min` pinned in the set JSON (all `registryMax` values in that JSON are from the 2026-10-19 readback and are stale: e.g. M1 `rightkit-tauri-shell` min 0.4.0 vs registry 0.8.2).

### A1. Owners

| Row | Name | Owner (ecosystem) | Repo ver | Registry latest | Row min in set JSON | Provides (from manifest description) | src lines |
|---|---|---|---|---|---|---|---|
| M1 | Window host | rightkit-tauri-shell (crates) | 0.8.2 | 0.8.2 | 0.4.0 | Shared Tauri desktop shell for Right Suite apps: webview hardening, window state, native menu, tray, single instance, deep links, autostart, rich clip | 32820 |
| M1 | Window host | rightkit-device (crates) | 0.5.1 | 0.5.1 | 0.3.0 | Native host services for Right Suite Tauri license and device adapters. | 515 |
| M1 | Window host | @rightkit/tauri (npm) | 0.4.2 | 0.4.2 | 0.2.3 | Typed host adapters for shared Right Suite Tauri commands. | 872 |
| M2 | App frame | @rightkit/react-shell (npm) | 0.7.4 | 0.7.4 | 0.7.1 | Brand-neutral React frame for Right Suite desktop apps: optional fused sidebar+titlebar layout with slots, command palette, shortcut registry/help, to | 7394 |
| M2 | App frame | @rightkit/platform-ui (npm) | 0.4.1 | 0.4.1 | 0.4.0 | Brand-neutral platform, shortcut, and window-control primitives for Right Suite apps. | 1898 |
| M2 | App frame | @rightkit/theme (npm) | 0.3.4 | 0.3.4 | 0.3.1 | Brand file to CSS tokens generator for Right Suite apps: light/dark/system, derived dark accent, WCAG contrast and hard-coded-colour checks, runtime b | 1165 |
| M3 | Engine supervision | rightkit-job-queue (crates) | 0.6.0 | 0.6.0 | 0.4.0 | Persistent job store, runner, and stdio/CLI command contract for Right Suite engines. | 20140 |
| M3 | Engine supervision | rightkit-child-process (crates) | 0.5.13 | 0.5.13 | 0.5.2 | Ownership-safe child lifecycle, restart, and health primitives for Right Suite apps. | 15555 |
| M4 | Window/CLI protocol | rightkit-job-queue (crates) | 0.6.0 | 0.6.0 | 0.4.0 | Persistent job store, runner, and stdio/CLI command contract for Right Suite engines. | 20140 |
| M4 | Window/CLI protocol | rightkit-sidecar (crates) | 0.3.4 | 0.3.4 | 0.3.0 | Bounded JSON-lines transport primitives for Right Suite sidecars. | 2505 |
| M5 | Other child processes | rightkit-child-process (crates) | 0.5.13 | 0.5.13 | 0.5.2 | Ownership-safe child lifecycle, restart, and health primitives for Right Suite apps. | 15555 |
| M6 | Data location | rightkit-daemon (crates) | 0.9.0 | 0.9.0 | 0.3.0 | One discovery/auth convention for Right Suite local services: service.json, ranking and takeover, current-user-only Unix socket / named pipe, per-app  | 18764 |
| M7 | Local services | rightkit-daemon (crates) | 0.9.0 | 0.9.0 | 0.3.0 | One discovery/auth convention for Right Suite local services: service.json, ranking and takeover, current-user-only Unix socket / named pipe, per-app  | 18764 |
| M8 | Safe file writes | rightkit-fs (crates) | 0.4.16 | 0.4.16 | 0.4.4 | Safe state directories, atomic writes, cross-process locks, owner-only files, and stable file identity for Right Suite apps. | 24026 |
| M9 | Database | rightkit-db (crates) | 0.5.8 | 0.5.8 | 0.5.0 | Correct-by-default SQLite and SQLCipher connection bootstrap for Right Suite apps. | 10135 |
| M10 | Settings | rightkit-app-settings (crates) | 0.4.1 | 0.4.1 | 0.2.4 | Versioned per-app settings persistence: migrations, typed defaults, atomic writes, corruption recovery, change events. | 5104 |
| M10 | Settings | @rightkit/settings-ui (npm) | 0.7.3 | 0.7.3 | 0.7.0 | Settings panel frame, search and standard sections (appearance, updates, license, legal, diagnostics, models, API keys, about) persisted through right | 4578 |
| M11 | Secrets | rightkit-credentials (crates) | 0.5.5 | 0.5.5 | 0.4.2 | Per-app OS credential storage for Right Suite apps: Keychain / Credential Manager / Linux Secret Service / Android Keystore, passive presence checks,  | 9545 |
| M12 | Licence/device | rightkit-license (crates) | 0.3.1 | 0.3.1 | 0.2.3 | Right Suite license enforcement primitive — offline verification of server-signed Ed25519 entitlement tokens. The Rust (security) half of @rightkit/li | 14686 |
| M12 | Licence/device | rightkit-device (crates) | 0.5.1 | 0.5.1 | 0.3.0 | Native host services for Right Suite Tauri license and device adapters. | 515 |
| M12 | Licence/device | @rightkit/license (npm) | 0.3.3 | 0.3.3 | 0.2.1 | Native licence invoke client & safe display views for Right Suite apps. | 1203 |
| M13 | Pro entitlement | rightkit-license (crates) | 0.3.1 | 0.3.1 | 0.2.3 | Right Suite license enforcement primitive — offline verification of server-signed Ed25519 entitlement tokens. The Rust (security) half of @rightkit/li | 14686 |
| M14 | Legal/onboarding | @rightkit/legal (npm) | 0.6.3 | 0.6.3 | 0.6.1 | Legal manifest, assent receipt, and release-contract primitives for proprietary Right Suite desktop apps. | 1267 |
| M14 | Legal/onboarding | @rightkit/legal-ui (npm) | 0.4.2 | 0.4.2 | 0.4.0 | The one first-run / re-acceptance legal gate screen for Right Suite desktop apps. | 1436 |
| M14 | Legal/onboarding | @rightkit/onboarding (npm) | 0.6.5 | 0.6.5 | 0.6.1 | First-run onboarding framework: ordered steps, progress, skip/resume, legal gate first, permissions, model download and licence/trial steps. | 1256 |
| M15 | Logging | rightkit-logs (crates) | 0.4.2 | 0.4.2 | 0.3.3 | Privacy-safe local JSONL logging and diagnostics for Right Suite desktop apps. | 8170 |
| M15 | Logging | @rightkit/logs (npm) | 0.1.6 | 0.1.6 | 0.1.6 | Local-only structured logging and diagnostics client for Right Suite apps. | 808 |
| M16 | Crash reporting | rightkit-crash (crates) | 0.4.1 | 0.4.1 | 0.3.3 | Privacy-first panic capture, redacted crash reports, native dumps and support bundles for Right Suite apps. | 3611 |
| M17 | HTTP | rightkit-http (crates) | 0.4.0 | 0.4.0 | 0.3.6 | Brand-neutral blocking HTTP client for Right Suite apps: timeouts, retry/backoff with Retry-After, SSE streaming, multipart bodies, redacted secrets,  | 9125 |
| M18 | Updates | @rightkit/app-update (npm) | 0.4.4 | 0.4.4 | 0.4.1 | Shared runtime update policy helper for Right Suite Tauri apps. | 2632 |
| M19 | Release tooling | @rightkit/release (npm) | 0.6.57 | 0.6.57 | 0.4.1 | Portable release CLI/SDK with fail-closed public GitHub Releases and private R2 + RightApps distribution. | 98999 |
| M20 | QA | rightkit-qa (crates) | 0.10.8 | 0.10.8 | 0.7.0 | Rust QA harness for Right Suite apps: engine black-box scenarios, hidden native UI driving (the in-app control server is the dev-only `control` featur | 51888 |
| M20 | QA | @rightkit/qa (npm) | 0.7.0 | 0.7.0 | 0.5.1 | Shared QA evidence, ownership, browser helpers, and release-backdoor guards for Right Suite apps. | 7205 |
| M20 | QA | @rightkit/qa-chrome (npm) | 0.1.12 | 0.1.12 | 0.1.8 | Zero-dependency Chrome for Testing resolver, launcher and graceful close for Right Suite QA harnesses. | 1864 |
| M21 (pending) | Test mode | rightkit-fs (crates) | 0.4.16 | 0.4.16 | 0.4.4 | Safe state directories, atomic writes, cross-process locks, owner-only files, and stable file identity for Right Suite apps. | 24026 |
| M21 (pending) | Test mode | rightkit-daemon (crates) | 0.9.0 | 0.9.0 | 0.3.0 | One discovery/auth convention for Right Suite local services: service.json, ranking and takeover, current-user-only Unix socket / named pipe, per-app  | 18764 |
| M21 (pending) | Test mode | rightkit-qa (crates) | 0.10.8 | 0.10.8 | 0.7.0 | Rust QA harness for Right Suite apps: engine black-box scenarios, hidden native UI driving (the in-app control server is the dev-only `control` featur | 51888 |
| M22 | Agent surface | rightkit-agent-tools (crates) | 0.4.3 | 0.4.3 | 0.2.0 | One typed tool interface for agents over rightkit-automation, rightkit-browser and each app's engine commands (via rightkit-daemon), plus the `right-c | 3559 |
| M23 (pending) | Release-time switches | @rightkit/release (npm) | 0.6.57 | 0.6.57 | 0.4.1 | Portable release CLI/SDK with fail-closed public GitHub Releases and private R2 + RightApps distribution. | 98999 |
| M23 (pending) | Release-time switches | rightkit-daemon (crates) | 0.9.0 | 0.9.0 | 0.3.0 | One discovery/auth convention for Right Suite local services: service.json, ranking and takeover, current-user-only Unix socket / named pipe, per-app  | 18764 |
| M24 (pending) | Issue tracker | @rightkit/issues-tracker (npm) | 0.1.0 | 0.1.0 | 0.1.0 | Writes issues.html at a repository root from its GitHub issues: grouped by feature, status chips from labels, offline, light and dark. | 454 |
| M25 (pending) | End-to-end journeys | rightkit-qa (crates) | 0.10.8 | 0.10.8 | 0.7.0 | Rust QA harness for Right Suite apps: engine black-box scenarios, hidden native UI driving (the in-app control server is the dev-only `control` featur | 51888 |
| M25 (pending) | End-to-end journeys | @rightkit/qa (npm) | 0.7.0 | 0.7.0 | 0.5.1 | Shared QA evidence, ownership, browser helpers, and release-backdoor guards for Right Suite apps. | 7205 |

Pending rows have no enforced owner today: M21 Test mode, M23 Release-time switches, M24 Issue tracker, M25 End-to-end journeys (their `pendingOwners` are shown above). Cli profile (Legion): M5 M8 M9 M10 M11 M15 M16 M17 M19 M20 owed, M18 pending with no owner. Amendments: v2.1 adds `@rightkit/tauri-shell` 0.1.1 to M1 (draft, opt-in; used by 7 apps + template); v2.2 adds `rightkit-roots` 0.2.0 to M6 (draft; `rightkit-qa` already hard-depends on it; HeardRight/Pulse/CodeRight work branches use it).

### A2. What each app declares in `rightkit-mandatory.json` (default branch; `>` = differs on work branch)

`adopted` = listed in `adopted` or `rows.<id>.status`; `N/A` = `notApplicable`; `pending` = `pending`; `role:` = M22 applicability record; `-` = nothing declared (adoption can still be seen from dependencies in A3).

| Row | HR | VR | PU | CR | GR | MR | SR | CU | PR |
|---|---|---|---|---|---|---|---|---|---|
| M1 Window host | adopted | - | - | adopted | adopted | - | - | - | - |
| M2 App frame | adopted | - | - | adopted | adopted | - | - | - | - |
| M3 Engine supervision | adopted | N/A | - | adopted | adopted | - | - | - | - |
| M4 Window/CLI protocol | adopted | N/A | - | adopted | adopted | - | - | - | - |
| M5 Other child processes | adopted | - | - | adopted | adopted | - | - | - | - |
| M6 Data location | adopted | - | - | adopted | adopted | - | - | - | - |
| M7 Local services | adopted | N/A | - | adopted | adopted | - | - | - | - |
| M8 Safe file writes | adopted | - | - | adopted | adopted | - | - | - | - |
| M9 Database | adopted | - | - | adopted | adopted | - | - | N/A | - |
| M10 Settings | adopted | - | - | adopted | adopted | - | - | - | - |
| M11 Secrets | adopted | - | - | adopted | adopted | - | - | - | - |
| M12 Licence/device | adopted | - | - | adopted | adopted | - | - | - | - |
| M13 Pro entitlement | adopted | - | - | adopted | adopted | - | - | - | - |
| M14 Legal/onboarding | adopted | - | - | adopted | adopted | - | - | - | - |
| M15 Logging | adopted | - | - | adopted | adopted | - | - | - | - |
| M16 Crash reporting | adopted | - | - | adopted | adopted | - | - | - | - |
| M17 HTTP | adopted | N/A | - | adopted | adopted | - | - | N/A | - |
| M18 Updates | adopted | - | - | adopted | adopted | - | - | - | - |
| M19 Release tooling | adopted | - | - | adopted | adopted | - | - | - | - |
| M20 QA | adopted | - | - | adopted | adopted | - | - | - | - |
| M21 Test mode | pending | - | - | pending | - | - | - | - | - |
| M22 Agent surface | N/A | N/A | role:host | N/A | adopted | N/A | N/A | role:host | N/A |
| M23 Release-time switches | pending | - | - | pending | - | - | - | - | - |
| M24 Issue tracker | pending | - | - | pending | - | - | - | - | - |
| M25 End-to-end journeys | pending | - | - | pending | - | - | - | - | - |

Declaration formats differ per app: HeardRight and CodeRight (`adopted` map of strings / `owners[]` objects with versions), GenRight (`rows` map with `status` and `owners`), PostRight (a `$comment` stating M1-M20 adopted plus `notApplicable.M22`), the rest have no adoption map. Not-applicable rows and their stated reasons: ViewRight M3 M4 M7 M17 M22 (expire 2026-11-30), CutRight M9 M17, HeardRight/CodeRight/MailRight/ScrapeRight/PostRight M22. HeardRight, CodeRight: version strings in the JSON differ between the branches (e.g. HeardRight M1 `rightkit-tauri-shell =0.6.0` on main, `=0.8.0` on work). On main the JSON strings are also stale against the manifests: HeardRight's manifest pins `rightkit-tauri-shell` 0.7.0 (A3) while its `adopted.M1` says 0.6.0.

### A3. Owner versions per app, default branch > work branch

Each line shows `owner main>work` taken from the app manifests (`-` = not declared on that branch; only owners that the app depends on are listed). A row with "no owner dependency" for an app means the app is not on the owner at all.

**M1 Window host**
- HR: tauri-shell 0.7.0>0.8.0; device 0.4.2>0.5.0; @tauri 0.3.0>0.4.2
- VR: tauri-shell 0.7.0; device 0.4.0; @tauri 0.3.0
- PU: tauri-shell 0.7.0>0.8.0; device 0.4.0>0.5.0; @tauri 0.3.0>0.4.2
- CR: tauri-shell 0.7.0>0.8.0; device 0.4.0/ws>0.5.0/ws; @tauri 0.3.0>0.4.2
- GR: tauri-shell 0.7.0>0.8.0; device 0.4.0>0.5.0; @tauri 0.3.0>0.4.2
- MR: tauri-shell 0.7.0; device 0.4.0; @tauri 0.3.0
- SR: tauri-shell 0.7.0; device 0.4.0; @tauri 0.3.0
- CU: tauri-shell 0.7.0; device 0.4.0; @tauri 0.3.0
- PR: tauri-shell 0.7.0; device 0.4.0; @tauri 0.3.0

**M2 App frame**
- HR: @react-shell 0.7.2>0.7.4; @platform-ui 0.4.0>0.4.1; @theme 0.3.1>0.3.3
- VR: @react-shell 0.7.2; @platform-ui 0.4.0; @theme 0.3.1
- PU: @react-shell 0.7.2>0.7.4; @platform-ui 0.4.0>0.4.1; @theme 0.3.1>0.3.3
- CR: @react-shell 0.7.2>0.7.4; @platform-ui 0.4.0>0.4.1; @theme 0.3.1>0.3.4
- GR: @react-shell 0.7.2>0.7.4; @platform-ui 0.4.0>0.4.1; @theme 0.3.1>0.3.4
- MR: @react-shell 0.7.2; @platform-ui 0.4.0; @theme 0.3.1
- SR: @react-shell 0.7.2; @platform-ui 0.4.0; @theme 0.3.1
- CU: @react-shell 0.7.2; @platform-ui 0.4.0; @theme 0.3.1
- PR: @react-shell 0.7.2; @platform-ui 0.4.0; @theme 0.3.1

**M3 Engine supervision**
- HR: job-queue 0.5.1>0.6.0; child-process 0.5.7>0.5.12
- VR: child-process 0.5.7
- PU: job-queue 0.5.1>0.6.0; child-process 0.5.7>0.5.12
- CR: job-queue 0.5.1/ws>0.6.0/ws; child-process 0.5.7/0.5.9/ws>0.5.12/ws
- GR: job-queue 0.5.1>0.6.0; child-process 0.5.7>0.5.12
- MR: job-queue 0.5.1; child-process 0.5.7
- SR: job-queue 0.5.1; child-process 0.5.7
- CU: job-queue 0.5.1; child-process 0.5.7
- PR: job-queue 0.5.1; child-process 0.5.7

**M4 Window/CLI protocol**
- HR: job-queue 0.5.1>0.6.0; sidecar 0.3.0>0.3.4
- VR: no owner dependency in any manifest
- PU: job-queue 0.5.1>0.6.0; sidecar 0.3.0>0.3.4
- CR: job-queue 0.5.1/ws>0.6.0/ws; sidecar 0.3.0/ws>0.3.4/ws
- GR: job-queue 0.5.1>0.6.0; sidecar 0.3.0>0.3.4
- MR: job-queue 0.5.1; sidecar 0.3.0
- SR: job-queue 0.5.1; sidecar 0.3.0
- CU: job-queue 0.5.1; sidecar 0.3.0
- PR: job-queue 0.5.1; sidecar 0.3.0

**M5 Other child processes**
- HR: child-process 0.5.7>0.5.12
- VR: child-process 0.5.7
- PU: child-process 0.5.7>0.5.12
- CR: child-process 0.5.7/0.5.9/ws>0.5.12/ws
- GR: child-process 0.5.7>0.5.12
- MR: child-process 0.5.7
- SR: child-process 0.5.7
- CU: child-process 0.5.7
- PR: child-process 0.5.7

**M6 Data location**
- HR: daemon 0.5.1>0.9.0
- VR: daemon 0.5.1
- PU: daemon 0.4.0/0.5.1>0.9.0
- CR: daemon 0.5.3/ws>0.9.0/ws
- GR: daemon 0.5.1>0.9.0
- MR: daemon 0.5.1
- SR: daemon 0.5.1
- CU: daemon 0.5.1
- PR: daemon 0.5.1

**M7 Local services**
- HR: daemon 0.5.1>0.9.0
- VR: daemon 0.5.1
- PU: daemon 0.4.0/0.5.1>0.9.0
- CR: daemon 0.5.3/ws>0.9.0/ws
- GR: daemon 0.5.1>0.9.0
- MR: daemon 0.5.1
- SR: daemon 0.5.1
- CU: daemon 0.5.1
- PR: daemon 0.5.1

**M8 Safe file writes**
- HR: fs 0.4.12>0.4.15
- VR: fs 0.4.9
- PU: fs 0.4.12/0.4.9>0.4.15
- CR: fs 0.4.9/ws>0.4.15/ws
- GR: fs 0.4.9>0.4.15
- MR: fs 0.4.9
- SR: fs 0.4.9
- CU: fs 0.4.9
- PR: fs 0.4.9

**M9 Database**
- HR: db 0.5.5>0.5.8
- VR: db 0.5.3
- PU: db 0.5.3/0.5.5>0.5.8
- CR: db 0.5.3/ws>0.5.8/ws
- GR: db 0.5.3>-
- MR: db 0.5.3
- SR: db 0.5.3
- CU: no owner dependency in any manifest
- PR: db 0.5.3

**M10 Settings**
- HR: app-settings 0.3.1>0.4.1; @settings-ui 0.7.0>0.7.3
- VR: @settings-ui 0.7.0
- PU: app-settings 0.3.1>0.4.1; @settings-ui 0.7.0>0.7.2
- CR: app-settings 0.3.1/ws>0.4.1/ws; @settings-ui 0.7.0>0.7.3
- GR: app-settings 0.3.1>0.4.1; @settings-ui 0.7.0>0.7.2
- MR: app-settings 0.3.1; @settings-ui 0.7.0
- SR: app-settings 0.3.1; @settings-ui 0.7.0
- CU: app-settings 0.3.1; @settings-ui 0.7.0
- PR: app-settings 0.3.1; @settings-ui 0.7.0

**M11 Secrets**
- HR: credentials 0.5.0>0.5.4
- VR: credentials 0.4.3
- PU: credentials 0.4.6>0.5.5
- CR: credentials 0.4.3/ws>0.5.4/ws
- GR: credentials 0.4.3>0.5.4
- MR: credentials 0.4.3
- SR: credentials 0.4.3
- CU: credentials 0.4.3
- PR: credentials 0.4.3

**M12 Licence/device**
- HR: license 0.2.8>0.3.1; device 0.4.2>0.5.0; @license 0.3.0>0.3.3
- VR: license 0.2.6; device 0.4.0; @license 0.3.0
- PU: license 0.2.6>0.3.1; device 0.4.0>0.5.0; @license 0.3.0>0.3.2
- CR: license 0.2.6/ws>0.3.1/ws; device 0.4.0/ws>0.5.0/ws; @license 0.3.0>0.3.3
- GR: license 0.2.6>0.3.1; device 0.4.0>0.5.0; @license 0.3.0>0.3.2
- MR: license 0.2.6; device 0.4.0; @license 0.3.0
- SR: license 0.2.6; device 0.4.0; @license 0.3.0
- CU: license 0.2.6; device 0.4.0; @license 0.3.0
- PR: license 0.2.6; device 0.4.0; @license 0.3.0

**M13 Pro entitlement**
- HR: license 0.2.8>0.3.1
- VR: license 0.2.6
- PU: license 0.2.6>0.3.1
- CR: license 0.2.6/ws>0.3.1/ws
- GR: license 0.2.6>0.3.1
- MR: license 0.2.6
- SR: license 0.2.6
- CU: license 0.2.6
- PR: license 0.2.6

**M14 Legal/onboarding**
- HR: @legal 0.6.3; @legal-ui 0.4.1>0.4.2; @onboarding 0.6.3>0.6.5
- VR: @legal 0.6.3; @legal-ui 0.4.1; @onboarding 0.6.3
- PU: @legal 0.6.3; @legal-ui 0.4.1>0.4.2; @onboarding 0.6.3>0.6.5
- CR: @legal 0.6.3; @legal-ui 0.4.1>0.4.2; @onboarding 0.6.3>0.6.5
- GR: @legal 0.6.3; @legal-ui 0.4.1>0.4.2; @onboarding 0.6.3>0.6.5
- MR: @legal 0.6.3; @legal-ui 0.4.1; @onboarding 0.6.3
- SR: @legal 0.6.3; @legal-ui 0.4.1; @onboarding 0.6.3
- CU: @legal 0.6.3; @legal-ui 0.4.1; @onboarding 0.6.3
- PR: @legal 0.6.3; @legal-ui 0.4.1; @onboarding 0.6.3

**M15 Logging**
- HR: logs 0.3.7>0.4.1; @logs 0.1.6
- VR: logs 0.3.7; @logs 0.1.6
- PU: logs 0.3.7>0.4.1; @logs 0.1.6
- CR: logs 0.3.7/ws>0.4.1/ws; @logs 0.1.6
- GR: logs 0.3.7>0.4.1; @logs 0.1.6
- MR: logs 0.3.7; @logs 0.1.6
- SR: logs 0.3.7; @logs 0.1.6
- CU: logs 0.3.7; @logs 0.1.6
- PR: logs 0.3.7; @logs 0.1.6

**M16 Crash reporting**
- HR: crash 0.3.6>0.4.1
- VR: crash 0.3.6
- PU: crash 0.3.6>0.4.1
- CR: crash 0.3.6/ws>0.4.1/ws
- GR: crash 0.3.6>0.4.1
- MR: crash 0.3.6
- SR: crash 0.3.6
- CU: crash 0.3.6
- PR: crash 0.3.6

**M17 HTTP**
- HR: http 0.3.9>0.4.0
- VR: no owner dependency in any manifest
- PU: http 0.3.9>0.4.0
- CR: http 0.3.9/ws>0.4.0/ws
- GR: http 0.3.9>0.4.0
- MR: http 0.3.9
- SR: http 0.3.9
- CU: no owner dependency in any manifest
- PR: http 0.3.9

**M18 Updates**
- HR: @app-update 0.4.2>0.4.4
- VR: @app-update 0.4.2
- PU: @app-update 0.4.2>0.4.4
- CR: @app-update 0.4.2>0.4.4
- GR: @app-update 0.4.2>0.4.4
- MR: @app-update 0.4.2
- SR: @app-update 0.4.2
- CU: @app-update 0.4.2
- PR: @app-update 0.4.2

**M19 Release tooling**
- HR: @release 0.6.46>0.6.57
- VR: @release 0.6.29
- PU: @release 0.6.47>0.6.57
- CR: @release 0.6.29>0.6.57
- GR: @release 0.6.40>0.6.57
- MR: @release 0.6.40
- SR: @release 0.6.29
- CU: @release 0.6.40
- PR: @release 0.6.29

**M20 QA**
- HR: qa 0.10.4; @qa 0.6.1>0.7.0; @qa-chrome 0.1.12
- VR: qa 0.10.0; @qa 0.6.1; @qa-chrome 0.1.12
- PU: qa 0.10.3>0.10.6; @qa 0.6.1>0.7.0; @qa-chrome 0.1.12
- CR: qa 0.10.4>0.10.6; @qa 0.6.1>0.7.0; @qa-chrome 0.1.12
- GR: qa 0.10.0>0.10.6; @qa 0.6.1>0.7.0; @qa-chrome 0.1.12
- MR: qa 0.10.0; @qa 0.6.1; @qa-chrome 0.1.12
- SR: qa 0.10.0; @qa 0.6.1; @qa-chrome 0.1.12
- CU: qa 0.10.0; @qa 0.6.1; @qa-chrome 0.1.12
- PR: qa 0.10.0; @qa 0.6.1; @qa-chrome 0.1.12

**M21 Test mode**
- HR: fs 0.4.12>0.4.15; daemon 0.5.1>0.9.0; qa 0.10.4
- VR: fs 0.4.9; daemon 0.5.1; qa 0.10.0
- PU: fs 0.4.12/0.4.9>0.4.15; daemon 0.4.0/0.5.1>0.9.0; qa 0.10.3>0.10.6
- CR: fs 0.4.9/ws>0.4.15/ws; daemon 0.5.3/ws>0.9.0/ws; qa 0.10.4>0.10.6
- GR: fs 0.4.9>0.4.15; daemon 0.5.1>0.9.0; qa 0.10.0>0.10.6
- MR: fs 0.4.9; daemon 0.5.1; qa 0.10.0
- SR: fs 0.4.9; daemon 0.5.1; qa 0.10.0
- CU: fs 0.4.9; daemon 0.5.1; qa 0.10.0
- PR: fs 0.4.9; daemon 0.5.1; qa 0.10.0

**M22 Agent surface**
- HR: no owner dependency in any manifest
- VR: no owner dependency in any manifest
- PU: agent-tools 0.3.0>0.3.4
- CR: no owner dependency in any manifest
- GR: agent-tools 0.3.0>0.4.3
- MR: no owner dependency in any manifest
- SR: no owner dependency in any manifest
- CU: agent-tools 0.3.0
- PR: no owner dependency in any manifest

**M23 Release-time switches**
- HR: @release 0.6.46>0.6.57; daemon 0.5.1>0.9.0
- VR: @release 0.6.29; daemon 0.5.1
- PU: @release 0.6.47>0.6.57; daemon 0.4.0/0.5.1>0.9.0
- CR: @release 0.6.29>0.6.57; daemon 0.5.3/ws>0.9.0/ws
- GR: @release 0.6.40>0.6.57; daemon 0.5.1>0.9.0
- MR: @release 0.6.40; daemon 0.5.1
- SR: @release 0.6.29; daemon 0.5.1
- CU: @release 0.6.40; daemon 0.5.1
- PR: @release 0.6.29; daemon 0.5.1

**M24 Issue tracker**
- HR: @issues-tracker 0.1.0
- VR: no owner dependency in any manifest
- PU: @issues-tracker 0.1.0
- CR: no owner dependency in any manifest
- GR: no owner dependency in any manifest
- MR: @issues-tracker 0.1.0
- SR: @issues-tracker 0.1.0
- CU: no owner dependency in any manifest
- PR: @issues-tracker 0.1.0

**M25 End-to-end journeys**
- HR: qa 0.10.4; @qa 0.6.1>0.7.0
- VR: qa 0.10.0; @qa 0.6.1
- PU: qa 0.10.3>0.10.6; @qa 0.6.1>0.7.0
- CR: qa 0.10.4>0.10.6; @qa 0.6.1>0.7.0
- GR: qa 0.10.0>0.10.6; @qa 0.6.1>0.7.0
- MR: qa 0.10.0; @qa 0.6.1
- SR: qa 0.10.0; @qa 0.6.1
- CU: qa 0.10.0; @qa 0.6.1
- PR: qa 0.10.0; @qa 0.6.1


### A4. Rows whose owner looks incomplete for the job

| Row | Evidence | Missing |
|---|---|---|
| M1 Window host | `crates/rightkit-tauri-shell/Cargo.toml` hard-depends on `rightkit-autostart rightkit-clipboard rightkit-deep-link rightkit-instance rightkit-presence rightkit-notifications rightkit-signal` and optionally on `overlay pdf models app-update`; none of those is an M1 owner in the set JSON. `@rightkit/tauri` still exports per-app clients (`packages/tauri/src/deprecated-app-clients.ts`, 11 product-name lines). | Owner list under-states the base: 7 crates in every app's window host are unlisted. |
| M2 App frame | `react-shell` exports `AppShell` (nav-group sidebar, `statusFooter`, `titlebarStart/End`, `planeBar`, `searchMode`) and lower-level `AppFrame`/`Plane`. ViewRight composes `AppFrame`+`Plane` and keeps its own 2,291 lines of shell (`src/app/AppShell,Sidebar,CommandPalette,ShortcutHelp,ToastViewport,Toolbar,...`) and 4,128 lines of sidebar/tabs/shell; CodeRight keeps its own `Sidebar`, `TitleBar`, `CommandPalette`, `AppFrame` (1,871 lines); HeardRight wraps `AppShell` and keeps `HoverTooltip`, `SettingsTabs`, settings registry, palette hooks (section 5). | The frame expresses a nav-group sidebar app; a document/tab/explorer app (ViewRight) or an IDE-like app (CodeRight) cannot express its frame through `AppShell`, so each wrapped `AppFrame` or re-implemented. Note the owner's model allows each app its own composition; what is missing are the generic parts listed in section 5. |
| M6 Data location | `rightkit-roots` is the real owner on work branches (HR `rightkit-roots =0.2.0`), yet it is only amendment v2.2 (draft). | Row owner list is behind practice; `rightkit-qa` (M20) already depends on `rightkit-roots`. |
| M10 Settings | `@rightkit/settings-ui` has panel + sections (appearance, updates, licence, legal, diagnostics, doctor, models, api keys, about) and search; no account sign-in/out/delete section and no leased-device list (grep of `sections.tsx`: only a "Deactivate this device" row, line 311/514). HeardRight kept both (`dictation-ui/src/settings/AccountSettings.tsx` 381, `DeviceManagementSettings.tsx` 296). | Account and device-management sections. |
| M12 Licence | `@rightkit/license` has the lifecycle state machine (`lifecycle.ts`: `dialogForLicenseLifecycle`, monitor) but no dialog component; HeardRight/dictation-ui carry `LicenseLifecycleDialog` (70 lines). | The lifecycle dialog (and the account/device UI above). |
| M18 Updates | Only the npm package is an owner; the native half `rightkit-app-update` (658 lines) is pulled by `rightkit-tauri-shell` but unlisted. `@rightkit/app-update/react` has `UpdateOverlay`, `UpdateConsentCard`, `BootProbationReporter`; no sidebar/status line. HeardRight kept `SidebarUpdateStatus` (159) and `autoUpdates.ts` (246 on main). | Update status line component; crate not listed. |
| M22 Agent surface | `rightkit-agent-tools` is "a client toolbox with no host-side API to adopt" (MailRight `rightkit-mandatory.json` M22 reason). Hosts (MailRight, GenRight, Pulse, CutRight) built their own service on `rightkit-daemon` and declared host role or N/A. 6 apps declare N/A. | Host-side half of the agent surface. |
| M21 M23 M24 M25 | "No selected owner"/"not yet published"/"not built" in the JSON; `@rightkit/issues-tracker` 0.1.0 is on the registry and used by HR, PU, MR, SR, PR (`right-issues`). `rightkit-qa` 0.10.8 / `@rightkit/qa` 0.7.0 are on the registry; journeys are built (`desktop-app-template/journeys.toml` exists) but the row still says not built. | Rows are behind the code; the set JSON is stale. |
| M3/M4 | Engine supervision is `rightkit-job-queue` (feature `supervisor`) + `rightkit-child-process`; HeardRight's supervisor lives in `rightkit-dictation-app-runtime/engine_supervisor` (1,916 lines, over `job-queue` supervisor), ScrapeRight still has its own `engine_supervisor.rs` (`docs/plans/2026-10-20-rightkit-implementation/06-scraperight.md:55`). | Not inspected in depth. |

## 2. Deliverable B: classification of every package and crate

Columns: Repo / Registry = version in the repo / npm or crates.io latest; "Apps" = direct use as defined above; "carried" = apps that reach it through a dependent; "RK dependents" = number of other RightKit packages/crates that depend on it (non-dev).

### B1. npm packages (52)

| Class | Name | Repo | Registry | Src lines | CSS lines | Apps (direct; m=main only, w=work only) | Apps carried (via dependents) | RK dependents (non-dev) | Reason |
|---|---|---|---|---|---|---|---|---|---|
| MANDATORY | @rightkit/release | 0.6.57 | 0.6.57 | 98999 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | M19 owner (release CLI; also M23 pending) |
| MANDATORY | @rightkit/react-shell | 0.7.4 | 0.7.4 | 7394 | 1223 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 21 | M2 owner (app frame, AppShell/AppFrame, palette, ui kit) |
| MANDATORY | @rightkit/qa | 0.7.0 | 0.7.0 | 7205 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 0 | M20 owner (UI QA, allowDev) |
| MANDATORY | @rightkit/settings-ui | 0.7.3 | 0.7.3 | 4578 | 107 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 11 | M10 owner (settings panel + standard sections) |
| MANDATORY | @rightkit/app-update | 0.4.4 | 0.4.4 | 2632 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 8 | M18 owner (update policy/UI) |
| MANDATORY | @rightkit/platform-ui | 0.4.1 | 0.4.1 | 1898 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 17 | M2 owner (platform primitives) |
| MANDATORY | @rightkit/qa-chrome | 0.1.12 | 0.1.12 | 1864 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | M20 owner (QA chrome) |
| MANDATORY | @rightkit/legal-ui | 0.4.2 | 0.4.2 | 1436 | 35 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 6 | M14 owner (legal gate screen); product names only in .preview artifacts |
| MANDATORY | @rightkit/legal | 0.6.3 | 0.6.3 | 1267 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 4 | M14 owner (legal manifest/receipts) |
| MANDATORY | @rightkit/onboarding | 0.6.5 | 0.6.5 | 1256 | 62 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 6 | M14 owner (onboarding framework) |
| MANDATORY | @rightkit/license | 0.3.3 | 0.3.3 | 1203 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 5 | M12 owner (native licence client) |
| MANDATORY | @rightkit/theme | 0.3.4 | 0.3.4 | 1165 | 8 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 6 | M2 owner (brand tokens) |
| MANDATORY | @rightkit/tauri-shell | 0.1.4 | 0.1.4 | 977 | 0 | VR,PU,CR,GR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,PR,PU,SR,VR | 6 | M1 amendment v2.1 owner (draft, opt-in); used by 7 apps + template |
| MANDATORY | @rightkit/tauri | 0.4.2 | 0.4.2 | 872 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 2 | M1 owner (renderer bridge) |
| MANDATORY | @rightkit/logs | 0.1.6 | 0.1.6 | 808 | 0 | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 7 | M15 owner (web log view) |
| MANDATORY | @rightkit/issues-tracker | 0.1.0 | 0.1.0 | 454 | 0 | HR,PU,MR,SR,PR,E2E | HR,MR,PR,PU,SR | 0 | M24 pending owner (published 0.1.0; used by 5 apps) |
| SHARED-CAPABILITY | @rightkit/media-ui | 0.3.6 | 0.3.6 | 5713 | 427 | VR,GR,SR,CU,E2E | CU,GR,HR,SR,VR | 4 | media player / image editor; used by VR, GR, SR, CU |
| SHARED-CAPABILITY | @rightkit/doc-render | 0.2.2 | 0.2.2 | 2669 | 65 | VR,CR,SR,E2E | CR,HR,SR,VR | 4 | markdown/HTML renderer; used by VR, CR, SR and 4 packages |
| SHARED-CAPABILITY | @rightkit/pdf-ui | 0.3.1 | 0.3.1 | 2085 | 82 | VR,E2E | MR,VR | 3 | PDF viewer; VR + document-core + mail-ui |
| SHARED-CAPABILITY | @rightkit/i18n | 0.2.1 | 0.2.1 | 237 | 0 | PU,CR,E2E | CR,PU | 0 | i18n React runtime; PU, CR |
| APP-CAPABILITY | @rightkit/agent-ui-workbench | 0.1.1 | 0.1.1 | 12984 | 0 | CR(w) | CR | 1 | coding-agent workbench/runtime panels (13k); CodeRight work branch only |
| APP-CAPABILITY | @rightkit/agent-ui-hooks | 0.1.1 | 0.1.1 | 10368 | 0 | CR(w) | CR | 3 | coding-agent React hooks (10.4k); CodeRight work branch only |
| APP-CAPABILITY | @rightkit/terminal-ui | 0.1.0 | 0.1.0 | 1045 | 0 | - | CR | 1 | terminal pane + hook (1,045 lines); only agent-ui-components (CodeRight) |
| APP-CAPABILITY | @rightkit/waitlist-worker | 0.1.0 | 0.1.0 | 234 | 0 | VR | VR | 0 | Cloudflare waitlist worker; ViewRight only |
| APP-CAPABILITY | @rightkit/blender-worker | 0.1.1 | 0.1.1 | 4 | 0 | GR | GR | 0 | Blender render worker (3,977 lines Python + evidence); GenRight, CutRight evidence |
| APP-LOOK | @rightkit/agent-ui-settings | 0.1.1 | 0.1.1 | 6837 | 0 | CR(w) | CR | 1 | CodeRight settings panes (6.8k); duplicates settings-ui composition |
| APP-LOOK | @rightkit/agent-ui-web | 0.1.0 | 0.1.0 | 3161 | 865 | - | - | 0 | CodeRight browser client: App.tsx, App.css, service worker "coderight-shell-v1"; no consumer |
| MIXED | @rightkit/document-workspace | 0.2.1 | 0.2.1 | 37281 | 2209 | - | - | 0 | documents/ = viewers (capability); app/ = ViewRight AppShell, Sidebar, tabs, palette, toolbar |
| MIXED | @rightkit/agent-ui-components | 0.1.2 | 0.1.2 | 31361 | 2 | CR(w) | CR | 0 | chat/composer/review panels (capability) + components/app (CodeRight frame, TitleBar, Sidebar) + settings |
| MIXED | @rightkit/agent-ui-core | 0.1.4 | 0.1.4 | 28917 | 626 | CR(w) | CR | 5 | daemon client/stores (capability) + CodeRight index.css/themes/app copy |
| MIXED | @rightkit/mail-ui | 0.2.1 | 0.2.1 | 19080 | 4852 | MR | MR | 1 | MailRight client UI: logic/store/ipc vs views + 17 CSS files |
| MIXED | @rightkit/video-ui | 0.2.1 | 0.2.1 | 17841 | 1572 | CU | CU | 0 | root = editor components (capability); ./app = CutRight whole UI (10k) |
| MIXED | @rightkit/social-ui | 0.2.1 | 0.2.1 | 17418 | 0 | - | - | 0 | data/engine+types = PostRight client; screens/ = PostRight pages; data/mock = fixtures |
| MIXED | @rightkit/dictation-ui | 0.2.7 | 0.2.7 | 16064 | 149 | HR(w) | HR | 0 | pill = capability; pages/onboarding/settings/shell = HeardRight look+composition (see C) |
| MIXED | @rightkit/library-ui | 0.2.3 | 0.2.3 | 11604 | 3311 | HR(w) | HR | 1 | list+reader = shared capability (HR history, SR detail); queue/ = ScrapeRight queue screens |
| MIXED | @rightkit/agent-ui-approvals | 0.2.1 | 0.2.1 | 9791 | 203 | MR | MR | 0 | approval queue (capability) + Pulse remote/secure-box settings (8.7k) coupled to mail-ui and system-ui |
| MIXED | @rightkit/document-core | 0.2.0 | 0.2.0 | 5974 | 2 | - | - | 4 | documents+state = capability; app/ + settings/ = ViewRight composition |
| MIXED | @rightkit/system-ui | 0.2.1 | 0.2.1 | 5810 | 485 | - | MR | 1 | Pulse system views (4.3k) + typed client/chart helpers |
| MIXED | @rightkit/workspace-ui | 0.2.1 | 0.2.1 | 3134 | 0 | MR | MR | 1 | MailRight workspace views (calendar/contacts/drive/tasks/meeting) + client |
| TOOLING | @rightkit/ci | 0.3.2 | 0.3.2 | 4801 | 0 | E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | CI helper scripts |
| TOOLING | @rightkit/outdated | 0.3.1 | 0.3.1 | 4744 | 0 | E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | dependency freshness tool used by release |
| TOOLING | @rightkit/build-control-policy | 0.1.5-phase1 | unpublished | 4655 | 0 | E2E | - | 0 | release/build policy, unpublished 0.1.5-phase1; lists 7 product names as data |
| TOOLING | @rightkit/ax | 0.3.2 | 0.3.2 | 4097 | 0 | E2E | - | 0 | agent-experience conformance/gates (no app dependency) |
| TOOLING | @rightkit/ml-lab | 0.2.0 | unpublished | 689 | 0 | - | - | 0 | private ASR model tooling, not an app dependency (README) |
| TOOLING | @rightkit/config | 0.1.1 | 0.1.1 | 131 | 0 | CU | CU | 0 | Vite/ESLint shared config (CutRight) |
| TOOLING | @rightkit/hooks | 0.1.2 | 0.1.2 | 102 | 0 | E2E | - | 0 | agent hook dispatcher scripts (102 lines) |
| UNUSED | @rightkit/markdown-editor | 0.2.0 | 0.2.0 | 27075 | 418 | - | - | 1 | ViewRight markdown/mdx editor (27k); only dependent is document-workspace, itself unused; nature: ViewRight capability |
| UNUSED | @rightkit/media-gen-ui | 0.2.1 | 0.2.1 | 21200 | 705 | - | - | 0 | GenRight generation panels over a host client (README: panels, not an app); no app or package imports it; GenRight keeps its own 28k lines; nature: GenRight capability components |
| UNUSED | @rightkit/html-editor | 0.2.0 | 0.2.0 | 14647 | 0 | - | - | 1 | ViewRight HTML editor (14.6k); only dependent is document-workspace, itself unused; nature: ViewRight capability |
| UNUSED | @rightkit/docx-editor | 0.1.2 | 0.1.2 | 1982 | 2 | - | - | 1 | ViewRight docx pane (2k); only dependent is document-workspace, itself unused; nature: ViewRight capability |
| UNUSED | @rightkit/sync-worker | 0.1.0 | unpublished | 1164 | 0 | - | - | 0 | parked Cloudflare worker (pnpm-workspace.yaml excludes it), unpublished |
| UNUSED | @rightkit/send-ui | 0.1.0 | 0.1.0 | 742 | 67 | - | - | 0 | LocalSend UI panel (742 lines) for rightkit-send; no consumer; nature: Pulse capability |

### B2. Crates (228)

| Class | Name | Repo | Registry | Src lines | CSS lines | Apps (direct; m=main only, w=work only) | Apps carried (via dependents) | RK dependents (non-dev) | Reason |
|---|---|---|---|---|---|---|---|---|---|
| MANDATORY | qa | 0.10.8 | 0.10.8 | 51888 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 2 | owner of M20,M21,M25; Rust QA harness for Right Suite apps: engine black-box scenarios, hidden native UI dr |
| MANDATORY | tauri-shell | 0.8.2 | 0.8.2 | 32820 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | owner of M1; Shared Tauri desktop shell for Right Suite apps: webview hardening, window state, nat |
| MANDATORY | fs | 0.4.16 | 0.4.16 | 24026 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 99 | owner of M21,M8; Safe state directories, atomic writes, cross-process locks, owner-only files, and sta |
| MANDATORY | job-queue | 0.6.0 | 0.6.0 | 20140 |  | HR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 9 | owner of M3,M4; Persistent job store, runner, and stdio/CLI command contract for Right Suite engines. |
| MANDATORY | daemon | 0.9.0 | 0.9.0 | 18764 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 21 | owner of M21,M23,M6,M7; One discovery/auth convention for Right Suite local services: service.json, ranking a |
| MANDATORY | child-process | 0.5.13 | 0.5.13 | 15555 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 60 | owner of M3,M5; Ownership-safe child lifecycle, restart, and health primitives for Right Suite apps. |
| MANDATORY | license | 0.3.1 | 0.3.1 | 14686 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 3 | owner of M12,M13; Right Suite license enforcement primitive — offline verification of server-signed Ed2 |
| MANDATORY | db | 0.5.8 | 0.5.8 | 10135 |  | HR,VR,PU,CR,GR(m),MR,SR,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 19 | owner of M9; Correct-by-default SQLite and SQLCipher connection bootstrap for Right Suite apps. |
| MANDATORY | credentials | 0.5.5 | 0.5.5 | 9545 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 24 | owner of M11; Per-app OS credential storage for Right Suite apps: Keychain / Credential Manager / L |
| MANDATORY | http | 0.4.0 | 0.4.0 | 9125 |  | HR,PU,CR,GR,MR,SR,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 26 | owner of M17; Brand-neutral blocking HTTP client for Right Suite apps: timeouts, retry/backoff with |
| MANDATORY | logs | 0.4.2 | 0.4.2 | 8170 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 26 | owner of M15; Privacy-safe local JSONL logging and diagnostics for Right Suite desktop apps. |
| MANDATORY | app-settings | 0.4.1 | 0.4.1 | 5104 |  | HR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 10 | owner of M10; Versioned per-app settings persistence: migrations, typed defaults, atomic writes, co |
| MANDATORY | roots | 0.2.0 | 0.2.0 | 4895 |  | HR(w),PU(w),CR(w) | CR,CU,GR,HR,MR,PR,PU,SR,VR | 7 | M6 amendment v2.2 owner (draft, opt-in); used by HR/PU/CR work branches; Where a Right Suite app keeps its data: the one run-mode decision, the per-user suite |
| MANDATORY | crash | 0.4.1 | 0.4.1 | 3611 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 6 | owner of M16; Privacy-first panic capture, redacted crash reports, native dumps and support bundles |
| MANDATORY | agent-tools | 0.4.3 | 0.4.3 | 3559 |  | PU,GR,CU,TPL,E2E | CU,GR,PU | 2 | owner of M22; One typed tool interface for agents over rightkit-automation, rightkit-browser and ea |
| MANDATORY | sidecar | 0.3.4 | 0.3.4 | 2505 |  | HR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 10 | owner of M4; Bounded JSON-lines transport primitives for Right Suite sidecars. |
| MANDATORY | device | 0.5.1 | 0.5.1 | 515 |  | HR,VR,PU,CR,GR,MR,SR,CU,PR,TPL,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | owner of M1,M12; Native host services for Right Suite Tauri license and device adapters. |
| SHARED-CAPABILITY | repo-graph | 0.3.2 | 0.3.2 | 38372 |  | PU(w),CR(w) | CR,PU | 3 | used directly by CR,PU; Repository code graph: tree-sitter extraction across 30+ languages, symbol/reference  |
| SHARED-CAPABILITY | llm | 0.7.0 | 0.7.0 | 25825 |  | HR(m),PU,CR,MR,E2E | CR,CU,GR,HR,MR,PU | 8 | used directly by CR,HR,MR,PU; Brand-neutral BYOK LLM clients: Anthropic Messages and OpenAI-compatible chat, stream |
| SHARED-CAPABILITY | automation | 0.7.0 | 0.7.0 | 25271 |  | HR,PU,CU,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 11 | used directly by CU,HR,PU; Brand-neutral desktop automation: observe and act on any app (macOS AX, Windows UIA)  |
| SHARED-CAPABILITY | asr | 0.5.0 | 0.5.0 | 23079 |  | HR,SR,E2E | HR,SR | 2 | used directly by HR,SR; Parakeet ONNX + Whisper (CoreML / whisper.cpp) speech decoding runtime for Right Suit |
| SHARED-CAPABILITY | pdf | 0.3.5 | 0.3.5 | 16795 |  | VR,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 2 | used by VR and carried to CR,CU,GR,HR,MR,PR,PU,SR,VR through dependents; Product-neutral PDF engine: inspect, compose (source-text replace / add text / OCR co |
| SHARED-CAPABILITY | browser | 0.3.11 | 0.3.11 | 15375 |  | PU,CR,GR,SR,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 7 | used directly by CR,GR,PU,SR; Shared Chrome DevTools Protocol browser runtime for Right Suite: multi-page sessions, |
| SHARED-CAPABILITY | models | 0.7.1 | 0.7.1 | 15016 |  | HR,CR,GR(m),MR,SR,CU,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 10 | used directly by CR,CU,GR,HR,MR,SR; Shared local-model manifest and installer for Right Suite apps: one on-disk store (th |
| SHARED-CAPABILITY | mcp | 0.10.0 | 0.10.0 | 14274 |  | PU,CR,E2E | CR,CU,GR,PU | 6 | used directly by CR,PU; Shared MCP client (stdio + streamable HTTP, trust gate, pool, OAuth hook) and server  |
| SHARED-CAPABILITY | sandbox | 0.6.6 | 0.6.6 | 11672 |  | CR,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 12 | used by CR and carried to CR,CU,GR,HR,MR,PR,PU,SR,VR through dependents; Shared OS sandbox, PTY lease, process-tree, and reserve-before-spawn resource authori |
| SHARED-CAPABILITY | review | 0.6.0 | 0.6.0 | 10830 |  | CU,PR,E2E | CU,PR | 2 | used directly by CU,PR; Shared review capability for the content pipeline: deterministic checks over a media  |
| SHARED-CAPABILITY | agent-policy | 0.2.2 | 0.2.2 | 9799 |  | CR(w) | CR,CU,GR,HR,MR,PR,PU,SR,VR | 10 | used by CR and carried to CR,CU,GR,HR,MR,PR,PU,SR,VR through dependents; Agent tool-use policy: permission modes, command classification, path and network rul |
| SHARED-CAPABILITY | ffmpeg | 0.3.2 | 0.3.2 | 9033 |  | HR,PU,CR,GR,SR,CU,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 18 | used directly by CR,CU,GR,HR,PU,SR; Verified FFmpeg artifact and lifecycle primitives for Right Suite media pipelines. |
| SHARED-CAPABILITY | daemon-remote | 0.1.0 | 0.1.0 | 8036 |  | - | CU,PR | 1 | no app lists it; consumed through dependents of CU,PR; Remote side of a Right Suite daemon: the daemon-api v1 HTTPS host for enrolled device |
| SHARED-CAPABILITY | context-protocol | 0.2.0 | 0.2.0 | 7368 |  | PU | CR,PU | 9 | used by PU and carried to CR,PU through dependents; Context protocol contract: typed scope-grant, candidate-set, packet, receipt and know |
| SHARED-CAPABILITY | sync | 0.3.0 | 0.3.0 | 7047 |  | HR,PU,SR,E2E | HR,PU,SR | 1 | used directly by HR,PU,SR; Brand-neutral desktop/mobile cloud sync engine: change tokens, tombstones, last-write |
| SHARED-CAPABILITY | compose | 0.2.3 | 0.2.3 | 6877 |  | GR(w),CU,E2E | CU,GR | 1 | used directly by CU,GR; Product-neutral evaluated composition model: layered video, stills, cursor sprites, b |
| SHARED-CAPABILITY | oauth | 0.11.0 | 0.11.0 | 6567 |  | CR,MR,E2E | CR,CU,GR,HR,MR,PR,PU | 7 | used directly by CR,MR; Shared OAuth 2.0 sign-in for Right Suite apps: loopback PKCE, RFC 8628 device flow, p |
| SHARED-CAPABILITY | overlay-win | 0.1.5 | 0.1.5 | 6464 |  | HR,PU(w),E2E | HR,PU | 1 | used directly by HR,PU; Native Win32 layered-window overlays for Right Suite apps: software painter, DWM-pace |
| SHARED-CAPABILITY | memory-core | 0.2.0 | 0.2.0 | 6155 |  | PU(w) | CR,PU | 3 | used by PU and carried to CR,PU through dependents; Durable-memory core: memory tiers, retrieval ranking, a quality eval gate and token a |
| SHARED-CAPABILITY | timeline | 0.1.1 | 0.1.1 | 6109 |  | CU | CU,GR,PR | 7 | used by CU and carried to CU,GR,PR through dependents; Portable video timeline core: tracks, clips, rational time, revisions, ripple edits,  |
| SHARED-CAPABILITY | file-watch | 0.4.4 | 0.4.4 | 5751 |  | VR,PU,CR,E2E | CR,PU,VR | 3 | used directly by CR,PU,VR; Filesystem watching for Right Suite apps: ref-counted async subscribers, debounced fi |
| SHARED-CAPABILITY | video-project | 0.1.3 | 0.1.3 | 5749 |  | CU | CU,GR,PR | 6 | used by CU and carried to CU,GR,PR through dependents; Video project layer for Right Suite apps: project files, sources and their integrity, |
| SHARED-CAPABILITY | audio | 0.3.4 | 0.3.4 | 5311 |  | HR,CR,GR(m),SR,CU,E2E | CR,CU,GR,HR,PR,SR | 4 | used directly by CR,CU,GR,HR,SR; Shared audio for Right Suite apps: typed FFmpeg filter graphs, loudness gates, room t |
| SHARED-CAPABILITY | hotkeys | 0.1.4 | 0.1.4 | 5067 |  | HR,PU,CU,E2E | CU,HR,PU | 4 | used directly by CU,HR,PU; Global shortcuts and low-level input hooks for Right Suite apps: chord parsing, confl |
| SHARED-CAPABILITY | evidence-contracts | 0.1.1 | 0.1.1 | 5029 |  | - | CR,CU,PR | 15 | no app lists it; consumed through dependents of CR,CU,PR; Evidence contracts: plans, providers, findings reports, receipts, traces, source anch |
| SHARED-CAPABILITY | docs | 0.1.5 | 0.1.5 | 4992 |  | VR,PU,E2E | PU,VR | 1 | used directly by PU,VR; Product-neutral DOCX, XLSX and PDF read and write: format detection from content, typ |
| SHARED-CAPABILITY | embed | 0.7.0 | 0.7.0 | 4908 |  | PU,MR,E2E | CR,MR,PU | 5 | used directly by MR,PU; Product-neutral local text embeddings and persistent cosine vector store. |
| SHARED-CAPABILITY | spend | 0.2.3 | 0.2.3 | 4656 |  | PU,GR(m),PR,E2E | GR,PR,PU | 1 | used directly by GR,PR,PU; Spend reservation, allowance and compare-and-swap ledger on SQLite: integer micro-uni |
| SHARED-CAPABILITY | onnx | 0.3.2 | 0.3.2 | 4612 |  | HR,PU,CR,GR(m),MR,SR,CU,E2E | CR,CU,GR,HR,MR,PU,SR,VR | 15 | used directly by CR,CU,GR,HR,MR,PU,SR; Product-neutral ONNX Runtime dynamic-library resolution, environment, execution-provi |
| SHARED-CAPABILITY | agent-engine-protocol | 0.4.1 | 0.4.1 | 4484 |  | CR(w) | CR,CU,GR,HR,MR,PR,PU,SR,VR | 16 | used by CR and carried to CR,CU,GR,HR,MR,PR,PU,SR,VR through dependents; Agent engine protocol: events, envelopes, tool calls, trust wrapping and settlement t |
| SHARED-CAPABILITY | agent-config | 0.1.3 | 0.1.3 | 3841 |  | CR(w) | CR,CU,GR,HR,MR,PU | 6 | used by CR and carried to CR,CU,GR,HR,MR,PU through dependents; Layered TOML configuration for an agent host: defaults, user, workspace, environment  |
| SHARED-CAPABILITY | evidence-effects | 0.1.1 | 0.1.1 | 3819 |  | - | CR,CU,PR | 3 | no app lists it; consumed through dependents of CR,CU,PR; Effect execution with sandboxed child processes and receipts |
| SHARED-CAPABILITY | code-audit-provider-sdk | 0.1.1 | 0.1.1 | 3787 |  | - | CR,CU,PR | 7 | no app lists it; consumed through dependents of CR,CU,PR; Code audit provider SDK: provider registry, source and effect interfaces for audit pr |
| SHARED-CAPABILITY | search | 0.4.5 | 0.4.5 | 3779 |  | CR,GR(m),MR,SR,CU,E2E | CR,CU,GR,MR,PU,SR | 8 | used directly by CR,CU,GR,MR,SR; Product-neutral search query language: lexer, AST, operator values, SQL/FTS5 predicat |
| SHARED-CAPABILITY | speech-client | 0.5.0 | 0.5.0 | 3621 |  | HR,CR,SR,CU,E2E | CR,CU,GR,HR,PU,SR | 3 | used directly by CR,CU,HR,SR; Client and contract for the Orthic Labs shared speech service (one resident engine pe |
| SHARED-CAPABILITY | motion | 0.4.0 | 0.4.0 | 3121 |  | GR,CU,E2E | CU,GR | 1 | used directly by CU,GR; Product-neutral motion helpers for Right Suite video: bracketed-script beat sync, sto |
| SHARED-CAPABILITY | ocr | 0.3.0 | 0.3.0 | 2899 |  | VR,CR,SR,CU,E2E | CR,CU,SR,VR | 2 | used directly by CR,CU,SR,VR; Product-neutral OCR: Apple Vision / Windows.Media.Ocr native backends with per-line g |
| SHARED-CAPABILITY | compositor | 0.2.1 | 0.2.1 | 2603 |  | - | CU,GR | 1 | no app lists it; consumed through dependents of CU,GR; Native wgpu layer compositor for Right Suite video apps: GPU context, layer blending, |
| SHARED-CAPABILITY | contracts | 0.1.11 | 0.1.11 | 2543 |  | GR,CU,PR,E2E | CU,GR,PR | 5 | used directly by CU,GR,PR; Versioned content-pipeline contracts for the Right Suite: content brief, delivery env |
| SHARED-CAPABILITY | cache | 0.2.4 | 0.2.4 | 2110 |  | CR,MR,CU,PR,E2E | CR,CU,GR,MR,PR | 7 | used directly by CR,CU,MR,PR; Disk cache primitive with a hard size cap, LRU eviction that survives restarts, atomi |
| SHARED-CAPABILITY | color | 0.1.1 | 0.1.1 | 1988 |  | CU | CU,GR,PR | 1 | used by CU and carried to CU,GR,PR through dependents; Colour grading for Right Suite video apps: grade parameters, LUTs and FFmpeg filter r |
| SHARED-CAPABILITY | instance | 0.2.0 | 0.2.0 | 1930 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Single instance and second-launch forwarding without Tauri over rightkit-daemon, with |
| SHARED-CAPABILITY | engine-host | 0.2.0 | 0.2.0 | 1814 |  | GR(w),CU | CR,CU,GR,MR,PR | 7 | used directly by CU,GR; Engine binary framework: one command registry served as a headless JSON CLI and as a  |
| SHARED-CAPABILITY | overlay | 0.2.3 | 0.2.3 | 1670 |  | HR,PU,CU,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 7 | used directly by CU,HR,PU; Shared overlay state, hit testing, and placement policy for Right Suite apps |
| SHARED-CAPABILITY | approvals | 0.2.1 | 0.2.1 | 1567 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 8 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; One approvals core for every app: a pending request bound to a digest of its exact ar |
| SHARED-CAPABILITY | agent-task-contract | 0.1.1 | 0.1.1 | 1563 |  | PU(w) | CR,CU,GR,HR,MR,PR,PU,SR,VR | 3 | used by PU and carried to CR,CU,GR,HR,MR,PR,PU,SR,VR through dependents; The agent task contract: task envelope, its ten statuses and legal transitions, princ |
| SHARED-CAPABILITY | captions | 0.1.1 | 0.1.1 | 1501 |  | CU | CU,GR,PR | 2 | used by CU and carried to CU,GR,PR through dependents; Captions for Right Suite video apps: caption styles, per-language caption sets, cue l |
| SHARED-CAPABILITY | transcript | 0.4.2 | 0.4.2 | 1456 |  | HR,GR(m),SR,CU,E2E | CR,CU,GR,HR,PR,PU,SR | 13 | used directly by CU,GR,HR,SR; Product-neutral word-timed transcript contract, cue segmentation, and SRT/VTT/JSON im |
| SHARED-CAPABILITY | context-core | 0.2.0 | 0.2.0 | 1360 |  | PU | CR,PU | 3 | used by PU and carried to CR,PU through dependents; Context budget and lane reconciliation: one cross-provider attention budget whose exp |
| SHARED-CAPABILITY | identity | 0.1.2 | 0.1.2 | 1306 |  | E2E | CU,PR | 2 | no app lists it; consumed through dependents of CU,PR; Installation identity with clone detection and startup claim, device pairing with sig |
| SHARED-CAPABILITY | autostart | 0.1.0 | 0.1.0 | 1046 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Launch at login without Tauri: LaunchAgent or SMAppService on macOS, HKCU Run on Wind |
| SHARED-CAPABILITY | notifications | 0.1.0 | 0.1.0 | 1014 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Neutral native notifications owner usable without Tauri: Client, Notification builder |
| SHARED-CAPABILITY | vad | 0.2.0 | 0.2.0 | 1012 |  | HR,CU,E2E | CU,HR,SR | 2 | used directly by CU,HR; Product-neutral voice activity detection: Silero scorer, hysteresis/padding/chunking  |
| SHARED-CAPABILITY | playback | 0.3.1 | 0.3.1 | 995 |  | SR,CU,E2E | CU,SR | 0 | used directly by CU,SR; Source-preserving FFmpeg compatibility playback preparation for Right Suite media app |
| SHARED-CAPABILITY | pty | 0.1.1 | 0.1.1 | 956 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Async managed PTY child for Right Suite apps: owned tree, resize, bounded input, EOF, |
| SHARED-CAPABILITY | doctor | 0.2.0 | 0.2.0 | 932 |  | CR,MR,CU | CR,CU,MR | 1 | used directly by CR,CU,MR; Doctor framework for RightKit engines and CLIs: check rows, verdicts and exit codes,  |
| SHARED-CAPABILITY | crypto | 0.1.3 | 0.1.3 | 823 |  | HR,PU,CR,GR,MR,PR,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 39 | used directly by CR,GR,HR,MR,PR,PU; Neutral crypto primitives: SHA-256, HMAC, constant-time compare, base64/base64url, he |
| SHARED-CAPABILITY | clipboard | 0.1.0 | 0.1.0 | 819 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Rich clipboard without Tauri: text, HTML, PNG, file lists and app-defined formats in  |
| SHARED-CAPABILITY | agent-engine-runtime-resources | 0.1.1 | 0.1.1 | 777 |  | CR | CR,CU,GR,PU | 4 | used by CR and carried to CR,CU,GR,PU through dependents; Agent engine runtime resources: resource discovery and limits for agent turns |
| SHARED-CAPABILITY | app-update | 0.2.0 | 0.2.0 | 658 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Native half of Right Suite app updates that the Tauri shell does not carry: the faile |
| SHARED-CAPABILITY | time | 0.1.4 | 0.1.4 | 630 |  | HR,PU,CR,GR,MR,SR,PR,E2E | CR,CU,GR,HR,MR,PR,PU,SR,VR | 27 | used directly by CR,GR,HR,MR,PR,PU,SR; Time owner: RFC 3339 parse and format, fixed UTC offsets, civil date math and HTTP-da |
| SHARED-CAPABILITY | signal | 0.1.0 | 0.1.0 | 613 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Cross-process wake-up signals between a native companion and a host app: Darwin notif |
| SHARED-CAPABILITY | typeset | 0.1.2 | 0.1.2 | 593 |  | E2E | CU,GR,PR | 1 | no app lists it; consumed through dependents of CU,GR,PR; Typst rendering for titles, captions and math, with mitex LaTeX input, for Right Suit |
| SHARED-CAPABILITY | presence | 0.1.1 | 0.1.1 | 398 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Meeting and fullscreen presence detection and Do-Not-Disturb state without Tauri: win |
| SHARED-CAPABILITY | deep-link | 0.1.1 | 0.1.1 | 169 |  | - | CR,CU,GR,HR,MR,PR,PU,SR,VR | 1 | no app lists it; consumed through dependents of CR,CU,GR,HR,MR,PR,PU,SR,VR; Deep-link queue without Tauri: scheme validation, argv extraction and a bounded queue |
| SHARED-CAPABILITY | repo-graph-grammars | 0.1.1 | 0.1.1 | 60 |  | PU(w) | CR,PU | 1 | used by PU and carried to CR,PU through dependents; Vendored tree-sitter grammars (ql, systemrdl, vue) for the repository graph, with no  |
| APP-CAPABILITY | agent-coding-tools | 0.1.8 | 0.1.8 | 53365 |  | - | CR | 6 | only reached via CodeRight through a dependent crate; The coding agent's host tools (files, edit and patch, shell and PTY, git, browser, MC |
| APP-CAPABILITY | agent-orchestration | 0.1.6 | 0.1.5 | 38209 |  | CR(w) | CR | 4 | only CodeRight uses it; Agent orchestration runtime: plans, routes, tasks, budgets, grants, escalation, sched |
| APP-CAPABILITY | media-gen | 0.2.3 | 0.2.3 | 35664 |  | GR(w) | GR | 5 | only GenRight uses it; Generative media providers (fal, WaveSpeed, Veo/Gemini, OpenAI Images, ElevenLabs, He |
| APP-CAPABILITY | agents | 0.2.7 | 0.2.7 | 23517 |  | CR,E2E | CR | 2 | only CodeRight uses it; Supervised vendor agent sessions for Right Suite apps: shared agent event model, Clau |
| APP-CAPABILITY | adapt | 0.2.0 | 0.2.0 | 15220 |  | PU(w) | PU | 2 | only Pulse uses it; Governed behavioural learning from agent transcripts (Pulse's Membrane Adapt): taste  |
| APP-CAPABILITY | social-publish | 0.2.0 | 0.2.0 | 13982 |  | PR | PR | 3 | only PostRight uses it; Social platform publish adapters (X, Bluesky, Mastodon, Threads, Instagram, Facebook, |
| APP-CAPABILITY | mesh | 0.2.2 | 0.2.2 | 13285 |  | PU(w) | PU | 1 | only Pulse uses it; Device mesh for Right Suite apps: iroh live sessions between paired devices, end-to-e |
| APP-CAPABILITY | disk-cleanup | 0.2.0 | 0.2.0 | 12938 |  | PU | PU | 1 | only Pulse uses it; Disk usage scanning and safe cleanup: symlink-safe bounded scan, cleanup rules with e |
| APP-CAPABILITY | clipboard-history | 0.3.2 | 0.3.2 | 12575 |  | PU(w) | PU | 1 | only Pulse uses it; Clipboard history logic: classify copied text (colour, link, code, plain) and spot wh |
| APP-CAPABILITY | agent-engine-storage | 0.3.0 | 0.3.0 | 12416 |  | CR(w) | CR | 4 | only CodeRight uses it; Agent engine SQLite storage: session event journal, transcripts, tasks, artifacts, go |
| APP-CAPABILITY | workflow | 0.2.3 | 0.2.3 | 11772 |  | CR,E2E | CR | 2 | only CodeRight uses it; Shared scenario interpreter and durable state machine for the RightKit scenario runti |
| APP-CAPABILITY | media-pipeline | 0.2.1 | 0.2.1 | 11465 |  | GR(w) | GR | 0 | only GenRight uses it; Named production pipelines on rightkit-media-gen: compose, dubbing with retiming, tra |
| APP-CAPABILITY | context-federation | 0.2.1 | 0.2.1 | 10919 |  | PU(w) | PU | 1 | only Pulse uses it; Context federation coordinator: immutable provider registry, fan-out of context opera |
| APP-CAPABILITY | autoedit | 0.2.0 | 0.2.0 | 10637 |  | CU | CU | 1 | only CutRight uses it; Automatic editing for Right Suite video apps: transcript-driven cut engine (retakes,  |
| APP-CAPABILITY | fsindex | 0.6.2 | 0.6.2 | 9861 |  | PU,E2E | PU | 1 | only Pulse uses it; Live whole-disk file index: fuzzy name search plus clone- and hard-link-aware folder  |
| APP-CAPABILITY | record | 0.4.0 | 0.4.0 | 9649 |  | CU,E2E | CU | 1 | only CutRight uses it; Per-window screen capture for any app, chosen by window id and pid: ScreenCaptureKit  |
| APP-CAPABILITY | local-gen | 0.1.3 | 0.1.3 | 8668 |  | GR | GR | 3 | only GenRight uses it; Keyless local image generation: managed sd.cpp server runtime, memory modes, model pr |
| APP-CAPABILITY | memory-store | 0.2.0 | 0.2.0 | 8640 |  | - | CR | 2 | only reached via CodeRight through a dependent crate; Durable memory store: SQLite memory store, installation identity, scopes and durable  |
| APP-CAPABILITY | send | 0.4.0 | 0.4.0 | 7368 |  | PU,E2E | PU | 0 | only Pulse uses it; Nearby file and text sharing over the open LocalSend v2 protocol, with Pulse extensio |
| APP-CAPABILITY | notch-ui | 0.2.1 | 0.2.1 | 6419 |  | PU(w) | PU | 1 | only Pulse uses it; Notch drawing core: edge-aware geometry, spring motion, hover card model and a softwa |
| APP-CAPABILITY | app-manager | 0.2.2 | 0.2.2 | 6226 |  | PU(w) | PU | 1 | only Pulse uses it; Installed-app management: inventory, icons, usage, updates, safe uninstall with lefto |
| APP-CAPABILITY | ai-accounts | 0.1.1 | 0.1.1 | 5989 |  | PU | PU | 1 | only Pulse uses it; AI assistant account management for Claude: Desktop login slots and switching, chat s |
| APP-CAPABILITY | dictation-command-host | 0.2.1 | 0.2.1 | 5929 |  | HR(w) | HR | 3 | only HeardRight uses it; Dictation command host: voice command dispatch (keys, app launch, Shortcuts, power wi |
| APP-CAPABILITY | google-apis | 0.2.0 | 0.2.0 | 5725 |  | MR | MR | 1 | only MailRight uses it; Async Google Workspace REST clients: Gmail (sync, history, batch fetch, MIME, compose |
| APP-CAPABILITY | dictation-text-pipeline | 0.2.5 | 0.2.5 | 4532 |  | HR(w) | HR | 4 | only HeardRight uses it; Dictation text pipeline: LLM cleanup (L1-L3) with provider transports and policy, pro |
| APP-CAPABILITY | owner-approvals | 0.1.2 | 0.1.2 | 4276 |  | - | PU | 4 | only reached via Pulse through a dependent crate; Signed owner approvals for privileged and reserved actions: owner-check and approver  |
| APP-CAPABILITY | hooks | 0.2.11 | 0.2.11 | 4275 |  | PU,E2E | PU | 4 | only Pulse uses it; Typed Claude Code and Codex hook payloads, a deadline-bound exit-0 dispatcher, exact  |
| APP-CAPABILITY | system-monitor | 0.3.0 | 0.3.0 | 4190 |  | PU | PU | 2 | only Pulse uses it; Live system monitoring: CPU, memory pressure, hang-proof disk listing, per-process fo |
| APP-CAPABILITY | win-ui | 0.1.1 | 0.1.1 | 3454 |  | PU(w) | PU | 3 | only Pulse uses it; Win32 UI toolkit: RAII window and GDI wrappers, software canvas, GDI surface and text |
| APP-CAPABILITY | text-polish | 0.1.1 | 0.1.1 | 3349 |  | HR | HR | 6 | only HeardRight uses it; Transcript polish: deterministic L0 cleanup (numbers, dates, inline edits, formatting |
| APP-CAPABILITY | dictation-core | 0.3.1 | 0.3.1 | 3156 |  | HR | HR | 9 | only HeardRight uses it; Dictation core: engine wire protocol and validation, provider key handoff, recording  |
| APP-CAPABILITY | shorts | 0.2.4 | 0.2.4 | 3143 |  | CU | CU | 0 | only CutRight uses it; Shorts finder for Right Suite video apps: candidate moments from a long video's trans |
| APP-CAPABILITY | remote-compute | 0.2.1 | 0.2.1 | 3068 |  | GR(w) | GR | 0 | only GenRight uses it; Rented GPU workers: SSH and Vast.ai drivers, register/stage/run/teardown lifecycle, a |
| APP-CAPABILITY | tts | 0.3.0 | 0.3.0 | 2967 |  | GR,E2E | GR | 1 | only GenRight uses it; Shared local text-to-speech for Right Suite apps: OS voices (say / SAPI) and a Qwen3- |
| APP-CAPABILITY | agent-telemetry | 0.1.0 | 0.1.0 | 2961 |  | CR(w) | CR | 4 | only CodeRight uses it; Agent runtime telemetry for an agent host: resource metrics, turn latency, round trip |
| APP-CAPABILITY | reframe | 0.2.0 | 0.2.0 | 2762 |  | CU | CU | 0 | only CutRight uses it; Tracked 9:16 reframe for Right Suite video apps: ONNX face detection (YuNet; CoreML,  |
| APP-CAPABILITY | mail | 0.1.2 | 0.1.2 | 2732 |  | MR | MR | 2 | only MailRight uses it; Mail-domain building blocks: Gmail-style search query compiler, SQL predicates and ke |
| APP-CAPABILITY | bot-messaging | 0.3.1 | 0.3.1 | 2643 |  | CR(w) | CR | 1 | only CodeRight uses it; Bot messaging bridge: remote chat channels (Telegram, Slack, Discord style) relaying  |
| APP-CAPABILITY | media-gen-client | 0.1.0 | 0.1.0 | 2619 |  | - | CU | 1 | only reached via CutRight through a dependent crate; Client of the generation engine contract: drive an installed media-generation engine  |
| APP-CAPABILITY | document-workspace | 0.2.0 | 0.2.0 | 2614 |  | VR | VR | 0 | only ViewRight uses it; Document workspace store: recent files, pinned files and folders, per-document view s |
| APP-CAPABILITY | launcher | 0.2.0 | 0.2.0 | 2444 |  | - | PU | 2 | only reached via Pulse through a dependent crate; Portable command launcher core: calculator and unit conversion that never hand typed  |
| APP-CAPABILITY | channel-memory | 0.2.3 | 0.2.3 | 2192 |  | PR,E2E | PR | 0 | only PostRight uses it; Channel memory for the content pipeline: the channel-memory.v1 schema with serde type |
| APP-CAPABILITY | media-gen-models | 0.1.1 | 0.1.1 | 2168 |  | GR(w) | GR | 0 | only GenRight uses it; Local model management for the media-generation engine: hardware fit, installs throug |
| APP-CAPABILITY | agent-plugins | 0.1.2 | 0.1.2 | 2093 |  | - | CR | 3 | only reached via CodeRight through a dependent crate; Agent plugin manifests, discovery and loader |
| APP-CAPABILITY | agent-transcript | 0.1.3 | 0.1.3 | 1878 |  | PU | PU | 2 | only Pulse uses it; Deterministic agent transcript normalizer (TranscriptEventV1): Claude Code and Codex  |
| APP-CAPABILITY | agent-engine-history | 0.1.4 | 0.1.4 | 1820 |  | - | CR | 3 | only reached via CodeRight through a dependent crate; Agent engine conversation history store helpers |
| APP-CAPABILITY | web-extract | 0.1.2 | 0.1.2 | 1783 |  | SR | SR | 1 | only ScrapeRight uses it; Article extraction without transport: readable-text parsing, title and redirect resol |
| APP-CAPABILITY | transcript-library | 0.1.2 | 0.1.2 | 1526 |  | HR | HR | 9 | only HeardRight uses it; Transcript library: delivery records (outcome, target, timings, source), history stor |
| APP-CAPABILITY | media-gen-approvals | 0.1.1 | 0.1.1 | 1386 |  | GR(w) | GR | 2 | only GenRight uses it; Media-generation approvals on the rightkit-approvals core: launch-secret approval tok |
| APP-CAPABILITY | agent-gateway | 0.3.0 | 0.3.0 | 1224 |  | MR | MR | 2 | only MailRight uses it; Core of a local agent gateway, with no product verbs: agent registry (hashed pairing  |
| APP-CAPABILITY | platform-rules | 0.1.1 | 0.1.1 | 1195 |  | PR,E2E | PR | 2 | only PostRight uses it; Versioned catalog of per-platform limits and rules with a stable rule-revision hash a |
| APP-CAPABILITY | agent-host-traits | 0.1.3 | 0.1.3 | 1086 |  | - | CR | 5 | only reached via CodeRight through a dependent crate; Host traits for agent runtimes: a skill catalog the host implements and installs, so  |
| APP-CAPABILITY | voice-commands | 0.1.1 | 0.1.1 | 1063 |  | HR | HR | 4 | only HeardRight uses it; Voice command recognition: typed command actions, spoken-command catalog and matching |
| APP-CAPABILITY | agent-daemon | 0.2.0 | 0.2.0 | 989 |  | CR(w) | CR | 2 | only CodeRight uses it; Daemon primitives for an agent host: trusted-source authority gate, daemon settings,  |
| APP-CAPABILITY | context-provider-sdk | 0.2.0 | 0.2.0 | 916 |  | PU | PU | 2 | only Pulse uses it; Context provider SDK: the typed Provider trait, the canonical Fixture shape and the r |
| APP-CAPABILITY | scheduler | 0.3.0 | 0.3.0 | 870 |  | - | CR | 2 | only reached via CodeRight through a dependent crate; Agent schedule model: bot profiles, schedule specs (cron, once, interval, monitor), d |
| APP-CAPABILITY | evidence | 0.1.1 | 0.1.1 | 829 |  | CR | CR | 1 | only CodeRight uses it; Evidence and spec records: requirement specs, acceptance evidence and verification st |
| APP-CAPABILITY | device-link-types | 0.1.2 | 0.1.2 | 663 |  | - | PU | 4 | only reached via Pulse through a dependent crate; Record types of the device link: device reference, effect classes, principals, paired |
| APP-CAPABILITY | agent-engine-runtime-middleware | 0.1.3 | 0.1.3 | 657 |  | CR(w) | CR | 3 | only CodeRight uses it; Agent engine runtime middleware: request and turn interception hooks |
| APP-CAPABILITY | agent-engine-tasks | 0.3.0 | 0.3.0 | 339 |  | CR(w) | CR | 2 | only CodeRight uses it; Agent engine task model: task records, states and transitions for agent hosts |
| APP-CAPABILITY | agent-engine-api-platform | 0.1.3 | 0.1.3 | 336 |  | CR(w) | CR | 1 | only CodeRight uses it; Agent engine platform HTTP API types |
| APP-CAPABILITY | privileged-helper | 0.2.0 | 0.2.0 | 209 |  | PU(w) | PU | 0 | only Pulse uses it; Elevation broker for root-owned uninstall moves: parks paths for a trusted UI that as |
| APP-CAPABILITY | context-client | 0.2.0 | 0.2.0 | 185 |  | PU | PU | 1 | only Pulse uses it; Transport-injected client for canonical context v1 operation envelopes. |
| APP-CAPABILITY | agent-engine-cli | 0.1.1 | 0.1.1 | 183 |  | CR | CR | 1 | only CodeRight uses it; Agent engine command-line argument model |
| APP-CAPABILITY | app-host | 0.1.2 | 0.1.2 | 129 |  | PU | PU | 5 | only Pulse uses it; Host services a desktop app gives RightKit companion crates (state folder, account ho |
| APP-CAPABILITY | memory | 0.3.0 | 0.3.0 | 25 |  | CR | CR | 0 | only CodeRight uses it; The RightKit memory engine (Pulse's Cortex): memory tiers, retrieval, planning and re |
| APP-CAPABILITY | pipeline | 0.2.0 | 0.2.0 | 19 |  | CU | CU | 0 | deprecated alias of rightkit-media-gen-client (19 lines); CU; Deprecated: renamed rightkit-media-gen-client (the generation engine client). Re-expo |
| APP-CAPABILITY | adapt-taste | 0.1.3 | 0.1.3 | 5 |  | PU(m) | PU | 0 | deprecated alias of rightkit-adapt (5 lines); PU main; Deprecated name of rightkit-adapt (governed behavioural learning: taste and insights) |
| TOOLING | ownership | 0.4.6 | 0.4.6 | 11919 |  | E2E | - | 0 | ownership scanner/broker; RightKit ownership scanner, verdict and admission: the single Rust implementation the |
| TOOLING | apple-ops | 0.2.0 | 0.2.0 | 10034 |  | - | - | 0 | App Store Connect/build analysis tooling; Apple ops: App Store Connect API and upload, build analysis, preflight, profiling and |
| TOOLING | qa-driver | 0.7.2 | 0.7.1 | 955 |  | E2E | - | 0 | deprecated shim for rightkit-qa; Deprecated shim: use rightkit-qa (`control` feature) for the in-app QA control server |
| TOOLING | bench | 0.2.1 | 0.2.1 | 885 |  | SR | SR | 0 | benchmark harness (SR); Benchmark harness building blocks: warmup-and-percentile timing, concurrent load driv |
| TOOLING | connector-gen | 0.1.0 | 0.1.0 | 528 |  | - | - | 0 | n8n/Zapier/Make connector generator; Generate workflow-automation connectors (n8n node, Zapier app, Make app) from an engi |
| TOOLING | catalog | 0.1.0 | 0.1.0 | 449 |  | - | - | 0 | localisation catalog generator; One localisation catalog source (a folder of JSON string catalogs, English as the ref |
| UNUSED | agent-server | 0.2.0 | 0.1.1 | 160151 |  | - | - | 0 | no app and no used crate depends on it (nature: CodeRight/Legion agent stack); Agent server host: HTTP/daemon API routes, CLI and interactive-session runtime, sessi |
| UNUSED | code-audit-engine | 0.2.0 | 0.1.0 | 130008 |  | - | - | 4 | no app and no used crate depends on it (dependents: code-audit-commands,code-audit-qualification,evidence-application,evidence-commands are themselves unused) (nature: Legion code audit); Repository code audit engine: native providers, DAG and scheduler, evidence issuers,  |
| UNUSED | context-runtime | 0.1.4 | 0.1.4 | 62135 |  | - | - | 1 | no app and no used crate depends on it (dependents: context are themselves unused) (nature: Pulse context); Context runtime: pull/push context packets, durable memory service, ledger, freshness |
| UNUSED | terminal-ui | 0.1.3 | 0.1.3 | 42631 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused); Terminal UI for agent sessions: ratatui chat transcript, composer, approvals, diffs a |
| UNUSED | agent-engine-core | 0.1.3 | 0.1.2 | 33295 |  | - | - | 3 | no app and no used crate depends on it (dependents: agent-engine,agent-engine-mobile-runtime,agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent engine core: the agent turn loop, tool dispatch, permission and approval flow,  |
| UNUSED | chat-bridge | 0.2.0 | 0.2.0 | 22601 |  | - | - | 1 | no app and no used crate depends on it (dependents: remote-gateway are themselves unused); Chat between AI chats: envelopes, linked computers and the device link, native delive |
| UNUSED | remote-gateway | 0.1.3 | 0.1.3 | 17996 |  | - | - | 1 | no app and no used crate depends on it (dependents: web-session are themselves unused); Remote tools for agents on this computer: files, documents, terminals, processes, scr |
| UNUSED | dictation-inference | 0.2.2 | 0.2.2 | 17234 |  | - | - | 2 | no app and no used crate depends on it (dependents: dictation-engine,dictation-runtime are themselves unused) (nature: HeardRight dictation stack); Dictation inference: Parakeet TDT on CoreML and ONNX Runtime, Whisper, keyword spotti |
| UNUSED | dictation-runtime | 0.2.2 | 0.2.2 | 15002 |  | - | - | 1 | no app and no used crate depends on it (dependents: dictation-engine are themselves unused) (nature: HeardRight dictation stack); Dictation engine runtime: the JSON-RPC sidecar worker (recording, finalize, polish, d |
| UNUSED | prompt-recipes | 0.2.0 | 0.2.0 | 11540 |  | - | - | 0 | no app and no used crate depends on it; Prompt contracts and validator, recipe composer, image workflow catalogue, character  |
| UNUSED | tts-qwen | 0.3.0 | 0.3.0 | 9985 |  | E2E | - | 0 | no app and no used crate depends on it; Qwen3 TTS inference (GGUF + GGML), vendored from yet-another-ai/qts v0.2.0 for RightK |
| UNUSED | code-audit-commands | 0.2.0 | 0.1.0 | 9840 |  | - | - | 0 | no app and no used crate depends on it (nature: Legion code audit); Code audit command logic (audit, assurance, coverage, minimize, report, review, rules |
| UNUSED | tts-qwen-sys | 0.1.5 | 0.1.5 | 8547 |  | E2E | - | 1 | no app and no used crate depends on it (dependents: tts-qwen are themselves unused); Low-level FFI bindings to ggml (CPU, Metal, Vulkan, BLAS) for rightkit-tts-qwen, vend |
| UNUSED | agent-engine-mobile-runtime | 0.1.1 | 0.1.0 | 8406 |  | - | - | 0 | no app and no used crate depends on it (nature: CodeRight/Legion agent stack); Agent engine mobile runtime: in-process engine host for platforms without fork/exec ( |
| UNUSED | launcher-win | 0.1.0 | 0.1.0 | 7963 |  | - | - | 0 | no app and no used crate depends on it; The Windows launcher panel over rightkit-launcher: a layered search window with apps, |
| UNUSED | research | 0.1.2 | 0.1.2 | 7930 |  | - | - | 0 | no app and no used crate depends on it; Research pipeline: sources, claims and evidence with provenance |
| UNUSED | mail-store | 0.2.1 | 0.2.1 | 7360 |  | - | - | 0 | no app and no used crate depends on it (nature: MailRight); Mail store over SQLCipher: encrypted open with key candidates and lifetime lock, vers |
| UNUSED | evidence-commands | 0.2.0 | 0.1.0 | 7115 |  | - | - | 1 | no app and no used crate depends on it (dependents: code-audit-commands are themselves unused) (nature: Legion evidence); Evidence, freeze, contract, decision, governance, key, state and run command logic as |
| UNUSED | agent-orchestration-handoff | 0.1.2 | 0.1.2 | 6794 |  | - | - | 0 | no app and no used crate depends on it (nature: CodeRight/Legion agent stack); Agent handoff packets, continuity gate and live state |
| UNUSED | policy-rules | 0.1.1 | 0.1.1 | 6538 |  | - | - | 2 | no app and no used crate depends on it (dependents: code-audit-commands,evidence-commands are themselves unused); Agent policy rules: policy model, evaluator, precedence, frozen scopes and explanatio |
| UNUSED | mail-gateway | 0.1.0 | 0.1.0 | 6488 |  | - | - | 0 | no app and no used crate depends on it (nature: MailRight); The mail adapter of the agent gateway: mail verbs (search, thread, draft, send with a |
| UNUSED | evidence-governance | 0.1.1 | 0.1.1 | 6091 |  | - | - | 1 | no app and no used crate depends on it (dependents: evidence-commands are themselves unused) (nature: Legion evidence); Evidence governance: certification, judgment dispatch and lifecycle receipts over evi |
| UNUSED | code-audit-qualification | 0.2.0 | 0.1.0 | 5525 |  | - | - | 0 | no app and no used crate depends on it (nature: Legion code audit); Qualification tooling for the code audit engine: fixture bench scoring, semantic caps |
| UNUSED | media-fetch | 0.2.0 | 0.2.0 | 5087 |  | - | - | 0 | no app and no used crate depends on it; Media fetch: yt-dlp download and 16 kHz mono WAV normalize, playlist expansion, and a |
| UNUSED | mesh-ffi | 0.1.0 | unpublished | 4784 |  | - | - | 0 | no app and no used crate depends on it; C ABI over rightkit-mesh for a phone (the joiner side of short-code pairing and one r |
| UNUSED | dictation-tauri | 0.2.1 | 0.2.1 | 4664 |  | - | - | 0 | no app and no used crate depends on it (nature: HeardRight dictation stack); Dictation runtime for a desktop shell: recording state machine over the engine sideca |
| UNUSED | github | 0.2.0 | 0.2.0 | 4625 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused); GitHub App authority contracts, forge request specs, App state store, webhook verific |
| UNUSED | memory-client | 0.1.1 | 0.1.0 | 4286 |  | - | - | 2 | no app and no used crate depends on it (dependents: agent-engine-mobile-runtime,agent-server are themselves unused) (nature: Pulse memory); Client of the RightKit memory engine: the typed MemoryBackend over the external membr |
| UNUSED | agent-skills | 0.1.0 | 0.1.0 | 4128 |  | - | - | 0 | no app and no used crate depends on it (nature: CodeRight/Legion agent stack); Agent skills and slash-command grammar: skill discovery, manifests, the shared comman |
| UNUSED | social-analytics | 0.3.0 | 0.3.0 | 3965 |  | - | - | 2 | no app and no used crate depends on it (dependents: content-plan,social-inbox are themselves unused) (nature: PostRight); Social campaign analytics without a store: tracked short links (public-https destinat |
| UNUSED | content-plan | 0.2.1 | 0.2.1 | 3613 |  | - | - | 0 | no app and no used crate depends on it; Store-free content planning rules: manual draft approval gate, first-sentence disclos |
| UNUSED | library | 0.3.0 | 0.3.0 | 3519 |  | - | - | 0 | no app and no used crate depends on it; Local artifact library: SQLite-backed history/search index (files stay the source of  |
| UNUSED | agent-demo | 0.2.0 | 0.2.0 | 3288 |  | - | - | 0 | no app and no used crate depends on it (nature: CodeRight/Legion agent stack); Consented agent demo mode for Tauri hosts: an agent drives one catalog app through ri |
| UNUSED | context | 0.2.0 | 0.2.0 | 3189 |  | - | - | 0 | no app and no used crate depends on it (nature: Pulse context); Context, memory and repository-graph commands for coding agents: the membrane CLI, ho |
| UNUSED | dictation-models | 0.2.0 | 0.2.0 | 2980 |  | - | - | 3 | no app and no used crate depends on it (dependents: dictation-app-runtime,dictation-runtime,dictation-tauri are themselves unused) (nature: HeardRight dictation stack); Dictation model store: speech model catalog, shared model leases and paths, downloads |
| UNUSED | agent-learning | 0.1.2 | 0.1.2 | 2809 |  | - | - | 2 | no app and no used crate depends on it (dependents: agent-engine-api-workflows,agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Post-session learning for a coding agent (CodeRight): proposal extraction, evaluation |
| UNUSED | dictation-app-runtime | 0.2.0 | 0.2.0 | 2489 |  | - | - | 1 | no app and no used crate depends on it (dependents: dictation-tauri are themselves unused) (nature: HeardRight dictation stack); Dictation app runtime: the shell-side engine supervisor (spawn, launch frame, health, |
| UNUSED | social-inbox | 0.3.1 | 0.3.1 | 2296 |  | - | - | 0 | no app and no used crate depends on it (nature: PostRight); Signed social webhook inbox core: constant-time HMAC-SHA256 signature verification, b |
| UNUSED | evidence-report | 0.1.1 | 0.1.1 | 2166 |  | - | - | 3 | no app and no used crate depends on it (dependents: code-audit-commands,code-audit-engine,evidence-application are themselves unused) (nature: Legion evidence); Evidence reports: findings report rendering to JSON, SARIF, Markdown and HTML |
| UNUSED | web-session | 0.1.1 | 0.1.1 | 1954 |  | - | - | 0 | no app and no used crate depends on it; Agent browser logins: one owner-approved Chromium session at a time on the login prof |
| UNUSED | evidence-application | 0.2.0 | 0.1.0 | 1696 |  | - | - | 2 | no app and no used crate depends on it (dependents: code-audit-commands,evidence-commands are themselves unused) (nature: Legion evidence); In-process evidence application: composes audit, orchestration, policy and report cra |
| UNUSED | dictation-telemetry | 0.2.0 | 0.2.0 | 1398 |  | - | - | 3 | no app and no used crate depends on it (dependents: dictation-app-runtime,dictation-models,dictation-tauri are themselves unused) (nature: HeardRight dictation stack); Dictation telemetry: local utterance timelines, telemetry writer, crash dump capture, |
| UNUSED | align | 0.1.0 | unpublished | 1363 |  | E2E | - | 0 | no app and no used crate depends on it; Product-neutral script-to-ASR word alignment: pronunciation normalisation, edit-dista |
| UNUSED | evidence-decisions | 0.1.1 | 0.1.1 | 1343 |  | - | - | 1 | no app and no used crate depends on it (dependents: evidence-commands are themselves unused) (nature: Legion evidence); Evidence decision store: recorded decisions with SQLite persistence and validation |
| UNUSED | sync-keys | 0.1.0 | unpublished | 1311 |  | E2E | - | 0 | no app and no used crate depends on it; End-to-end sync key management for Right Suite apps: account data key, versioned rota |
| UNUSED | agent-engine-attention | 0.1.3 | 0.1.3 | 1268 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent attention inbox: unread/needs-input items, snooze, density, quiet hours and dee |
| UNUSED | agent-engine-api-workflows | 0.2.2 | 0.2.2 | 1165 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent engine workflow HTTP API: request/response types, error mapping and OpenAPI fra |
| UNUSED | vault | 0.3.0 | 0.3.0 | 1164 |  | E2E | - | 0 | no app and no used crate depends on it; Split-key secret sync for Right Suite apps: a small secret map sealed under a random  |
| UNUSED | agent-recipes | 0.1.2 | 0.1.2 | 1161 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent recipes: reusable, parameterised task recipes with validation |
| UNUSED | android | 0.3.1 | 0.3.1 | 1123 |  | E2E | - | 0 | no app and no used crate depends on it; Android bridge for Right Suite apps: the UniFFI surface and JNI context capture (Java |
| UNUSED | app-lock | 0.1.0 | 0.1.0 | 1045 |  | - | - | 0 | no app and no used crate depends on it; Application lock: lock state machine types, preference fail-closed rules, OS user ver |
| UNUSED | device-link | 0.1.0 | 0.1.0 | 924 |  | - | - | 0 | no app and no used crate depends on it; Reliable ordered message delivery between linked devices over a relay: sequence numbe |
| UNUSED | gpu | 0.1.0 | 0.1.0 | 905 |  | - | - | 0 | no app and no used crate depends on it; Shared wgpu device context for Right Suite apps: per-OS backend policy, a crash senti |
| UNUSED | power | 0.1.0 | 0.1.0 | 900 |  | - | - | 0 | no app and no used crate depends on it; Sleep, resume, session lock, battery, energy saver, display state and keep-awake for  |
| UNUSED | feed | 0.1.0 | 0.1.0 | 816 |  | - | - | 0 | no app and no used crate depends on it; RSS 2.0, RSS 1.0 and Atom feed parser: tolerant, bounded, DOCTYPE skipped, only the f |
| UNUSED | dictation-ffi | 0.2.0 | 0.2.0 | 790 |  | - | - | 0 | no app and no used crate depends on it (nature: HeardRight dictation stack); Stable C surface for native shells over the dictation text pipeline, ASR audio condit |
| UNUSED | theme | 0.1.0 | 0.1.0 | 670 |  | - | - | 0 | no app and no used crate depends on it; Resolved theme tokens for non-web UIs: the @rightkit/theme brand resolver (derived da |
| UNUSED | agent-local-models | 0.1.1 | 0.1.1 | 598 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Local model primitives for an agent host: hardware report, runtime detection, local m |
| UNUSED | desktop-tweaks | 0.3.0 | 0.3.0 | 435 |  | - | - | 0 | no app and no used crate depends on it; Desktop tweaks for Windows: Mac-style editing shortcuts (left Alt as Ctrl, terminal-a |
| UNUSED | agent-engine-api-integrations | 0.1.5 | 0.1.5 | 411 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent engine integrations HTTP API: plugin discovery, manifest projection and plugin  |
| UNUSED | sysmem | 0.1.0 | 0.1.0 | 311 |  | - | - | 0 | no app and no used crate depends on it; Safe API over the platform memory calls: return free allocator pages to the OS, read  |
| UNUSED | now-playing | 0.1.0 | 0.1.0 | 310 |  | - | - | 0 | no app and no used crate depends on it; Now-playing and media-keys owner: publishes playback state and receives media-key and |
| UNUSED | passkeys | 0.1.0 | 0.1.0 | 289 |  | - | - | 0 | no app and no used crate depends on it; Passkey authenticator adapter over passkey-rs for Right Suite apps: a CredentialStore |
| UNUSED | decimal | 0.1.0 | 0.1.0 | 242 |  | - | - | 0 | no app and no used crate depends on it; Exact decimal owner: parse a decimal string, canonical form, total ordering and lt/le |
| UNUSED | dictation-engine | 0.2.0 | 0.2.0 | 229 |  | - | - | 0 | no app and no used crate depends on it (nature: HeardRight dictation stack); Dictation engine: the speech engine sidecar binary and library facade over the dictat |
| UNUSED | i18n | 0.1.0 | 0.1.0 | 189 |  | - | - | 0 | no app and no used crate depends on it; Rust runtime for the shared @rightkit/i18n catalogs: locale tag resolution, catalog m |
| UNUSED | content-proof | 0.1.1 | 0.1.1 | 172 |  | - | - | 0 | no app and no used crate depends on it; Exact claim-number reading for content proof review: currency, thousands separators,  |
| UNUSED | contrast | 0.1.0 | 0.1.0 | 97 |  | - | - | 0 | no app and no used crate depends on it; Colour contrast owner: WCAG relative luminance, contrast ratio and readable ink on a  |
| UNUSED | agent-engine-api-sessions | 0.1.1 | 0.1.1 | 42 |  | - | - | 1 | no app and no used crate depends on it (dependents: agent-server are themselves unused) (nature: CodeRight/Legion agent stack); Agent engine session HTTP API routes |
| UNUSED | agent-engine | 0.2.0 | 0.2.0 | 26 |  | - | - | 0 | 26-line facade, no consumer; The agent engine: one facade over the turn loop (core), its protocol and storage, and |
| UNUSED | google-identity | 0.3.0 | 0.2.1 | 19 |  | - | - | 0 | deprecated unpublished alias of rightkit-oauth google; Deprecated, unpublished: re-exports the Google sign-in of rightkit-oauth (feature `go |

Unused crate families (all `UNUSED`, no app and no used crate depends on them), by product area, with src lines: agent-engine / agent-server stack (agent-server 160,151, agent-engine-core 33,295, mobile-runtime 8,406, ...), Legion code audit and evidence (code-audit-engine 130,008, commands 9,840, ...), Pulse context-runtime 62,135, `rightkit-terminal-ui` 42,631, HeardRight dictation stack (dictation-inference 17,234, runtime 15,002, tauri 4,664, app-runtime 2,489, models 2,980, ...: HeardRight keeps `heardright-*` crates on origin/main and has only started moving: work branch adds `dictation-command-host`, `dictation-text-pipeline`), MailRight mail-store/mail-gateway, social-analytics/inbox. Total UNUSED crates: 71 with 693,429 lines (see the table). Of the 71, six are listed only by `e2e-app` (marker `E2E`: align, android, sync-keys, tts-qwen, tts-qwen-sys, vault).

## 3. Deliverable C: the mixed and misplaced ones in detail

### C1. `@rightkit/dictation-ui` 0.2.7 (16,123 lines: 15,893 ts/tsx, 149 css, 47 json, 34 d.ts)

Classes: CAP = capability component that belongs in RightKit; HRL = HeardRight look+composition that belongs in the app; GEN = generic UI that duplicates or should be part of a mandatory package (platform-ui, react-shell, settings-ui, onboarding, license, app-update).

**By src directory**

| src entry | lines | CAP (belongs in RightKit) | HRL (HeardRight look+composition) | GEN (generic; dup of mandatory pkg) |
|---|---|---|---|---|
| host.ts | 495 | 495 | 0 | 0 |
| index.ts | 43 | 43 | 0 | 0 |
| components | 1038 | 717 | 168 | 153 |
| history | 315 | 235 | 80 | 0 |
| hooks | 353 | 310 | 0 | 43 |
| lib | 362 | 155 | 197 | 10 |
| onboarding | 2932 | 412 | 1927 | 593 |
| pages | 1821 | 0 | 1541 | 280 |
| pill | 2762 | 2762 | 0 | 0 |
| settings | 3456 | 1719 | 0 | 1737 |
| shared | 47 | 47 | 0 | 0 |
| shell | 1861 | 0 | 602 | 1259 |
| tauri | 516 | 516 | 0 | 0 |
| types | 88 | 88 | 0 | 0 |
| env.d.ts | 34 | 0 | 0 | 0 |
| TOTAL | 16123 | 7499 | 4515 | 4075 |

What each directory is:
- `components` (1,038): banners (accessibility, hotkey conflict, error), replacements/snippets editors, page frame, transcribe modal, settings group frame.
- `history` (315): delivery-record -> library-row mapping + the History page over `@rightkit/library-ui` `LibraryScreen`.
- `hooks` (353): dictation events, home dashboard, mic devices, settings store hook.
- `lib` (362): chords, push-to-talk, sounds, home metrics, `ui.ts` (127 lines of Tailwind class strings: espresso/coffee surfaces, hero band, "pageContainer max-w-[1320px]").
- `onboarding` (2,932 incl. 15 css): `DictationOnboarding` composes `@rightkit/onboarding` with HeardRight's steps (Welcome, MicSetup, Controls, Commands, Trial, AppLives, RecognitionSetup, model intro), step rules `flow.ts`, `constants.ts` (button class strings, `PRIVACY_LINE`).
- `pages` (1,821): Home 400, Vocabulary 514, Polish 257, Transcribe 297, Dictation 38, polish/ApiKeysPanel 280, PolishCards 35.
- `pill` (2,762): `PillOverlay`, `HintPopup`, `MiniClipboard(Popup)`, animation/geometry/event hooks, `pillStyles.ts` (390 lines of CSS-in-TS, classes `hr-*`).
- `settings` (3,456): mic/speech-model settings (CAP), Apple/iCloud/Google sync, licence, account, device management, developer, application, `Toggle`, `SegmentedControl`.
- `shell` (1,861 incl. 134 css): sidebar status footer ("Markup and classes are HeardRight's", `SidebarStatusFooter.tsx:2`), feature reel, privacy summary, plus the generic pieces named in the README (HoverTooltip, SettingsTabs, settings registry/palette, settingsSync coordinator, LicenseLifecycleDialog, SidebarUpdateStatus, ModelUpdatePrompt, fieldStore, useTauriEvent).
- `tauri` (516): `DictationClient`/`PillClient` native bindings ("exactly the native calls the HeardRight web code makes", `tauri/index.ts:12`).
- `types` (88), `host.ts` (495, the configure seam with `components`, `classNames`, `hooks` overrides "for a host whose pages must look or behave as its own"), `index.ts` (43), `shared/hint-catalog.json` (47, pill hint text with `{app}` placeholder).

**By package export** (`package.json` exports)

| Export | Resolves to | Lines | Mostly |
|---|---|---|---|
| `.` | `src/index.ts` -> host.ts, pages/, components/, history/, hooks/, lib/, types/ | 495 + 43 + 1,821 + 1,038 + 315 + 353 + 362 + 88 = 4,515 | HRL pages and GEN/CAP parts; seam is CAP |
| `./pill` | `src/pill/` (+ `shared/hint-catalog.json`) | 2,762 (+47) | CAP |
| `./onboarding` | `src/onboarding/` | 2,917 | HRL (1,927), GEN (593), CAP (412) |
| `./settings` | `src/settings/` | 3,456 | CAP 1,719, GEN 1,737 |
| `./tauri` | `src/tauri/` | 516 | CAP (HeardRight-bound) |
| `./shell` | `src/shell/` | 1,727 | HRL 602, GEN 1,125 |
| `./onboarding.css` | `onboarding/stepCount.css` | 15 | HRL |
| `./shell.css` | `shell/settings-tabs.css` | 134 | GEN (hr-settings-* classes, 23 tokens) |

**CSS lines**: 149 css (15 + 134) plus 390 lines of CSS-in-TS in `pill/pillStyles.ts`; `lib/ui.ts`, `onboarding/constants.ts` and every page carry Tailwind utility strings (lines containing `className=`: pages 166, onboarding 187, settings 168, shell 87, pill 56, components 66). Hex/rgb literals: 117 occurrences across `src` (not broken down by directory).

**Hard-coded product strings**: `HeardRight`/`heardright` 6 occurrences, all in comments (`types/settings.ts:22`, `shell/SidebarStatusFooter.tsx:2`, `tauri/settingsBackend.ts:10`, `tauri/index.ts:12`, `tauri/dictationClient.ts:240`, `pages/Vocabulary.tsx:3`); the product prefix `hr-` appears 372 times (pill 151+80+36+25+8+5, settings-tabs.css 23, onboarding 12); other product-feature words: "Parakeet" 14, "Apple" 42, "iCloud" 12, "Google Drive" 2, "AI polish" 30, "Whisper" 42. `Pulse` 8 hits are Tailwind `animate-pulse`, not the product. Sentence-like string literals (regex `"[A-Z][a-z]+ [a-z]+ ..."`, undercounts): pages 15, onboarding 41, settings 33, shell 15, pill 17, components 6, history 6 (133). Example: `onboarding/constants.ts` `PRIVACY_LINE = "Your audio never leaves your device. Text goes out only if you turn on AI polish."`.

**What the package does well under the owner's model**: the pill (`./pill`) is brand-neutral over a `PillClient` contract and imports nothing from the pages (`README.md`); product name comes through `hostAppName()`.

**Per file**: `c_dictation_files.md` in the working folder (every file with line count, class and target package). Summary of the GEN targets:

| Generic piece in dictation-ui | Lines | Mandatory owner that has it, or should |
|---|---|---|
| `shell/HoverTooltip` | 113 | `react-shell/react/tooltip.tsx` (wrapper tooltip exists; delegated document-level variant missing) |
| `shell/SettingsTabs` + css, `settings/Toggle`, `SegmentedControl` | 75+134+45+68 | `react-shell` `TabStrip` (`tabs.tsx`), `Toggle:115`, `SegmentedControl:129` |
| `shell/settingsRegistry`, `palette`, `settingsSync`, `fieldStore` | 127+100+191+37 | `settings-ui` `search-core.tsx`, `store.ts` + `react-shell` `CommandPalette`/titlebar search |
| `shell/LicenseLifecycleDialog`, `settings/LicenseSettings`, `onboarding/useTrial` | 70+475+106 | `@rightkit/license` `lifecycle.ts`, `settings-ui` `licenseSection:560`, `onboarding` `licenseStep:178` |
| `settings/AccountSettings`, `DeviceManagementSettings` | 381+296 | missing from `settings-ui`/`license` |
| `shell/SidebarUpdateStatus`, `settings/ApplicationSettings` | 159+103 | `app-update/react`, `settings-ui` `updatesSection:265` |
| `onboarding/usePermissions` | 487 | `onboarding` `permissionsStep:53` |
| `pages/polish/ApiKeysPanel`, `components/ErrorBanner`, `CompactSettingsGroup`, `settings/DeveloperSettings` | 280+81+72+369 | `settings-ui` `apiKeysSection:1238`, `diagnosticsSection:889`/`doctorSection:1028`, `react-shell` `Notice` |
| `hooks/useSettings`, `lib/time`, `shell/useTauriEvent` | 43+10+27 | `settings-ui` `useSettings`, `platform-ui` `time.ts`, `@rightkit/tauri` |

### C2. Other MIXED / APP-LOOK npm packages

Line counts are src lines per top directory (ts/tsx/css), excluding tests.

**`@rightkit/video-ui` 0.2.1 (17,841; CSS 1,588)**. README: root = editing components "holds no app pages"; `./app` = "CutRight's application UI ... desktop workspace and its pages (Projects, Media, Auto-edit, Shorts, Phrases, Retention, Assets, Exports, batch delivery, bundle export, recipe library, prompt panel, progress, Settings), the onboarding and legal gate, agent demo mode" (`README.md:8-10`); `./tauri` is the Tauri host.
| dir | lines | class |
|---|---|---|
| `src/app` (views 25 files, components/DesktopWorkspace, demo, legal, lib, ui, app.css) | 10,035 | APP-LOOK: CutRight screens, workspace composition, onboarding/legal gate wiring, a CutRight->GenRight bridge (`app/lib/genright.ts`, 52 `genright` mentions) |
| `src/editor` | 5,066 | capability (timeline editor, vendored hyperframes pieces) |
| `src/lib` + `src/components` + `src/hooks` + `src/ui` + `host.ts` + `tauri` | 1,557 + 624 + 98 + 59 + 166 + 123 | capability (inspectors, host contract) |
Product names: 71 (cutright 17, genright 52, pulse 2). Used by CutRight only; CutRight app is 681 lines.

**`@rightkit/mail-ui` 0.2.1 (19,080; CSS 4,852)**: "Mail client UI: inbox, reader, compose editor, recipients, attachments, account switcher and the mail store".
| dir | lines | class |
|---|---|---|
| `src/styles` (17 css files `index-01..17.css`, 52 hex colours) | 4,846 | APP-LOOK: MailRight styling |
| `src/views` (Inbox*, Reader*, Compose, Sidebar, banners, toolbar) | 3,197 | APP-LOOK: MailRight screens and arrangement |
| `src/lib` (ipc, search routing/coverage, preferences, shortcuts, sound, theme, legal) | 5,419 | capability (mail client logic), with MailRight `ipc` and `legal.ts` |
| `src/hooks` + `src/store` | 2,038 + 1,739 | capability |
| `src/components` (EmailFrame, ComposeEditor, RecipientField, FindBar, attachment preview) | 1,639 | capability components |
| `workspaceHost.ts`, `workspaceViews.tsx` | 175 | MailRight wiring of `@rightkit/workspace-ui` ("MailRight's host", `workspaceHost.ts:1`) |
Names: 34 mentions (`mailright-img://` protocol, `data-mailright-image-blocked` in `EmailFrame.tsx:48-56`). Used by MailRight only; MailRight `src` = 4,412 lines. Dependents: `agent-ui-approvals` imports `mail-ui/lib/ipc`, `lib/ipc/agentApprovals`, `lib/visibleText`, `hooks/dialog`, `store/index` (11 import lines).

**`@rightkit/document-workspace` 0.2.1 (37,281; CSS 2,209)**: "Multi-format document workspace shell a host configures: tabs, sidebar, document router ... settings and the PDF editor pane."
| dir | lines | class |
|---|---|---|
| `src/app` (AppShell, Sidebar, Toolbar, CommandPalette, ShortcutHelp, FindBar, ToastViewport, tabs/, sidebar/, ~50 `use*` hooks) | 17,214 | APP-LOOK: ViewRight shell composition; overlaps react-shell `CommandPalette`/`ShortcutHelp`/`TabStrip`/toasts |
| `src/documents` (router; html, markdown, mdx, pdf, txt, web) | 16,094 | capability (viewers) |
| `src/styles` (tabs, explorer, index) | 2,166 | APP-LOOK (44 hex; `bg-copper`; "visual spec docs/design/viewright-tabs-explorer.html") |
| `src/settings` | 1,299 | GEN: duplicates `settings-ui` panel |
| `src/state` | 412 | capability |
Used by no app (ViewRight default branch keeps its own 88,032-line `src`; the mapping `viewright-rightkit-mapping.md:29-30` plans it).

**`@rightkit/document-core` 0.2.0 (5,974)**: `documents` 1,976 + `state` 1,113 + `shared` 91 capability; `app` 1,780 and `settings` 864 composition (GEN for settings); depended on by the three editors.

**`@rightkit/library-ui` 0.2.3 (11,604; CSS 3,311)**:
| dir | lines | class |
|---|---|---|
| `src/list`, `src/reader`, `contract.ts`, `host.ts`, `ui.ts` | 2,375 + 1,461 + 154 + 45 + 35 | SHARED capability (HeardRight History as list reference, ScrapeRight Detail as reader reference; `src/index.ts:2-4`) |
| `src/queue` (components, views, hooks, lib/ipc, contract, queue.css) | 7,385 | APP-LOOK: ScrapeRight queue screens that call ScrapeRight's Tauri commands ("until a queue UI package exists") |
Used by HeardRight (work) through dictation-ui only.

**`@rightkit/social-ui` 0.2.1 (17,418)**: `src/screens` 7,032 (20 PostRight screens: Accounts, Drafts, Approvals, Analytics, Calendar, Campaigns, Inbox, ...) APP-LOOK; `src/data/engine` 3,949 + `types.ts` 1,302 + `source.ts`, `derive`, `draftRules`, `money`, `platforms`, `time` ~600 capability (typed engine client); `src/data/mock` 4,196 fixtures (files start "RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch", a machine-local path; 31 `postright` mentions). Used by no app (PostRight keeps 18,517 lines).

**`@rightkit/media-gen-ui` 0.2.1 (21,200; CSS 705)**: classed `UNUSED` (nature: GenRight capability components). `src/views` 13,508 are panels (generate form, jobs, gallery, voices, models, rentals, recipes, production), `src/lib` 7,045 engine client and builders, `components` 608. README: "The package holds panels, not an app. The window, navigation, onboarding, settings screens ... are the host's"; CHANGELOG:9 removed the app shell. This is the shape the owner's model wants, but no app consumes it (GenRight keeps 28,245 lines). Product mentions: 8 (`production-stack-builders.ts:223,240,531,591` `genright_campaign_stack`).

**`@rightkit/agent-ui-*` (7 packages, ~103,400 lines)**: CodeRight's UI split for the work branch.
| package | lines | split |
|---|---|---|
| agent-ui-core | 28,917 (css 626) | `lib` 23,341 (daemon client, session/transcript, stores, providers) + `state` 5,822 + `hooks` 1,394 capability; `index.css` 495 + `styles/dark-theme.generated.css` + `styles/rightkit-shell-tokens.css` 134 + `components` 1,063 CodeRight look; CodeRight strings `coderight.bootProbation`, `exclusive_coderight_gateway` (`lib/cliProviders.ts:75`), 40 mentions; hard deps on `onboarding legal legal-ui license logs tauri-shell theme` |
| agent-ui-components | 31,361 | `components/app` 5,239 (AppFrame, AppBody 1,277, ComposerIsland 1,974, LegalGate, LegalAcceptance, OnboardingPanel 192, title-bar portal: CodeRight frame/onboarding composition, APP-LOOK) + root `TitleBar`/`Sidebar` + `components/settings` 5,478 (CodeRight settings panes) ; `chat composer review panels delivery browser bots environments usage` ~7,000 capability; `hooks` 4,718 |
| agent-ui-hooks 10,368, agent-ui-workbench 12,984 | | APP-CAPABILITY (hooks, workbench/runtime panels) |
| agent-ui-settings | 6,837 | APP-LOOK: settings panes composed over settings-ui |
| agent-ui-approvals | 9,791 (css 203) | approvals queue 1,094 capability; `remote` 8,665 = "Pulse's device link, remote approvals, remote unlock, browser logins, secure box and notch settings over the Tauri commands of Pulse's hub" (`remote/api.ts:1-2`, 162 `pulse` mentions) + MailRight's agents section (`SettingsAgentsSection.tsx:1`); depends on `mail-ui` and `system-ui` |
| agent-ui-web | 3,161 (css 865) | APP-LOOK: browser client with CodeRight copy (`App.tsx:229-232`), service worker `coderight-shell-v1` (`app/public/sw.js:7`) |
All used by CodeRight's work branch only, except `agent-ui-approvals` (MailRight, both branches).

**`@rightkit/system-ui` 0.2.1 (5,810; CSS 485)**: `views` 4,270 (Storage, Cleanup, Duplicates, Drive health, Apps, Activity: Pulse screens) APP-LOOK; `api.ts` 750 + `components/Chart`, `metrics`, `visible`, `activityLease`, `tauri/` ~1,500 capability. Used only through `agent-ui-approvals`.
**`@rightkit/workspace-ui` 0.2.1 (3,134)**: `views` 2,359 (calendar, contacts, drive, tasks, meeting) APP-LOOK (MailRight); `client.ts` 213, `hooks` 165, `host.tsx` 152, `lib` 151 capability. Used by `mail-ui` only.
**`@rightkit/send-ui` (742)**, `terminal-ui` (1,045), `waitlist-worker` (234), `blender-worker`: single-purpose capability, see table.

### C3. Rust crates named for a product area

| Crate | Lines | What it is | Verdict |
|---|---|---|---|
| `rightkit-dictation-tauri` 0.2.1 | 4,664 (runtime 3,494, host.rs 398) | Recording state machine over the engine sidecar, history sequencing, watchdog, runtime lock; everything app/platform is behind `host::DictationHost` (README). Moved from HeardRight `src-tauri/runtime/*` | Capability; brand-neutral seam. `UNUSED`: no app depends on it (HeardRight default branch keeps `heardright-*` crates; its work branch has not adopted it) |
| `rightkit-dictation-app-runtime` 0.2.0 | 2,489 (`engine_supervisor` 1,916) | Shell-side engine supervisor (spawn, launch frame, health, restart, sidecar logs) on `rightkit-job-queue` `supervisor`. Contains `HR_*`/`HEARDRIGHT_*` env handling (`engine_supervisor/supervisor.rs:250`) | Capability; only dependent is `dictation-tauri`; overlaps M3 (job-queue supervisor + child-process) |
| `rightkit-dictation-(core, command-host, text-pipeline) + voice-commands, text-polish, transcript-library` | 3,156; 5,929; 4,532; 1,063; 3,349; 1,526 | dictation engine pieces | APP-CAPABILITY (HeardRight uses core on both branches, the others on the work branch) |
| `rightkit-dictation-(inference, runtime, models, engine, ffi, telemetry)` | 17,234; 15,002; 2,980; 229; 790; 1,398 | engine sidecar, inference, model store | `UNUSED` (only used by the unused chain) |
| `rightkit-prompt-recipes` 0.2.0 | 11,540 + 484 KB `data/` | GenRight studio recipes, image workflow catalogue, character/LoRA lock data, video shot templates; "Brands are the host's ... The KDP workflow and the brand list stayed with the app" (README). Data `source` fields cite `docs/plans/2026-10-08-genright-execution-plan.md` (provenance only; 30 code/data hits, no brand list) | GenRight capability; `UNUSED` (no app/crate depends on it); not APP-LOOK |
| `rightkit-terminal-ui` 0.1.3 | 42,631 (transcript 13,011, composer 11,586, render 5,700, terminal 5,060) | ratatui chat transcript/composer/approvals/diffs for agent sessions | CodeRight terminal UI components; only dependent `agent-server` (unused). Whether any of `render`/`transcript` fixes a CodeRight look was not inspected: `UNCLEAR` for look vs capability, class by use `UNUSED` |
| `rightkit-agent-server` 0.2.0 | 160,151 | daemon API, CLI and interactive-session runtime; "The library holds no product's names. The app passes a `HostIdentity`" (README); contains `tui_host.rs` 10,052 and `main_runtime_interactive_session.rs` 7,211 | Capability, brand-neutral by design; `UNUSED` (registry has 0.1.1, repo 0.2.0) |
| `rightkit-notch-ui` 0.2.1 | 6,419 | Pulse's notch drawing core (geometry, spring motion, hover card, software renderer) | APP-CAPABILITY (Pulse, work branch only): the owner's example |
| `rightkit-launcher-win` 0.1.0 | 7,963 (`panel.rs` 5,395) | the Windows launcher panel window (apps, files, notes, snippets) | Pulse launcher screen as a crate; `UNUSED`; app-look share not inspected |
| `rightkit-agent-demo` 0.2.0 | 3,288 | consented demo mode: agent drives one catalog app | `UNUSED`, cross-app (depends on `record` (CutRight family)) |

## 4. Deliverable D: layering violations

### D1. MANDATORY or SHARED depending on APP-* (by use)
By construction a dependee of a used shared crate is at least as widely carried, so by the use-based classes there are **0** such edges among crates and npm packages. The violations are visible by product area (nature) instead: edges where a core/shared crate depends on a crate belonging to one product's stack (all 36 cross-area, non-dev edges are in `layering.json`; the ones involving core crates):

| From (class) | To (area) | Kind |
|---|---|---|
| `rightkit-llm` (SHARED; HR work, PU, CR, MR) | `agent-engine-protocol`, `agent-policy`, `agent-config` (CodeRight/agent stack) | optional |
| `rightkit-mcp` (SHARED; PU, CR) | `agent-engine-protocol`, `agent-engine-runtime-resources`, `agent-config` | optional |
| `rightkit-scheduler` (CodeRight stack, used by agent-server) | `agent-engine-protocol` | hard |
| `rightkit-ffmpeg` (SHARED, 6 apps) | `agent-policy` | optional |
| `rightkit-hooks` (PU) | `agent-engine-protocol` | optional |
| `rightkit-review` (CutRight, PostRight) | `evidence-contracts`, `code-audit-provider-sdk` (Legion) | optional |
| `rightkit-media-gen` (GenRight) | `compose` (CutRight) | hard |
| `rightkit-media-pipeline` (GenRight) | `motion` (CutRight) | optional |
| `rightkit-agent-orchestration` (CodeRight) | `evidence-contracts`, `code-audit-provider-sdk` (Legion), `repo-graph` (Pulse) | hard / optional |
| `rightkit-agent-server` | `memory-client`, `context-protocol` (Pulse), `github`, `evidence` (Legion) | hard |
| `rightkit-remote-gateway` (Pulse) | `agent-gateway` (MailRight) | hard |
| `rightkit-adapt` (Pulse) | `agent-transcript` (agent stack) | hard |

Mandatory -> non-mandatory: `rightkit-tauri-shell` depends on 7 crates unlisted as owners (autostart, clipboard, deep-link, instance, presence, notifications, signal; hard) + 4 optional (overlay, pdf, models, app-update); `rightkit-agent-tools` -> `automation browser speech-client mcp` (optional); `rightkit-qa` -> `browser`, `automation`, `roots`; `rightkit-crash`, `credentials`, `logs` -> `rightkit-crypto`.

npm layering facts:
- `dictation-ui` (CAP+HRL, MIXED) -> `library-ui` (MIXED) -> `doc-render` (SHARED), `media-ui` (SHARED), `tauri-shell`, `lucide-react`; plus declared but never imported: `@rightkit/legal-ui` and `@tauri-apps/plugin-dialog` (0 import sites in `src`).
- `terminal-ui` (1,045-line terminal pane, APP-CAPABILITY) -> `agent-ui-core` (28,917 lines of CodeRight core).
- `agent-ui-approvals` (CodeRight/Pulse/MailRight UI) -> `mail-ui` (19,080) and `system-ui` (5,810): 11 import lines from `mail-ui` subpaths and 23 from `system-ui` (`api`, `components/Chart`, `metrics`, `visible`, `activityLease`, `views/Storage`).
- `mail-ui` -> `workspace-ui`; editors (`markdown-editor`, `html-editor`, `docx-editor`) -> `document-core` (MIXED).
- `legal` has a peer dependency on `release` (both MANDATORY, fine).

### D2. `dictation-ui` -> `library-ui` -> `doc-render`, exact symbols
`dictation-ui` imports from `@rightkit/library-ui` (main entry): `LibraryScreen`, `friendlyAppName`, `OriginIcon`, `rowActionChip`, `useGroupSelection` and types `LibrarySource`, `LibraryRow`, `LibraryGroup`, `LibraryLabels`, `LibraryOrigin`, `LibraryQuery`, `LibraryVariant`, `LibraryComponents`, `LibraryButtonProps`, `LibraryOriginFilter`; plus `configureLibraryHost` from `@rightkit/library-ui/host` (6 files: `history/DictationHistory.tsx:2-8`, `deliveryRows.ts:9`, `useHistorySourceSelection.ts:3`, `host.ts:9,11`, `tauri/dictationClient.ts:4`, `pages/Home.tsx:18`). That is the list/rows part (`src/list` 2,375 lines). The main entry of `library-ui` also re-exports the reader (`src/index.ts:17-30`: `LibraryReader`, `ReaderChrome`, `CarouselLayout`, `FindBar`, `MediaPlayer`, `TranscriptBody`, ...), whose `reader/ReaderParts.tsx:2` imports `MarkdownRenderer` from `@rightkit/doc-render` (deps: unified, remark-parse/gfm/math/frontmatter/smartypants, shiki 4.5, parse5, dompurify) and `MediaPlayer` from `@rightkit/media-ui` (`ReaderParts.tsx:3`; the player is 871 lines); `library-ui/src/styles.css:4` `@import "@rightkit/doc-render/styles.css"`; `library-ui` also depends on `@rightkit/tauri-shell`, `@phosphor-icons/react` and `lucide-react` (the latter is imported only under `src/queue/`), and its `./queue` export (7,385 lines of ScrapeRight screens) ships in the same package. Used for a list view of delivery history.

### D3. Product names in MANDATORY/SHARED source or data
Totals (lines mentioning HeardRight, CodeRight, GenRight, ViewRight, MailRight, ScrapeRight, CutRight, PostRight, Pulse, Legion, case-insensitive including lowercase ids; `Pulse` includes some English/CSS `pulse` hits): MANDATORY 1,478 (963 code/data + 515 comment), SHARED 632 (194 + 438). Complete file:line list: `product_names_mandatory_shared.txt` (2,110 lines). Largest, with real source separated from artefacts:

| Package/crate | class | code/data | comment | where |
|---|---|---|---|---|
| `@rightkit/legal-ui` | MANDATORY | 647 | 7 | 600 of them in `.preview/verification/axe-report.json` and 22 in `preview/variants.ts` (artefacts); `src/LegalGate.tsx:216,262` comments |
| `@rightkit/release` | MANDATORY | 192 | 168 | `legion-semantic-contracts.mjs` (39), `suite-api.generated.mjs` (35), `ownership-mandatory-set.json` (18), `app-check.mjs` (18), `bin/rightkit-gate.mjs` (18), `deterministic-scans.mjs` (12) |
| `@rightkit/app-update` | MANDATORY | 35 | 2 | `src/suite-api.generated.ts` (35: per-app artifact host table) |
| `rightkit-license` | MANDATORY | 27 | 25 | `src/suite_api_generated.rs:17-20...` (per-app artifact host table) |
| `@rightkit/tauri` | MANDATORY | 26 | 1 | `src/index.ts:41-45` exports `createHeardRightTauriClient`, `createCodeRightTauriClient`, `createMailRightTauriClient`, `createScrapeRightTauriClient`; `src/deprecated-app-clients.ts` (11) |
| `rightkit-qa` | MANDATORY | 21 | 59 | `src/engine.rs:813` GenRight legacy frames, `src/media.rs:98` `GENRIGHT_FFPROBE`, `src/harness.rs:839`, `examples/.../rightkit-qa.toml:11` `app = "postright"` |
| `rightkit-tauri-shell` | MANDATORY | 5 | 71 | mostly comments (ViewRight 34, HeardRight 19, CodeRight 10) |
| `rightkit-daemon` | MANDATORY | 4 | 48 | comments |
| `rightkit-speech-client` | SHARED | 47 | 28 | `src/capture.rs:27,40,48` `HeardRightDictation` enum + wire string `heard_right_dictation`; `Cargo.toml:6` |
| `rightkit-llm` | SHARED | 27 | 13 | `src/catalogue.rs:47` `Genright` variant; `src/agent_providers/transport.rs:82,106` `coderight.provider-context.v1` schema ids; `assets/price-catalogue.json:5` |
| `rightkit-models` | SHARED | 23 | 45 | `src/rightapps.rs:17-20` app-name list (viewright, heardright, mailright, scraperight ...) |
| `rightkit-repo-graph` | SHARED | 15 | 3 | CodeRight/Pulse names |
| `rightkit-contracts` | SHARED | 14 | 1 | cutright 10, postright 4 |
| `rightkit-ffmpeg` | SHARED | 13 | 23 | genright 15, cutright 10, heardright 6 |
| `rightkit-timeline` | SHARED | 11 | 7 | cutright 18 |
| `rightkit-agent-policy` | SHARED | 10 | 10 | legion 16, coderight 4 |

Per class (all packages, for scale): APP-CAPABILITY 801, MIXED 378, UNUSED 738, TOOLING 79, APP-LOOK 14 matching lines.

## 5. Deliverable E: gaps in the mandatory base

Evidence is from default branches: HeardRight `origin/main` `tauri-app-next/src` (22,265 lines, 0 imports of dictation-ui) used `@rightkit/react-shell` `AppShell`, `Titlebar`, `PageShell`, `CommandPalette`, `ErrorBoundary`, `Table`, `createThemeStore` and kept the files below. Sizes are HeardRight origin/main unless stated.

| Feature | Where the app-specific implementation lives | What the mandatory package would need |
|---|---|---|
| Settings search registry feeding the command palette; reveal/focus of a row after choosing it | HR `lib/settingsRegistry.ts` 637, `hooks/usePaletteCommands.ts` 69, `usePaletteFocusRestore.ts` 36; copied generic into `dictation-ui/shell/settingsRegistry+palette` (227) | `settings-ui` already has in-panel search (`search-core.tsx`); needs a registry-to-`react-shell` `CommandPalette` bridge and `revealSetting`/focus-restore |
| Settings page as Chrome-style tabs | HR `components/settings/SettingsTabs.tsx` 76 + `settings-tabs.css` 134 (also `dictation-ui/shell` 75+134) | `react-shell` `TabStrip` exists (288 lines) but the settings tab strip with active tab joining the panel is not in `settings-ui` |
| Document-level hover/focus tooltip for icon-only buttons | HR `components/HoverTooltip.tsx` 113 (dictation-ui 113) | `react-shell` `Tooltip` (88) is wrapper-based; a delegated variant is missing |
| Licence lifecycle dialog (expired trial, lapsed, etc.) | HR `LicenseLifecycleDialog.tsx` 70, `lib/licenseLifecycle.ts` 297, `lib/license/core.ts` 247, `accountSession.ts` 153, `errors.ts` 87, `proBridge.ts` 49 (the package has `proBridge.ts` 59 and `lifecycle.ts` 364) | `@rightkit/license` has the state machine, not the dialog component; HR kept near-duplicates of `proBridge` and lifecycle on the default branch |
| Account sign-in/out/delete and leased-device list | `dictation-ui/settings/AccountSettings` 381, `DeviceManagementSettings` 296; HR `components/settings/*` | `settings-ui` `licenseSection` has only "Deactivate this device"; needs account and device sections (`onboarding` has `signInStep`, `adapters.ts:121` OAuth) |
| Sidebar update status line and update preference | HR `SidebarUpdateStatus.tsx` 159, `lib/autoUpdates.ts` 246, `relaunchForUpdate.ts` 27, `heardRightAutoUpdateClient.ts` 40 | `app-update/react` has overlay and consent card only; `settings-ui` `updatesSection` covers the settings side |
| Sidebar status rows (plan, AI polish, sync) | HR `Sidebar.tsx` 184 (nav data + footer) | `react-shell` `statusFooter` slot exists; rows are app composition (stay in app); `StatusRow` already in `react-shell/ui.tsx:727` |
| Speech-model update dialog | HR `components/model-update/ModelUpdatePrompt.tsx` 225 + `lib/modelUpdate.ts` 133 | `settings-ui` `modelsSection`/`onboarding` `modelsStep`; no update prompt |
| Optimistic settings coordinator and cross-window settings broadcast | `dictation-ui/shell/settingsSync.ts` 191 | `settings-ui` `store.ts` `createSettingsStore` (267 lines) |
| Safe native event subscription hook | HR `hooks/useTauriEvent.ts`, `dictation-ui/shell/useTauriEvent.ts` 27 | `@rightkit/tauri` (client/window/dialog only) |
| Title-bar search shared with a page | `dictation-ui/shell/fieldStore.ts` 37 | `react-shell` `searchMode`/titlebar slots |
| Microphone/accessibility permission polling for setup | `dictation-ui/onboarding/usePermissions.ts` 487 | `onboarding` `permissionsStep` (`steps.tsx:53`, backend interface only) |
| Document/tabs/explorer frame (ViewRight) | VR `src/app/AppShell,Sidebar,CommandPalette,ShortcutHelp,ToastViewport,Toolbar` 2,291 + `sidebar/`, `tabs/` 4,128; mirrored in `document-workspace/src/app` 17,214 | `react-shell` `AppShell` is nav-group sidebar; VR uses `AppFrame`+`Plane`; this is arrangement, so it stays app-side; generic pieces (`CommandPalette`, `ShortcutHelp`, toasts, `TabStrip`) exist in `react-shell` and VR still keeps copies (40 files import `react-shell`; the single-line named imports show `CommandPalette`, `ErrorBoundary`, `Titlebar`, `AppFrame`, `Plane`, not `AppShell`; multi-line imports were not enumerated) |
| IDE frame (CodeRight): title bar, sidebar, palette, frame, legal gate, onboarding panel | CR `components/Sidebar,TitleBar,CommandPalette,app/AppFrame,app/LegalGate (37 lines, 0 RightKit imports),app/OnboardingPanel (191, 0 RightKit imports),onboarding/` 1,871; now in `agent-ui-components/components/app` | CodeRight/MailRight each keep a 37-line `LegalGate.tsx` wrapper with no `@rightkit/legal-ui` import (`mailright src/components/LegalGate.tsx`, `coderight .../app/LegalGate.tsx`): `legal-ui` `LegalGate` (698) is not wired there |
| Engine supervision for a dictation/engine sidecar | `dictation-app-runtime/engine_supervisor` 1,916; ScrapeRight `engine_supervisor.rs` (plan AP-SR-01) | `job-queue` `supervisor` feature exists; not checked what remains app-specific (not inspected) |
| Agent host surface | MailRight `src-tauri/src/agent/gate.rs + service.rs`, GenRight `src-tauri/src/agent_tools.rs`, Pulse `engine/src/journal/agent_act.rs`, CutRight `apps/cutright/src-tauri/src/demo/host.rs + service.rs` | `rightkit-agent-tools` is client-side; hosts compose `rightkit-daemon` + approvals themselves |

Other apps on their default branches (GenRight 28,245 UI lines, ScrapeRight 15,824, PostRight 18,517, ViewRight 88,032) each still keep a full UI; `onboarding` is consumed by all nine apps in a thin way (apps keep their own 200-560 lines of onboarding composition, which is app-side by the owner's model).

## 6. Deliverable F: what would have to change (most important first)

| # | Change | Size | Apps touched |
|---|---|---|---|
| 1 | Split `@rightkit/dictation-ui`: keep `./pill` (2,762 lines + hint catalog 47, rename `hr-*` prefix, 372 tokens) and the capability parts (host seam, tauri bindings, hooks, types, history mapping, replacements/snippets, mic/speech settings, ~7,500 lines); move HeardRight look/composition (pages 1,541, onboarding flow 1,927, shell footer/feature reel/privacy 602, `lib/ui.ts` 127, ~4,500 lines) back to the app; move the generic ~4,075 lines into `react-shell`, `settings-ui`, `license`, `app-update`, `onboarding` (list in C1) | about 60 files; 7.5k stay, 4.5k leave, 4.1k relocate | HeardRight (work branch has 70 imports to rewire, `src` 4,872 -> about 9-10k) |
| 2 | Re-do the same split for `video-ui/app` (10,035), `mail-ui` (`views` 3,197 + `styles` 4,846), `document-workspace` (`app` 17,214 + `styles` 2,166 + `settings` 1,299), `library-ui/queue` (7,385), `social-ui/screens` (7,032), `system-ui/views` (4,270), `workspace-ui/views` (2,359), `agent-ui-components/components/app` + `settings` (about 10,700), `agent-ui-settings` (6,837), `agent-ui-web` (3,161), `agent-ui-approvals/remote` (8,665) | about 89,000 lines, several hundred files | CutRight (681-line app depends on it), MailRight, CodeRight, ViewRight, PostRight, ScrapeRight, Pulse |
| 3 | Add the missing generic parts to the mandatory owners (E): palette/registry bridge, settings tab page, delegated tooltip, licence lifecycle dialog, account + device sections, sidebar update line, model update prompt, `useTauriEvent`, permissions backend | up to about 3,000 lines (shell GEN 1,259 + settings GEN 1,737 in dictation-ui; much of it replaceable by components the owners already have) | all nine (consumers); HeardRight first |
| 4 | Make mandatory packages product-neutral: `@rightkit/tauri` per-app clients, `rightkit-speech-client` `HeardRightDictation`, `rightkit-llm` `Genright`, `rightkit-models` app list, `rightkit-qa` GenRight frames, generated per-app host tables in `app-update`/`rightkit-license` (data-driven from the release config), plus remove `.preview` artefacts from `legal-ui` | about 20 files, ~400 lines of code/data | all apps (data-driven tables), HR, GR, CR |
| 5 | Fix the mandatory set: list the 7 hard dependencies of `rightkit-tauri-shell` and `rightkit-app-update`/`rightkit-roots` as owners; refresh stale `min`/`registryMax`; give M21/M23/M24/M25 owners (`@rightkit/issues-tracker` is published); require every app to carry an adoption map (6 of 9 have none) | 1 JSON + 9 app JSON files | all |
| 6 | Resolve cross-product coupling in lower packages: `agent-ui-approvals` -> `mail-ui`/`system-ui`; `terminal-ui` -> `agent-ui-core`; `rightkit-llm`/`mcp`/`ffmpeg` optional deps on the agent stack; `rightkit-media-gen` -> `compose` | about 50 import sites in npm, 12 Cargo edges | CodeRight, MailRight, Pulse, GenRight |
| 7 | Decide the consumers of `UNUSED` capability: 71 crates (693k lines) and 6 npm packages (66.8k) have no consumer; `dictation-tauri`/`app-runtime` (HR), `media-gen-ui` (GR), `document-workspace` + editors (VR), `social-ui` (PR), `prompt-recipes` (GR) are capability that belongs in RightKit but are waiting for app adoption | adoption work in each app: HeardRight `heardright-*` crates, ViewRight 88k UI, GenRight 28k UI, PostRight 18.5k UI | HR, VR, GR, PR, SR |
| 8 | Correct the documents (section 9) so they stop commissioning "single-app code moves wholesale" | about 8 docs | none (docs) |

## 7. What I could not inspect (marked `not inspected`)

- Line-by-line overlap between `dictation-ui/settings/speech/*` and `settings-ui` `modelsSection`, and between `LicenseSettings`/`DeveloperSettings` and the settings-ui sections: classed GEN by name/purpose, not diffed.
- `rightkit-terminal-ui` look vs capability, `rightkit-launcher-win` app-look share, `rightkit-agent-server` CLI/TUI composition (`tui_host.rs`): sized, not read.
- Remaining ~190 crates were classified from manifest description, dependency graph and app usage, not by reading source; only the product-area crates in C3 and the product-name grep were checked at file level.
- Engine supervision remainders (M3/M4) in HeardRight/ScrapeRight.
- Swift packages, Android and iOS code (out of scope).
- App-side UI in ViewRight, GenRight, ScrapeRight and PostRight beyond line counts and imports.
- Work-branch rows: I did not build or run anything; version differences come from manifests only.

## 8. Notes on the data

- Ember (in `defaultApps`) has no checkout here.
- The set JSON `source.versions` says "registry readback 2026-10-19"; all `registryMax` there are stale against section A1.

## 9. Documents that state a different model (findings; they conflict with the owner's model)

| Where | Says |
|---|---|
| `docs/audits/2026-11-01-rightkit-package-master-plan/DECISIONS-ADDENDUM.md:8` | "Port commission (Adrian, 2026-11-01): every capability in every app moves into RightKit packages; each app keeps only a thin shell (<=2%), declared exceptions, recipes, and journey/e2e tests." |
| `docs/audits/2026-11-01-rightkit-package-master-plan/PORT-EXECUTION.md:3` | same commission ("at most 2%") |
| `docs/audits/2026-11-01-rightkit-package-master-plan.md:25,27` | table total of 0.5% kept in apps; "Every capability goes to RightKit, single-app ones included. The app keeps only its look and composition of the shared shell" (this sentence matches the owner's model, the 0.5% figure and the per-app tables below do not) |
| `docs/audits/2026-11-01-rightkit-package-master-plan/per-app/heardright-rightkit-mapping.md:31-34,46` | HeardRight `components/`, `pill/`, `pages/` (dictation home, polish, vocabulary) and "onboarding pages" -> `@rightkit/dictation-ui` NEW / `@rightkit/onboarding` EXTEND |
| `docs/audits/2026-11-01-rightkit-package-master-plan/design_draft.md:17-26` | goal 2 matches the owner's model ("its look and the way it composes the shared shell") while goal 1 says every capability "including ... only one app uses today" becomes a public package |
| App inventories `docs/rightkit-inventory/*-rightkit-mapping.md` in the app repos: `viewright:29-30` (`@rightkit/document-workspace` NEW, "explorer/library sidebar, command palette"), `genright:28` (`@rightkit/media-gen-ui` NEW "generation studio UI"), `postright:27-29` (`@rightkit/social-ui` "content planning screens"), `mailright:31` (`@rightkit/mail-ui`), `coderight:28-29` (`@rightkit/agent-ui` 666 files / 113,422 lines, "remote web client") | whole UIs mapped into RightKit |
| `docs/plans/2026-11-11-linux-handoff.md:47` | "HeardRight. Qualify stops on 38 ownership findings: 30 capabilities HeardRight still implements itself. Adrian: no exceptions, the code moves into RightKit." |
| `docs/audits/2026-11-01-rightkit-package-master-plan/consolidated_review_report.md:36` | "Shell target stated as at most 10% of code. All apps meet it (max 7%)" |
| `packages/video-ui/CHANGELOG.md:5,8` and `README.md:8` | "The owner's decision on 0.2.0's redraw: CutRight's UI belongs in RightKit, not in the app ... Restored CutRight's application UI as an app-level entry" |
| `packages/dictation-ui/CHANGELOG.md:9` | `/shell` added "so an app can delete its own copy. Markup, classes, wording, native calls and call order equal HeardRight's shipped sidebar footer." |
| `docs/audits/2026-11-11-shape-matrix/reports/review-brief.md:7` | "Each app is meant to move the majority of its code into RightKit crates ... keeping only a t[hin ...]" |
| Consistent with the owner's model: `packages/media-gen-ui/CHANGELOG.md:9` ("the package is the media-generation panels, not the GenRight app"; shell, onboarding, settings stay in the app) and `packages/media-gen-ui/README.md` ("holds panels, not an app") | |
