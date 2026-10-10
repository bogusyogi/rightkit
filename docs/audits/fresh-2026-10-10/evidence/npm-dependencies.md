# Fresh npm dependency evidence — 2026-10-10

Scope: root `/Volumes/D/claude/rightkit/package.json`, all 24 `/Volumes/D/claude/rightkit/packages/*/package.json`, plus local app `/Volumes/D/claude/rightkit/desktop-app-template/package.json`. Prior audits were ignored.

Method: read manifests, scripts, source/test/e2e imports, dynamic imports, package `exports`/`files`/`bin`, app README/placeholders, app frontend & Cargo manifests. Import scan excluded `node_modules`, generated `dist`, `.git`, & e2e output. No install, network, build, test, source edit, commit, or publish was run. Evidence uses absolute `path:line` loci; within an evidence row, omitted `src/...` prefixes repeat row's absolute package prefix.

Denominator: 26 manifests; 25 RightKit workspace manifests (root + 24 packages) & 1 local app; 171 direct declarations = 24 `dependencies`, 0 `optionalDependencies`, 24 `peerDependencies`, 123 `devDependencies`. Every package name below is included, including zero-dependency/private packages.

## Direct declaration inventory

`runtime` means shipped `dependencies`; `peer` means host-supplied runtime/type contract; `dev` means build/test/preview/release tooling. Manifest loci are declaration evidence; source loci below validate use.

| Manifest/package | runtime | peer | dev/tooling |
|---|---|---|---|
| `/Volumes/D/claude/rightkit/package.json` · `rightkit-shared` | — | — | — (`:1-41`) |
| `/Volumes/D/claude/rightkit/packages/agent-room-mcp/package.json` · `@rightkit/agent-room-mcp` | — | — | — (`:1-10`, private) |
| `/Volumes/D/claude/rightkit/packages/app-shell/package.json` · `@rightkit/app-shell` | — | `@rightkit/platform-ui`, `react` (`:33-35`) | platform-ui, qa, React types/runtime, TypeScript, Vite, Vitest (`:37-46`) |
| `/Volumes/D/claude/rightkit/packages/ax/package.json` · `@rightkit/ax` | — | — | — (`:1-44`) |
| `/Volumes/D/claude/rightkit/packages/build-control-policy/package.json` · `@rightkit/build-control-policy` | — | — | — (`:1-6`, private) |
| `/Volumes/D/claude/rightkit/packages/doc-render/package.json` · `@rightkit/doc-render` | `@types/mdast`, `dompurify`, `github-slugger`, `remark-frontmatter`, `remark-gfm`, `remark-math`, `remark-parse`, `remark-smartypants`, `shiki`, `unified` (`:30-40`) | `react` (`:27-29`) | qa, React types/runtime, esbuild, TypeScript, Vitest (`:42-50`) |
| `/Volumes/D/claude/rightkit/packages/git/package.json` · `@rightkit/git` | — | — | — (`:1-30`) |
| `/Volumes/D/claude/rightkit/packages/hooks/package.json` · `@rightkit/hooks` | — | — | — (`:1-32`) |
| `/Volumes/D/claude/rightkit/packages/i18n/package.json` · `@rightkit/i18n` | — | `react` (`:12`) | Testing Library, React types/runtime, jsdom, TypeScript, Vitest (`:13-21`) |
| `/Volumes/D/claude/rightkit/packages/legal/package.json` · `@rightkit/legal` | — | — | Node types, TypeScript, Vitest (`:20-23`) |
| `/Volumes/D/claude/rightkit/packages/legal-ui/package.json` · `@rightkit/legal-ui` | — | `react` (`:27-29`) | Testing Library, React types/runtime, axe-core, esbuild, jsdom, TypeScript, Vitest (`:30-42`) |
| `/Volumes/D/claude/rightkit/packages/license/package.json` · `@rightkit/license` | — | optional `@tauri-apps/plugin-http` (`:26-30`) | same plugin, Node types, TypeScript, Vitest (`:20-24`) |
| `/Volumes/D/claude/rightkit/packages/logs/package.json` · `@rightkit/logs` | — | — | TypeScript, Vitest (`:23-25`) |
| `/Volumes/D/claude/rightkit/packages/media-ui/package.json` · `@rightkit/media-ui` | — | optional `fabric`, `react-image-crop`; required React + react-dom (`:28-37`) | qa, Testing Library, React types/runtime, esbuild, jsdom, TypeScript, Vitest (`:38-52`) |
| `/Volumes/D/claude/rightkit/packages/onboarding/package.json` · `@rightkit/onboarding` | — | app-shell, legal-ui, React (`:26-30`) | app-shell, legal-ui, qa, Testing Library, React types/runtime, jsdom, TypeScript, Vite, Vitest (`:31-47`) |
| `/Volumes/D/claude/rightkit/packages/pdf-ui/package.json` · `@rightkit/pdf-ui` | `pdfjs-dist` (`:30-32`) | `react` (`:27-29`) | qa, React types/runtime, esbuild, TypeScript, Vitest (`:33-41`) |
| `/Volumes/D/claude/rightkit/packages/platform-ui/package.json` · `@rightkit/platform-ui` | — | plugin-os, React (`:26-29`) | plugin-os, React types/runtime, TypeScript, Vitest (`:30-35`) |
| `/Volumes/D/claude/rightkit/packages/qa/package.json` · `@rightkit/qa` | `@rightkit/qa-chrome` (`:32-34`) | — | Node types, TypeScript, Vitest (`:35-38`) |
| `/Volumes/D/claude/rightkit/packages/qa-chrome/package.json` · `@rightkit/qa-chrome` | — | — | Node types, TypeScript, Vitest (`:28-31`) |
| `/Volumes/D/claude/rightkit/packages/release/package.json` · `@rightkit/release` | `smol-toml` (`:38-40`) | — | —; CLI/test package uses Node built-ins (`:1-37`) |
| `/Volumes/D/claude/rightkit/packages/settings-ui/package.json` · `@rightkit/settings-ui` | — | app-shell, shell, license, logs, theme, updates, React (`:27-35`) | qa, shell, Testing Library, React types/runtime, jsdom, TypeScript, Vite, Vitest (`:36-49`) |
| `/Volumes/D/claude/rightkit/packages/shell/package.json` · `@rightkit/shell` | — | — | TypeScript (`:22-23`) |
| `/Volumes/D/claude/rightkit/packages/tauri/package.json` · `@rightkit/tauri` | `@rightkit/logs` (`:18-20`) | `@rightkit/updates` (`:21-23`) | TypeScript, Vitest (`:30-32`) |
| `/Volumes/D/claude/rightkit/packages/theme/package.json` · `@rightkit/theme` | — | — | qa, Node types, TypeScript, Vitest (`:31-35`) |
| `/Volumes/D/claude/rightkit/packages/updates/package.json` · `@rightkit/updates` | — | — | TypeScript, Vitest (`:23-25`) |
| `desktop-app-template/package.json` · `rightapp` | logs, platform-ui, tauri, updates, Tauri API/plugins, React + react-dom (`:40-50`) | — | release, Tailwind Vite/CLI, Tauri CLI, React types, React Vite plugin, TypeScript, Vite (`:52-61`) |

## Import/use evidence

All shipped runtime declarations have matching source use. Peer declarations also match source or emitted public type contracts. Key loci:

| Package | Evidence |
|---|---|
| `@rightkit/app-shell` | platform-ui runtime/type imports at `/Volumes/D/claude/rightkit/packages/app-shell/src/bridge.ts:14`, `src/react/layout.tsx:18-19`, `src/react/provider.tsx:11`; React runtime imports at `src/react/contextmenu.tsx:1-2`; qa is e2e-only at `e2e/browser.ts:7`; Vitest config at `vitest.config.ts:1`. |
| `@rightkit/doc-render` | `unified` + all remark plugins at `/Volumes/D/claude/rightkit/packages/doc-render/src/parseMarkdown.ts:2-7`; slugger at `src/footnotes.ts:29` & `src/headingSlugs.ts:2`; DOMPurify at `src/security.ts:1`; Shiki static/dynamic imports at `src/shikiHighlighter.ts:1,22-74,139,145,156,168-171`; React peer at `src/renderers.tsx:1`; `@types/mdast` supplies type-only `mdast` imports (`src/detectors/callout.ts:1`, `src/parseMarkdown.ts:1`) & emitted public declarations (`dist/parseMarkdown.d.ts:1`). |
| `@rightkit/i18n` | React peer at `/Volumes/D/claude/rightkit/packages/i18n/src/index.tsx:1`; test tooling at `src/index.test.tsx:1-3` & `vitest.config.ts:1`; jest-dom side-effect setup at `src/index.test.tsx:2`. |
| `@rightkit/legal-ui` | React at `/Volumes/D/claude/rightkit/packages/legal-ui/src/LegalGate.tsx:1`; Testing Library/user-event/Vitest at `src/LegalGate.test.tsx:1-3`; jest-dom setup at `src/test-setup.ts:1`; esbuild at `preview/build.mjs:6`; axe-core at `preview/verify.mjs:7`. |
| `@rightkit/license` | Optional peer is lazy-loaded only in Tauri at `/Volumes/D/claude/rightkit/packages/license/src/index.ts:96-112`; failure falls back, matching optional peer metadata. |
| `@rightkit/media-ui` | Root entry documents fabric-free split at `/Volumes/D/claude/rightkit/packages/media-ui/src/index.ts:7-9,36-37`; fabric is imported by image-only module at `src/image/FabricTextOverlay.tsx:2`, loaded from `src/image/ImageViewerStage.tsx:20`; react-image-crop is image-only at `src/image/ImageCropTool.tsx:2`, loaded at `src/image/ImageViewerStage.tsx:23`; React/react-dom imports at `src/MediaPlayer.tsx:12`, `src/image/ImageViewerShell.tsx:1`; e2e qa/esbuild at `test/browser-e2e.mjs:10,46`. |
| `@rightkit/onboarding` | app-shell/legal-ui/React imports at `/Volumes/D/claude/rightkit/packages/onboarding/src/Onboarding.tsx:1-3`, `src/steps.tsx:1-2`, `src/types.ts:1-2`; Vite e2e config at `e2e/harness/vite.config.mjs:1`; qa at `e2e/browser.ts:7`. |
| `@rightkit/pdf-ui` | pdfjs runtime/type/TextLayer imports at `/Volumes/D/claude/rightkit/packages/pdf-ui/src/PdfViewer.tsx:12-15` & `src/pdfJs.ts:4`; React peer at `src/PdfViewer.tsx:12`; esbuild e2e at `test/browser-e2e.mjs:8`. |
| `@rightkit/platform-ui` | plugin-os peer is runtime-imported at `/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:1`; React peer at `src/react.tsx:1`; Vitest at `src/index.test.ts:1`. |
| `@rightkit/qa` | qa-chrome runtime re-export at `/Volumes/D/claude/rightkit/packages/qa/src/chrome.ts:1` and root export at `src/index.ts:1`; package README confirms dependency at `packages/qa/README.md:16-21`. |
| `@rightkit/release` | `smol-toml` runtime imports at `/Volumes/D/claude/rightkit/packages/release/inventory-check.mjs:11`, `ownership-catalog.mjs:4`, `ownership-lockfiles.mjs:2`. Puppeteer strings in tests are fixtures, not imports or declarations (`ownership-approval-v2.test.mjs:8`, `ownership.test.mjs:214-216`). |
| `@rightkit/settings-ui` | app-shell/license/logs/theme/updates imports at `/Volumes/D/claude/rightkit/packages/settings-ui/src/sections.tsx:2-6`; license runtime at `src/hooks.ts:5-6`; shell public type contract at `src/backends.ts:11,21-22`; React at `src/SettingsPanel.tsx:1`; test/e2e tooling at `src/*.test.tsx`, `e2e/harness/*`, `e2e/browser.ts:7`. |
| `@rightkit/tauri` | logs runtime constants at `/Volumes/D/claude/rightkit/packages/tauri/src/index.ts:2-6`; updates is a public type import at `src/index.ts:1,42-49`; README records both contracts at `packages/tauri/README.md:26-28`. |
| `desktop-app-template` | logs/platform-ui/updates at `/Volumes/D/claude/rightkit/desktop-app-template/src/lib/logs.ts:1`, `src/lib/platform.ts:2`, `src/lib/updates.ts:1`; Tauri API at `src/lib/host.ts:3` & `src/shell/AppShell.tsx:6`; updater plugin at `src/lib/updates.ts:2`; React/react-dom at `src/App.tsx:1`, `src/main.tsx:1-2`; release at `right-release.config.mjs:7`; Vite plugins at `vite.config.ts:1-3`; Tailwind at `src/styles.css:1`; Tauri CLI is script-backed at `package.json:16-18`. |

Zero-dependency packages (`agent-room-mcp`, `ax`, `build-control-policy`, `git`, `hooks`, `legal` runtime, `logs`, `qa-chrome`, `shell`, `theme`, `updates`) use Node built-ins, own files, or no external runtime package. `@rightkit/legal` uses Node crypto at `/Volumes/D/claude/rightkit/packages/legal/src/index.ts:1`; its Node types are dev-only.

## Confirmed findings & bounded candidates

### Confirmed current app declaration issue

`@rightkit/tauri@0.1.1` is declared in `/Volumes/D/claude/rightkit/desktop-app-template/package.json:43`, but no app source imports `@rightkit/tauri`; README labels it “add when wiring license/device” at `/Volumes/D/claude/rightkit/desktop-app-template/README.md:15`. Current app has no license/device adapter wiring. Remove from current app manifest when maintaining present scope, or retain only when wiring described in README begins.

### Candidates requiring product decision

1. `@tauri-apps/plugin-process@2.3.1` has no frontend JS import (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:47`; source scan found none), while native Cargo + capability use it at `/Volumes/D/claude/rightkit/desktop-app-template/src-tauri/Cargo.toml:24-26` & `/Volumes/D/claude/rightkit/desktop-app-template/src-tauri/capabilities/default.json:12-14`. npm package looks removable from frontend if no JS process API is planned; native Cargo plugin remains separate.
2. App pins lag current package manifests: `@rightkit/platform-ui` `0.1.2` vs package `0.2.0` (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:42`; `/Volumes/D/claude/rightkit/packages/platform-ui/package.json:3`), `@rightkit/tauri` `0.1.1` vs `0.2.0` (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:43`; `/Volumes/D/claude/rightkit/packages/tauri/package.json:3`), `@rightkit/updates` `0.2.4` vs `0.2.5` (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:44`; `/Volumes/D/claude/rightkit/packages/updates/package.json:3`), `@rightkit/release` `0.2.119` vs `0.2.147` (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:53`; `/Volumes/D/claude/rightkit/packages/release/package.json:3`). `@rightkit/logs` `0.1.4` matches (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:41`; `/Volumes/D/claude/rightkit/packages/logs/package.json:3`). This is adoption/version drift, not proof each bump is safe.
3. `@types/mdast` is listed under `dependencies` at `/Volumes/D/claude/rightkit/packages/doc-render/package.json:30-32`, while source usage is type-only and emitted `.d.ts` files import `mdast` (`/Volumes/D/claude/rightkit/packages/doc-render/src/parseMarkdown.ts:1`; `/Volumes/D/claude/rightkit/packages/doc-render/dist/parseMarkdown.d.ts:1`). It is not an unnecessary package: moving it to dev-only would break consumer declaration resolution unless types are bundled. Current classification is “public type runtime dependency”.

`@tauri-apps/plugin-os` has no direct app import, but is justified as the required peer for imported `@rightkit/platform-ui` (`/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:1`; app pin `/Volumes/D/claude/rightkit/desktop-app-template/package.json:46`).

No other declared direct dependency had absent source/script/config evidence after excluding fixture strings, docs, generated output, and built-in modules. Duplicate peer+dev declarations for React, optional image packages, plugin-http, and workspace peers are test/build support for peer contracts, not confirmed redundancy.

## Package export/file portability

Read-only path probe checked every package `main`, `types`, `exports`, `bin`, and literal `files` entry. All checked paths exist except:

- `/Volumes/D/claude/rightkit/packages/legal/package.json:8-13` points to `dist/index.js`/`dist/index.d.ts`, but `/Volumes/D/claude/rightkit/packages/legal/dist/` is absent at audit time. Build script exists at `/Volumes/D/claude/rightkit/packages/legal/package.json:15-18`; package is not standalone-consumable from this checkout until build output is produced.

Private `@rightkit/agent-room-mcp` (`/Volumes/D/claude/rightkit/packages/agent-room-mcp/package.json:4`) & `@rightkit/build-control-policy` (`/Volumes/D/claude/rightkit/packages/build-control-policy/package.json:3`) have no publish surface by design. CLI/source packages (`ax`, `git`, `hooks`, `release`, `theme`, `qa-chrome`) expose checked-in files through `files`/`bin` or direct MJS paths; no missing export/bin path was found.

## Local app adoption

Only npm app manifest inside RightKit checkout is `/Volumes/D/claude/rightkit/desktop-app-template/package.json`; sibling apps are covered in Report 3. Direct RightKit adoption is logs `0.1.4` (used), platform-ui `0.1.2` (used), tauri `0.1.1` (unused until future wiring), updates `0.2.4` (used), & dev release `0.2.119` (release config/script tooling). App README also records staged placeholders: app-shell & media-ui are not dependencies yet (`/Volumes/D/claude/rightkit/desktop-app-template/README.md:23-24,65-66`), while license/legal/legal-ui are add-ons (`/Volumes/D/claude/rightkit/desktop-app-template/README.md:24,60-64`). Non-RightKit direct frontend deps are Tauri API, updater, OS/process plugins, React, & React DOM (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:45-50`); Tailwind/Vite/Tauri CLI/React types/TypeScript tooling are dev-only (`/Volumes/D/claude/rightkit/desktop-app-template/package.json:52-61`).

Unknowns: registry package contents were not fetched; install-time peer auto-install behavior was not inferred as runtime adoption; no other app repositories are present under this checkout's package manifests.
