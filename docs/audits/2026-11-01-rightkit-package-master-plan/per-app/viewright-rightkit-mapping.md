# ViewRight: RightKit mapping

Repo `viewright` at `be38b24` (main). Generated 2026-11-01. Every tracked code file (Rust, TS/JS, Swift, Kotlin, Python, shell, CSS/HTML, TOML, SQL) is assigned to exactly one target. Full per-file list: [`viewright-rightkit-files.json`](viewright-rightkit-files.json).

Package names are final, after cross-app merging (see master plan in RightKit `docs/audits/2026-11-01-rightkit-package-master-plan.md`).

## Summary

| Action | Meaning | Files | Lines |
|---|---|---:|---:|
| NEW | new RightKit package (or new piece of one) | 408 | 83,202 |
| EXTEND | existing RightKit package gains this | 91 | 19,440 |
| ADOPT | RightKit already ships it; delete app copy & use owner | 14 | 1,823 |
| APPTEST | app journey/e2e test: stays with the app (P7, §6), runs on rightkit-qa | 85 | 34,140 |
| VENDOR-KEEP | vendored 3rd-party: keep as plain dependency, delete vendored copy | 1,078 | 208,933 |
| DROP | delete | 104 | 13,663 |
| SHELL | thin app composition that stays in app | 5 | 250 |
| CONFIG | app manifest/config that stays | 146 | 1,822 |
| NOTCODE | docs/data, not code | 12 | 31,121 |
| **Total** | | **1,943** | **394,394** |

Code leaving the app (NEW + EXTEND + ADOPT + LEGION + vendor + DROP): **327,061 lines**. Staying in the app as product code: **0 lines**; as shell/config: **2,072 lines** (0% of code), plus **34,140 lines** of app journey/e2e tests.

## Mapping table

| RightKit target | Kind | Action | What moves | Files | Lines | Where (sample) |
|---|---|---|---|---:|---:|---|
| `@rightkit/config` | npm | NEW | shared presets | 2 | 304 | `./` |
| `@rightkit/document-workspace` | npm | NEW | multi-format document workspace: tabs & panes, drafts, autosave, external-change watcher, find & cross-doc search, command palette, explorer/library sidebar, print pipeline, zoom, plain-text editor | 167 | 29,007 | `src/app/`, `src/app/shell/`, `src/app/sidebar/` +7 dirs |
| `@rightkit/document-workspace` | npm | NEW | workspace styles | 3 | 2,164 | `src/styles/` |
| `@rightkit/docx-editor` | npm | NEW | DOCX view/edit integration | 13 | 1,834 | `src/documents/docx/` |
| `@rightkit/html-editor` | npm | NEW | HTML document view/authoring: sandboxed session, blocks, site-root resources | 61 | 15,898 | `src/documents/html/`, `src/documents/html/blocks/` |
| `@rightkit/markdown-editor` | npm | NEW | markdown/MDX hybrid editor: projection, block & inline editing (code, math, mermaid, tables), citations, references, properties, export, safe HTML | 143 | 27,905 | `src/documents/markdown/`, `src/documents/markdown/analysis/`, `src/documents/markdown/assets/` +25 dirs |
| `@rightkit/waitlist-worker` | npm | NEW | Cloudflare waitlist worker | 2 | 218 | `workers/waitlist/`, `workers/waitlist/src/` |
| `rightkit-bench` | crate | NEW | association & PDF bench matrices | 6 | 2,469 | `scripts/` |
| `rightkit-bench` | crate | NEW | PDF perf bench | 1 | 256 | `src-tauri/src/` |
| `rightkit-document-workspace` | crate | NEW | Rust side of document workspace: recent files & pins DB, explorer listing, markdown index/resource inspection, document IO | 10 | 3,147 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `@rightkit/pdf-ui` | npm | EXTEND | PDF editor UI: annotate, text edit, OCR overlay, page ops | 38 | 10,411 | `src/documents/pdf/` |
| `@rightkit/platform-ui` | npm | EXTEND | API gap first: Browser-QA platform override seeded into __RIGHTKIT_PLATFORM__; platform-ui offers the override only on its /q | 1 | 42 | `src/` |
| `@rightkit/qa` | npm | EXTEND | QA browser helpers & tooling | 3 | 678 | `scripts/`, `scripts/lib/` |
| `@rightkit/react-shell` | npm | EXTEND | app frame composition, splash, engine API client (app keeps a short composition) | 1 | 52 | `src/` |
| `@rightkit/release` | npm | EXTEND | release config logic moves into @rightkit/release; app keeps a JSON data config (P8) | 1 | 361 | `./` |
| `@rightkit/settings-ui` | npm | EXTEND | document/editor settings sections | 20 | 2,684 | `src/settings/`, `src/settings/sections/` |
| `@rightkit/theme` | npm | EXTEND | icon, tray & installer art generation from the brand file | 2 | 399 | `src-tauri/icons/` |
| `@rightkit/theme` | npm | EXTEND | brand contract check | 1 | 290 | `scripts/` |
| `rightkit-docs` | crate | EXTEND | document format classification table | 2 | 695 | `src-tauri/src/` |
| `rightkit-license` | crate | EXTEND | API gap first: ViewRightDevice: the DeviceIdSource over DeviceService and ViewRight's keychain slot; rightkit-device 0.4.0 ha | 1 | 320 | `src-tauri/src/` |
| `rightkit-logs` | crate | EXTEND | API gap first: Native record helpers, the early (pre-install) append path and the diagnostics gate mirror around the shell Lo | 1 | 166 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | path-grant broker, site-root & preview custom protocols, range-streamed binary reads, contained HTML assets | 8 | 1,590 | `src-tauri/src/`, `src-tauri/src/commands/`, `src-tauri/src/path_broker/` |
| `rightkit-tauri-shell` | crate | EXTEND | Tauri app bootstrap, lifecycle router, command surface & engine bridge (app keeps a short builder call) | 5 | 764 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: Native menu bar built by hand (File before Edit, Find inside Edit, checkable View items, tab items inside Wind | 1 | 298 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | build identity, icon embedding, tauri config restore | 1 | 221 | `src-tauri/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: WebView2 residue after adopting the shell context-menu graft: browser accelerator keys off and a NewWindowRequ | 1 | 181 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: Print-preview child window: parent disabled while open, re-enabled and refocused on close, one-shot payload ha | 1 | 148 | `src-tauri/src/commands/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: WebView2 first-paint background colour, keep a launcher-chosen user data folder and create the folder; Profile | 1 | 53 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: macOS foreground activation on an open-document event; the shell reveals but does not activate. The --prepare- | 1 | 49 | `src-tauri/src/` |
| `rightkit-tauri-shell` | crate | EXTEND | API gap first: Navigation plugin that sends the broker host (open.viewright.invalid) to the system browser through NavPolicy  | 1 | 38 | `src-tauri/src/` |
| `@rightkit/legal` | npm | ADOPT | legal contract check | 1 | 312 | `scripts/` |
| `@rightkit/platform-ui` | npm | ADOPT | platform shim & test setup | 2 | 118 | `src/shims/`, `src/test/` |
| `rightkit-file-watch` | crate | ADOPT | debounced document watcher | 1 | 205 | `src-tauri/src/` |
| `rightkit-ocr` | crate | ADOPT | OCR worker | 3 | 401 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `rightkit-pdf` | crate | ADOPT | PDF editor adapter & font catalog | 3 | 503 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `rightkit-tauri-shell` | crate | ADOPT | menu, context menu, window buttons, WebView2 env, single instance, associations, print preview | 4 | 284 | `src-tauri/src/`, `src-tauri/src/commands/` |
| `@eigenpal/docx-editor-core` | other | VENDOR-KEEP | identical to published npm 1.9.0 (core, i18n): use the npm package | 831 | 157,859 | `vendor/docx-editor/packages/core/dist/`, `vendor/docx-editor/packages/core/dist/agent/`, `vendor/docx-editor/packages/core/dist/docx/` +70 dirs |
| `@eigenpal/docx-editor-react` | other | VENDOR-KEEP | use published 1.9.x + small host-toolbar patch carried by @rightkit/docx-editor; upstream the patch as a PR | 247 | 51,074 | `vendor/docx-editor/packages/react/dist/`, `vendor/docx-editor/packages/react/src/`, `vendor/docx-editor/packages/react/src/components/` +17 dirs |
| `none` | other | DROP | unused docx-editor agents package | 104 | 13,663 | `vendor/docx-editor/packages/agents/dist/`, `vendor/docx-editor/packages/agents/dist/ai-sdk/`, `vendor/docx-editor/packages/agents/dist/vue/` +11 dirs |
| `app-shell` | other | APPTEST | app journey/e2e tests | 84 | 34,125 | `scripts/`, `scripts/lib/`, `src-tauri/qa-e2e/` +3 dirs |
| `app-shell` | other | APPTEST | app journey/e2e/unit tests of the app binary | 1 | 15 | `functions/test/` |
| `app-shell` | other | SHELL | tauri entry & command registry | 4 | 242 | `src-tauri/`, `src-tauri/.cargo/`, `src-tauri/migrations/` |
| `app-shell` | other | SHELL | app entry | 1 | 8 | `src/` |
| `app-shell` | other | CONFIG | tauri autogenerated permissions | 137 | 1,507 | `src-tauri/permissions/autogenerated/` |
| `app-shell` | other | CONFIG | app manifests | 9 | 315 | `./`, `functions/_lib/`, `functions/api/checkout/` +3 dirs |
| `none` | other | NOTCODE | generated third-party notices (each app ships its own; produced by @rightkit/legal) | 3 | 24,467 | `legal/`, `legal/third-party/` |
| `none` | other | NOTCODE | documentation pages | 5 | 3,940 | `docs/design/`, `docs/dogfood/` |
| `none` | other | NOTCODE | handoff pages | 4 | 2,714 | `tasks/handoffs/2026-10-11/next-snapshot/qa-adapter/`, `tasks/handoffs/2026-10-12/pdf-neighbour-text/` |

## What stays in the app (SHELL)

Entry points, command tables & app-specific wiring over RightKit packages. Largest files:

| File | Lines |
|---|---:|
| `src-tauri/Cargo.toml` | 160 |
| `src-tauri/deny.toml` | 48 |
| `src-tauri/migrations/0001_init.sql` | 26 |
| `src-tauri/.cargo/audit.toml` | 8 |
| `src/vite-env.d.ts` | 8 |
