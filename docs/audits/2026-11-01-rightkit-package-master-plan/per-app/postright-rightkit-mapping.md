# PostRight: RightKit mapping

Repo `postright` at `e4f0f3d` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`postright-rightkit-files.json`](postright-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 116 | 57,710 |
| EXTEND | existing RightKit package gains this | 49 | 15,658 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 17 | 1,372 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 268 | 72,363 |
| DROP | delete | 1 | 468 |
| SHELL | thin app composition that stays in app | 6 | 274 |
| CONFIG | app manifest/config that stays | 3 | 97 |
| **Total** | | **460** | **147,942** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **75,208 lines**. Staying in the app as product code: **0 lines**; as shell/config: **371 lines** (0% of code), plus **72,363 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared toolchain/lint/vite presets | 4 | 101 | `./`, `.cargo/` |
| `@rightkit/social-ui` | npm | NEW | TS client & types for social/content engine commands (generate from Rust) | 10 | 5,688 | `src/data/`, `src/data/engine/` |
| `@rightkit/social-ui` | npm | NEW | typed mock data source for UI QA | 19 | 4,186 | `src/data/mock/` |
| `@rightkit/social-ui` | npm | NEW | content planning screens | 11 | 3,821 | `src/screens/` |
| `@rightkit/social-ui` | npm | NEW | accounts, publish jobs, pacing, failures, inbox, DM threads screens | 6 | 2,073 | `src/screens/` |
| `@rightkit/social-ui` | npm | NEW | analytics, campaigns & links screens | 3 | 757 | `src/screens/` |
| `@rightkit/social-ui` | npm | NEW | shared labels & scope select | 3 | 509 | `src/components/`, `src/screens/` |
| `rightkit-content-plan` | crate | NEW | typed briefs, platform variants, hook tests, manual drafts | 6 | 3,127 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-content-plan` | crate | NEW | brief & calendar entry cache, proposals | 2 | 1,472 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-content-proof` | crate | NEW | acquisition chain gates & claim/proof semantics | 4 | 3,681 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-content-proof` | crate | NEW | engine data assets (catalogues) | 3 | 139 | `crates/engine/assets/review/` |
| `rightkit-doctor` | crate | NEW | capability health probes behind doctor command | 1 | 338 | `crates/engine/src/` |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 3 | 703 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-library` | crate | NEW | reusable media records with occurrence ids & expiry | 2 | 1,836 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-pipeline` | crate | NEW | dispatch of content briefs to another engine as external process | 2 | 2,521 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-research` | crate | NEW | dated research digests & source readers (Reddit) | 4 | 1,572 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-social-analytics` | crate | NEW | analytics pull/store, manual metric import, calendar optimisation proposals | 6 | 3,806 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-social-analytics` | crate | NEW | campaigns, tracked short links & visit records | 2 | 1,380 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-social-inbox` | crate | NEW | signed webhook inbox & keyword-comment DM automation | 4 | 3,070 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-social-publish` | crate | NEW | durable publish job phase machine + platform adapters (X, Bluesky, Mastodon, Threads, Instagram, Facebook, LinkedIn, Pinterest, TikTok, YouTube) | 15 | 13,347 | `crates/engine/src/publish/` |
| `rightkit-social-publish` | crate | NEW | per-account quota pacing & publish job wire types | 2 | 1,921 | `crates/engine/src/publish/`, `crates/protocol/src/` |
| `rightkit-social-publish` | crate | NEW | connected platform accounts, readiness, platform data-deletion requests | 4 | 1,662 | `crates/engine/src/`, `crates/protocol/src/` |
| `@rightkit/i18n` | npm | EXTEND | money micro-units & instant formatting | 2 | 150 | `src/data/` |
| `@rightkit/react-shell` | npm | EXTEND | useAsync, state router, operator layout css | 3 | 350 | `src/`, `src/hooks/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 2 | 217 | `src/` |
| `@rightkit/release` | npm | EXTEND | manifest-consistency gate checks | 4 | 547 | `crates/tools/`, `crates/tools/src/`, `crates/tools/tests/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 138 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | spend allowances panel | 1 | 261 | `src/screens/` |
| `@rightkit/settings-ui` | npm | EXTEND | store health/diagnostics panel | 1 | 197 | `src/screens/` |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 1 | 5 | `src/` |
| `rightkit-agents` | crate | EXTEND | headless drafting via installed Claude/Codex CLI | 2 | 339 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-channel-memory` | crate | EXTEND | channel store & channel memory document (schema already in RK) | 3 | 1,530 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-channel-memory` | crate | EXTEND | fixtures | 8 | 88 | `tests/fixtures/channels/fixture-alpha/website/`, `tests/fixtures/channels/fixture-beta/website/` |
| `rightkit-contracts` | crate | EXTEND | contract.check & delivery envelope IO at app boundary | 2 | 1,218 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-daemon` | crate | EXTEND | headless server role (scheduler, public listener, remote host) | 1 | 2,442 | `crates/engine/src/` |
| `rightkit-db` | crate | EXTEND | SQLite store backup, verify & restore | 2 | 1,211 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-fs` | crate | EXTEND | API gap first: api-gap, rightkit-logs 0.3.5 redacts structured JSON by key (redact.rs:51) and text by shape or name=value (sc | 1 | 436 | `crates/engine/src/` |
| `rightkit-job-queue` | crate | EXTEND | per-command request/response schema validation | 1 | 141 | `crates/protocol/src/` |
| `rightkit-logs` | crate | EXTEND | API gap first: api-gap, rightkit-tauri-shell 0.6.0 logs feature covers write and collect, but clear is store.clear() on one f | 1 | 166 | `src-tauri/src/` |
| `rightkit-platform-rules` | crate | EXTEND | pinned platform API versions & sunset dates | 1 | 107 | `crates/engine/src/` |
| `rightkit-qa` | crate | EXTEND | fake editor engine & fake Claude/Codex CLI | 2 | 726 | `crates/mock-platform/src/` |
| `rightkit-review` | crate | EXTEND | deterministic text quality/approval gate with policy revisions | 2 | 1,485 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-review` | crate | EXTEND | phone review & pause surface over daemon | 1 | 924 | `crates/engine/src/` |
| `rightkit-spend` | crate | EXTEND | app spend layer over rightkit-spend ledger | 2 | 2,272 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 3 | 451 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 24 | `src-tauri/` |
| `rightkit-time` | crate | EXTEND | local wall time -> UTC for calendars (tz-aware) | 1 | 233 | `crates/engine/src/` |
| `@rightkit/issues-tracker` | npm | ADOPT | generated | 1 | 88 | `./` |
| `@rightkit/release` | npm | ADOPT | release pin check | 1 | 47 | `scripts/` |
| `@rightkit/settings-ui` | npm | ADOPT | settings panel | 1 | 64 | `src/screens/` |
| `@rightkit/tauri` | npm | ADOPT | thin adapters over existing RK packages | 7 | 133 | `src/lib/` |
| `@rightkit/theme` | npm | ADOPT | generated | 1 | 10 | `src/` |
| `rightkit-app-settings` | crate | ADOPT | settings get/patch for settings-ui | 1 | 66 | `crates/engine/src/` |
| `rightkit-daemon` | crate | ADOPT | heartbeat record already owned by rightkit-daemon | 2 | 186 | `crates/engine/src/`, `crates/protocol/src/` |
| `rightkit-http` | crate | ADOPT | one-request HTTP layer already on rightkit-http | 1 | 597 | `crates/engine/src/publish/` |
| `rightkit-qa` | crate | ADOPT | mock server already rightkit-qa MockServer | 2 | 181 | `crates/mock-platform/src/` |
| `serde_json` | other | DROP | hand-rolled JSON; use serde_json | 1 | 468 | `crates/mock-platform/src/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 266 | 71,997 | `crates/e2e/`, `crates/e2e/src/`, `crates/e2e/tests/` +10 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 2 | 366 | `src-tauri/examples/`, `src-tauri/tests/` |
| `app-shell` | other | SHELL | app composition | 3 | 124 | `./`, `src/` |
| `app-shell` | other | SHELL | tauri entry & engine wiring on rightkit-tauri-shell | 1 | 76 | `src-tauri/` |
| `app-shell` | other | SHELL | engine binary & command table composition | 2 | 74 | `crates/engine/`, `crates/protocol/` |
| `app-shell` | other | CONFIG | app manifests & release config | 3 | 97 | `./` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `src/Startup.tsx` | 96 |
| `src-tauri/Cargo.toml` | 76 |
| `crates/engine/Cargo.toml` | 60 |
| `src/vite-env.d.ts` | 16 |
| `crates/protocol/Cargo.toml` | 14 |
| `index.html` | 12 |
