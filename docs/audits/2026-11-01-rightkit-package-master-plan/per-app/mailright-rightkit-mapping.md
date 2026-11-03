# MailRight: RightKit mapping

Repo `mailright` at `debbd78` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`mailright-rightkit-files.json`](mailright-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 413 | 80,218 |
| EXTEND | existing RightKit package gains this | 179 | 26,927 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 35 | 3,664 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 14 | 1,868 |
| DROP | delete | 1 | 164 |
| SHELL | thin app composition that stays in app | 3 | 51 |
| CONFIG | app manifest/config that stays | 3 | 263 |
| NOTCODE | docs/data, not code | 2 | 12,893 |
| **Total** | | **650** | **126,048** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **110,973 lines**. Staying in the app as product code: **0 lines**; as shell/config: **314 lines** (0% of code), plus **1,868 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/agent-ui` | npm | NEW | agent approval cards, pairing & audit views | 7 | 1,057 | `src/components/`, `src/hooks/`, `src/lib/` +1 dirs |
| `@rightkit/agent-ui` | npm | NEW | tests | 1 | 241 | `tests/` |
| `@rightkit/agent-ui` | npm | NEW | approval IPC client | 1 | 97 | `src/lib/ipc/` |
| `@rightkit/config` | npm | NEW | shared presets | 5 | 114 | `./`, `src-tauri/.cargo/` |
| `@rightkit/mail-ui` | npm | NEW | inbox virtual list, reader & sandboxed email frame, compose editor & recipients, search routing/coverage/saved searches, image trust, attachment preview, account switcher, mail store | 94 | 15,708 | `src/components/`, `src/hooks/`, `src/lib/` +6 dirs |
| `@rightkit/mail-ui` | npm | NEW | tests | 18 | 1,347 | `tests/` |
| `@rightkit/waitlist-worker` | npm | NEW | Cloudflare waitlist worker | 1 | 311 | `workers/waitlist/src/` |
| `@rightkit/workspace-ui` | npm | NEW | calendar grid & event forms, contacts, drive browser & preview, tasks, meet | 19 | 2,679 | `src/hooks/`, `src/lib/`, `src/views/` +1 dirs |
| `@rightkit/workspace-ui` | npm | NEW | tests | 1 | 43 | `tests/` |
| `rightkit-agent-gateway` | crate | NEW | paired external agents over local daemon: method registry, gate, approval cards (send/forward/delete/attach), audit log, untrusted-content contract, redaction | 61 | 14,620 | `src-tauri/src/agent/`, `src-tauri/src/agent/tests/`, `src-tauri/src/commands/` +1 dirs |
| `rightkit-app-lock` | crate | NEW | app lock: OS-auth verification, serialized lock/unlock transitions, DB gate | 7 | 1,244 | `src-tauri/src/`, `src-tauri/src/app_lock/` |
| `rightkit-approvals` | crate | NEW | user approval cards for agent send/forward/delete/attach | 11 | 2,083 | `src-tauri/src/agent/`, `src-tauri/src/agent/tests/`, `src-tauri/src/commands/` +1 dirs |
| `rightkit-bench` | crate | NEW | SQLite perf proof & probes, load driver | 22 | 4,191 | `src-tauri/src/`, `src-tauri/src/bin/`, `src-tauri/src/perf_proof/` +1 dirs |
| `rightkit-doctor` | crate | NEW | doctor report | 1 | 95 | `src-tauri/engine/src/` |
| `rightkit-engine-host` | crate | NEW | engine binary framework: sidecar + headless CLI modes, command registration, contract export & TS codegen, human/JSON presentation, engine client | 1 | 7 | `src-tauri/core/src/` |
| `rightkit-google-apis` | crate | NEW | Google Workspace clients: Gmail sync/history/batch/MIME/compose/labels/filters/vacation/drafts, Calendar, Drive, People/Contacts, Tasks, Meet, budgets & backoff | 39 | 8,778 | `src-tauri/src/`, `src-tauri/src/calendar/`, `src-tauri/src/drive/` +6 dirs |
| `rightkit-mail` | crate | NEW | provider-neutral local mail mirror: schema, outbox & pending ops, sync apply/rollups, recipients, risk/trust signals, remote-image proxy & tracker blocking, attachment cache, engine method groups | 124 | 27,603 | `src-tauri/src/`, `src-tauri/src/attach_cache/`, `src-tauri/src/db/` +14 dirs |
| `@rightkit/qa` | npm | EXTEND | mock IPC data for UI QA | 10 | 1,335 | `src/lib/ipc/` |
| `@rightkit/qa` | npm | EXTEND | tests | 3 | 434 | `tests/` |
| `@rightkit/react-shell` | npm | EXTEND | error boundary, find bar, shortcut registry & help overlay, dialogs, perf monitor | 18 | 1,839 | `src/components/`, `src/hooks/`, `src/lib/` |
| `@rightkit/react-shell` | npm | EXTEND | tests | 10 | 578 | `tests/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 3 | 529 | `src/` |
| `@rightkit/react-shell` | npm | EXTEND | app lock overlay | 1 | 47 | `src/components/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 193 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | mail/compose/privacy/storage/semantic/shortcut sections, write queue | 23 | 2,776 | `src/hooks/`, `src/lib/`, `src/views/` |
| `@rightkit/settings-ui` | npm | EXTEND | tests | 6 | 689 | `tests/` |
| `@rightkit/tauri` | npm | EXTEND | API gap first: MOVE @rightkit/theme. See docs/rightkit-exceptions/M2.md#b3-theme-preset-catalogue-srclibthemets-themes-preset | 1 | 293 | `src/lib/` |
| `@rightkit/theme` | npm | EXTEND | app stylesheet: brand tokens & shell css from @rightkit/theme / react-shell | 1 | 21 | `src/` |
| `rightkit-app-settings` | crate | EXTEND | API gap first: MOVE @rightkit/settings-ui and the shell log host (multi-file log tail preview with reveal, bogusyogi/rightkit | 1 | 307 | `src-tauri/src/commands/` |
| `rightkit-credentials` | crate | EXTEND | API gap first: ADOPT rightkit-db 0.5.2. See docs/rightkit-exceptions/M9.md#b2-sqlcipher-key-rotation-and-pending-key-open-src | 1 | 274 | `src-tauri/src/` |
| `rightkit-daemon` | crate | EXTEND | runtime service lifecycle, health snapshot, event hub w/ debounce bridge, lock budgets, repo bundle | 23 | 3,421 | `src-tauri/src/runtime/`, `src-tauri/src/runtime/repos/`, `src-tauri/src/runtime/repos/sqlite/` +1 dirs |
| `rightkit-daemon` | crate | EXTEND | generic app CLI client over running app: arg parsing, error envelope & exit codes, pairing key, link | 11 | 2,168 | `src-tauri/engine/`, `src-tauri/engine/src/`, `src-tauri/engine/src/tests/` |
| `rightkit-db` | crate | EXTEND | compaction w/ rowid verify, background WAL checkpoints, verified pre-migration backup, migration gate, lock-order gate | 15 | 2,552 | `src-tauri/src/`, `src-tauri/src/commands/`, `src-tauri/src/db/` +3 dirs |
| `rightkit-embed` | crate | EXTEND | background semantic indexing job | 2 | 398 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `rightkit-fs` | crate | EXTEND | downloads folder, blocking helper, Windows ACL hardening | 3 | 94 | `src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: MOVE rightkit-license. See docs/rightkit-exceptions/M12.md#b1-revocation-denylist-src-taurisrcpro_gaters-revok | 1 | 184 | `src-tauri/src/` |
| `rightkit-llm` | crate | EXTEND | BYOK AI actions & translation providers | 4 | 941 | `src-tauri/src/`, `src-tauri/src/ai/`, `src-tauri/src/engine_api/` |
| `rightkit-logs` | crate | EXTEND | API gap first: MOVE rightkit-logs (text-line redactor with stable key-token hashing, bogusyogi/rightkit#520). See docs/rightk | 1 | 268 | `src-tauri/src/` |
| `rightkit-models` | crate | EXTEND | API gap first: MOVE @rightkit/release. See docs/rightkit-exceptions/M19.md#b1-embedding-model-bundle-provisioning-script-and- | 1 | 340 | `scripts/` |
| `rightkit-qa` | crate | EXTEND | process-wide test sandbox | 2 | 378 | `src-tauri/src/`, `src-tauri/src/test_root/` |
| `rightkit-qa` | crate | EXTEND | test sandbox | 1 | 110 | `src-tauri/engine/src/` |
| `rightkit-search` | crate | EXTEND | HTML->indexable text, FTS5 maintenance & background rebuild | 3 | 944 | `src-tauri/src/`, `src-tauri/src/db/` |
| `rightkit-sidecar` | crate | EXTEND | process engine split: core child owning DB, IPC transport, event pump, supervision, trust boundary | 15 | 3,109 | `src-tauri/src/core_ipc/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 15 | 2,377 | `src-tauri/src/`, `src-tauri/src/commands/`, `src-tauri/src/commands/mail/` +2 dirs |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 171 | `src-tauri/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: MOVE rightkit-tauri-shell. See docs/rightkit-exceptions/M5.md#b1-start-up-failure-message-box-src-taurisrcstar | 1 | 88 | `src-tauri/src/` |
| `rightkit-time` | crate | EXTEND | API gap first: MOVE rightkit-time. See docs/rightkit-exceptions/M15.md#b3-wall-clock-utc-stamps-src-taurisrcutc_stamprs: 1) n | 1 | 69 | `src-tauri/src/` |
| `@rightkit/issues-tracker` | npm | ADOPT | generated | 1 | 88 | `./` |
| `@rightkit/legal` | npm | ADOPT | legal configs & gates | 5 | 306 | `scripts/` |
| `@rightkit/legal` | npm | ADOPT | legal acceptance host | 2 | 258 | `src-tauri/src/commands/` |
| `@rightkit/legal-ui` | npm | ADOPT | legal gate | 3 | 201 | `src/components/`, `src/lib/` |
| `@rightkit/pdf-ui` | npm | ADOPT | pdf preview | 2 | 144 | `src/components/`, `src/lib/` |
| `@rightkit/tauri` | npm | ADOPT | adapters over existing RK packages | 4 | 266 | `src/lib/` |
| `@rightkit/theme` | npm | ADOPT | contrast check | 1 | 74 | `scripts/` |
| `rightkit-app-settings` | crate | ADOPT | UI preference store | 6 | 725 | `src-tauri/src/`, `src-tauri/src/commands/settings/` |
| `rightkit-crash` | crate | ADOPT | panic capture | 1 | 68 | `src-tauri/src/` |
| `rightkit-license` | crate | ADOPT | licence gate & device identity | 4 | 674 | `src-tauri/src/commands/` |
| `rightkit-logs` | crate | ADOPT | redaction & renderer logs | 1 | 46 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | ADOPT | build identity, tray, shell plugin, startup error window | 5 | 814 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `none` | other | DROP | one-off dev tool | 1 | 164 | `src-tauri/src/` |
| `app-shell` | other | APPTEST | app journey/e2e tests | 11 | 1,198 | `qa/journeys/`, `qa/native-e2e/`, `qa/native-e2e/src/` +3 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 3 | 670 | `src-tauri/src/commands/`, `src-tauri/tests/` |
| `app-shell` | other | SHELL | app composition | 2 | 32 | `src/` |
| `app-shell` | other | SHELL | tauri entry, setup, command surface | 1 | 19 | `src-tauri/core/` |
| `app-shell` | other | CONFIG | app manifests & release config | 3 | 263 | `./`, `src-tauri/` |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 2 | 12,893 | `legal/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `src/AppLazyViews.tsx` | 31 |
| `src-tauri/core/Cargo.toml` | 19 |
| `src/vite-env.d.ts` | 1 |
