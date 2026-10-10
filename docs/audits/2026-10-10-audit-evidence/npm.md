# RightKit npm packages and Swift package audit (read-only)

Date: 2026-10-10. Repo: /Volumes/D/claude/rightkit (main @ 08252a68). 24 packages under packages/ plus swift/RightKitSwift.

Method: static grep of every non-test source file for `from '<dep>'`, `import('<dep>')`, `require('<dep>')` (scripts: scratchpad/audit/scan.mjs, loc.mjs); `pnpm dlx depcheck@1.4.7 --json` once per package (completed for all 24; it reported no unused dependencies, only `vite` in app-shell and settings-ui, both false positives because vite is invoked by path); `pnpm ls` not needed. Nothing was installed, edited or built. Package sizes are from the shared pnpm store under /Volumes/D/claude/rightkit/node_modules/.pnpm.

Definitions: **src LOC** = `wc -l` of .mjs/.ts/.tsx/.js/.cjs excluding `*.test.*`, `*.integration.*`, test/, tests/, e2e/, fixtures/, preview/, type-tests/, vitest/vite configs, node_modules, dist, `.e2e-out`, `.audit` (generated). **Hits** = import sites in src / tests / scripts (e2e, preview, configs). **Verdicts**: needed, unused, move-to-dev, move-to-peer, replace-with, heavy.

## Summary: verdict counts per package

A dependency declared in several sections (e.g. dev + peer) is counted once. `depcheck` found no unused dependency anywhere (its two "unused vite" hits are false positives); hand analysis found none either. The findings are about placement and weight, not dead deps.

| Package | needed | unused | move-to-dev | move-to-peer | replace-with | heavy | deps |
|---|---|---|---|---|---|---|---|
| agent-room-mcp | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| app-shell | 7 | 0 | 0 | 1 | 1 | 0 | 9 |
| ax | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| build-control-policy | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| doc-render | 17 | 0 | 0 | 0 | 1 | 0 | 18 |
| git | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| hooks | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| i18n | 8 | 0 | 0 | 0 | 0 | 0 | 8 |
| legal | 3 | 0 | 0 | 0 | 0 | 0 | 3 |
| legal-ui | 12 | 0 | 0 | 0 | 0 | 0 | 12 |
| license | 4 | 0 | 0 | 0 | 0 | 0 | 4 |
| logs | 2 | 0 | 0 | 0 | 0 | 0 | 2 |
| media-ui | 11 | 0 | 0 | 0 | 1 | 1 | 13 |
| onboarding | 13 | 0 | 0 | 0 | 1 | 0 | 14 |
| pdf-ui | 8 | 0 | 0 | 0 | 1 | 0 | 9 |
| platform-ui | 5 | 0 | 0 | 0 | 0 | 0 | 5 |
| qa | 4 | 0 | 0 | 0 | 0 | 0 | 4 |
| qa-chrome | 3 | 0 | 0 | 0 | 0 | 0 | 3 |
| release | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| settings-ui | 14 | 0 | 3 | 0 | 1 | 0 | 18 |
| shell | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| tauri | 2 | 0 | 1 | 0 | 1 | 0 | 4 |
| theme | 3 | 0 | 0 | 0 | 1 | 0 | 4 |
| updates | 2 | 0 | 0 | 0 | 0 | 0 | 2 |
| **total** | 120 | 0 | 4 | 1 | 8 | 1 | 134 |

## agent-room-mcp

**Profile**

| Field | Value |
|---|---|
| Purpose | Private MCP stdio server exposing 12 room_* tools (join/post/vote/...) to an agent-room HTTP service. |
| Name / version | `@rightkit/agent-room-mcp` 0.1.0 |
| Private | yes |
| Source LOC | 237 (1 files); tests 123 LOC in 1 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 0 exports (no exports map: all files deep-importable), 0 bin |
| Test files | 1 |
| Rightkit dependents | none (private; no package or template dependent) |

**Dependencies**

None declared (no dependencies, devDependencies or peerDependencies). Imports only `node:*` builtins.

**Overlap / split / merge**

- No overlap in workspace; no dependencies at all (hand-rolled JSON-RPC over readline). Test runs `node --test src/index.test.ts` (TS type-stripping; Node >=22.6). Private and unreferenced by any other package; pairs with private crate `crates/agent-room-core` (README), so it is a candidate to live beside that crate rather than in the shared npm workspace.
- Split/merge: keep as is.

## app-shell

**Profile**

| Field | Value |
|---|---|
| Purpose | React app frame: sidebar+titlebar layout, command palette, shortcut registry, toasts, modals, context menus, small UI kit, theme store. |
| Name / version | `@rightkit/app-shell` 0.3.0 |
| Private | no |
| Source LOC | 2344 (19 files); tests 310 LOC in 1 files; aux (e2e/preview/config) 363 LOC |
| Entry points | 4 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | onboarding (peer+dev), settings-ui (peer) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/platform-ui` | dev+peer | workspace:* | 8/0/0 | needed | imported 8x in src (`bridge.ts:12` WindowControlAdapter, shortcuts); peer is correct, dev copy is the workspace link. |
| `@rightkit/qa` | dev | workspace:* | 0/0/1 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `react` | dev+peer | ^19.0.0 | 11/0/1 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 1/0/1 | move-to-peer | src/react/contextmenu.tsx:2 `import { createPortal } from "react-dom"` is a runtime import, but react-dom is only a devDependency and absent from peerDependencies. Add `react-dom >=18 <21` to peers. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vite` | dev | ^6.0.0 | 0/0/0 | needed | dev only: e2e harness build invoked by path (e2e/shell.e2e.test.ts:33); depcheck false-positive "unused". |
| `vitest` | dev | ^2.1.8 | 0/1/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 7, replace-with 1, move-to-peer 1.

**Overlap / split / merge**

- Theme store duplicated in intent with `theme`: app-shell/src/theme.ts (createThemeStore, resolveTheme, systemPrefersDark; framework-free, no React) vs theme/src/runtime.ts (`themeBootScript` reads the same storage key and the file comment says "pair with app-shell's theme store"). settings-ui must import createThemeStore from app-shell and applyBrand from theme. Move the store into `@rightkit/theme` so theme owns all theme logic.
- Token source of truth is split: app-shell/shell.css defines 152 `--rk-*` tokens by hand; theme/src/brand.ts:31-42 `SUITE_DEFAULTS` re-states colours and its comment says they are "defined by `@rightkit/app-shell/shell.css`". Generate one from the other.
- e2e/browser.ts (186 lines) is byte-identical in app-shell, theme, onboarding (and 39-line diff in settings-ui); it re-implements a CDP page driver (goto/eval/press/click/screenshot) although `@rightkit/qa` already ships `QaPage`/`QaBrowserSession` (qa/src/page.ts:43,89, cdp.ts). Delete 4 copies, use qa.
- Only 1 test file (e2e) for 2.3k LOC of src; vitest config includes `src/**/*.test.*` but none exist.
- Split/merge: do not merge with platform-ui (platform-ui is the Tauri-os leaf with its own pinned consumer, template depends on it without app-shell). Keep; move theme.ts out to theme.

## ax

**Profile**

| Field | Value |
|---|---|
| Purpose | `right-ax` CLI: agent-plugin scaffold/validate and AXA-1 static/conformance/behavioral/adversarial gates over a product MCP/CLI surface. |
| Name / version | `@rightkit/ax` 0.2.4 |
| Private | no |
| Source LOC | 2899 (19 files); tests 2254 LOC in 15 files; aux (e2e/preview/config) 206 LOC |
| Entry points | 10 exports, 1 bin (right-ax) |
| Test files | 15 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

None declared (no dependencies, devDependencies or peerDependencies). Imports only `node:*` builtins.

**Overlap / split / merge**

- Zero dependencies; 10 deep exports + bin. `ax/ownership-review.mjs` (216 LOC) builds ownership review packets: belongs with the ownership domain being split out of release, not in the AX conformance layer.
- `ax/lib/{redact,path-safety,json-schema-lite,version}.mjs` are private helpers; `git/` has its own `manifest.mjs` (1382 lines) and `adopt.mjs` with the same names but unrelated content (not duplicates).
- Split/merge: keep; extract ownership-review with the ownership split.

## build-control-policy

**Profile**

| Field | Value |
|---|---|
| Purpose | Agent write/session ownership hooks (Claude/Codex/Cline/CommandCode) plus installer for the content-addressed policy assets. |
| Name / version | `@rightkit/build-control-policy` 0.1.0-phase1 |
| Private | yes |
| Source LOC | 1822 (18 files); tests 915 LOC in 5 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 0 exports (no exports map: all files deep-importable), 0 bin |
| Test files | 5 |
| Rightkit dependents | none declared; receives 5 byte-identical mirrored files from release via rightkit-sync (release/rightkit-sync.mjs:185-187) |

**Dependencies**

None declared (no dependencies, devDependencies or peerDependencies). Imports only `node:*` builtins.

**Overlap / split / merge**

- Five files are byte-identical mirrors of release/ (background-process.mjs, ownership-availability.mjs, ownership-dependency-allowlist.mjs, ownership-source.mjs, rightkit-drift.mjs) plus `ownership-dependency-allowlist.json` and three generated catalogs, synced by release/rightkit-sync.mjs:185-187 (verified with cmp). ~350 LOC + a 1087-line JSON duplicated by design because installed hooks cannot import release.
- After the ownership split, the mirror source should be the new ownership package, not release.
- Three hook implementations without sharing: build-control-policy/ownership-write-hook.mjs (671 LOC), release/ownership-hooks.mjs (107, pre-commit installer), and `@rightkit/hooks` runtime (102). None imports another. `hooks` declares itself neutral runtime but build-control-policy does not use it.
- Private, version `0.1.0-phase1`, no deps, README only; tests 5 files. Split/merge: candidate to become `ownership` sibling package (policy/hook half) once release split lands.

## doc-render

**Profile**

| Field | Value |
|---|---|
| Purpose | Read-only Markdown/sanitized-HTML/email renderer (remark+shiki+DOMPurify) with block detectors (callout, FAQ, stepper, comparison). |
| Name / version | `@rightkit/doc-render` 0.1.4 |
| Private | no |
| Source LOC | 1687 (22 files); tests 62 LOC in 2 files; aux (e2e/preview/config) 63 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 2 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@types/mdast` | dep | ^4.0.4 | 0/0/0 | needed | correctly a runtime `dependency`: emitted dist/*.d.ts (footnotes, parseMarkdown, headingSlugs, remarkCitations, remarkWikiLink, remarkDefListAbbr) reference mdast types; 15 type-only imports of `mdast` in src. |
| `dompurify` | dep | ^3.4.16 | 1/0/0 | needed | security.ts:1; sanitization is the package's contract. 1.6 MB. |
| `github-slugger` | dep | ^2.0.0 | 2/0/0 | needed | headingSlugs.ts:2, footnotes.ts:29. |
| `remark-frontmatter` | dep | ^5.0.0 | 1/0/0 | needed | parseMarkdown.ts:3 (the only importer; 5 remark plugins are all single-use there). |
| `remark-gfm` | dep | ^4.0.1 | 1/0/0 | needed | parseMarkdown.ts:4. |
| `remark-math` | dep | ^6.0.0 | 1/0/0 | needed | parseMarkdown.ts:5 (math parse only; no KaTeX dependency remains). |
| `remark-parse` | dep | ^11.0.0 | 1/0/0 | needed | parseMarkdown.ts:6. |
| `remark-smartypants` | dep | ^3.0.3 | 1/0/0 | needed | parseMarkdown.ts:7. Smallest value of the five; candidate to drop if smart punctuation is not a contract. |
| `shiki` | dep | ^4.4.3 | 60/0/0 | needed | shikiHighlighter.ts: `shiki/core` + ~60 lazy `shiki/langs/*` imports (3.9 MB shiki + 11 MB @shikijs/langs installed). Lazy per-language import keeps bundle small, but install weight is the largest in this package. |
| `unified` | dep | ^11.0.5 | 1/0/0 | needed | parseMarkdown.ts:2. |
| `@rightkit/qa` | dev | workspace:* | 0/1/0 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `esbuild` | dev | ^0.28.1 | 0/1/0 | needed | dev only: bundles the real-browser e2e/preview harness. Same `^0.28.1` pin copied into doc-render, legal-ui, media-ui, pdf-ui (cross-cutting #4). |
| `react` | dev+peer | ^19.0.0 | 3/0/0 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 0/2/0 | needed | dev copy for tests/e2e harness; not imported by library source. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/2/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 17, replace-with 1.

Undeclared imports found: `mdast` (15/0/0) (type-only `mdast`, covered by `@types/mdast`).

**Overlap / split / merge**

- `@rightkit/qa` dev dep only for `qa/chrome` (see qa). Overlap with `pdf-ui`/`media-ui` is limited to duplicated e2e scaffolding: test/browser-e2e.mjs (27/122/297 lines) + test/harness.tsx (8/91/202) in doc-render, pdf-ui, media-ui, each re-implementing esbuild bundle + static server + Chrome launch.
- Generated `test/.e2e-out/` (12 MB doc-render, 62 MB media-ui, 62 MB pdf-ui: Chrome profile, model files, bundles) sits inside the package tree. It is git-ignored (.gitignore:3) but inflates any directory walk and risks being picked up by `files`/globs; point the Chrome profile at os.tmpdir().
- Split/merge: keep. Reader trio doc-render / pdf-ui / media-ui share only QA plumbing; no runtime overlap.

## git

**Profile**

| Field | Value |
|---|---|
| Purpose | `right-git`: public-repo GitHub Actions workflow templates, visibility gate, sync/drift/adopt/uninstall. |
| Name / version | `@rightkit/git` 0.2.33 |
| Private | no |
| Source LOC | 2389 (11 files); tests 2284 LOC in 10 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 0 exports (no exports map: all files deep-importable), 1 bin (right-git) |
| Test files | 10 |
| Rightkit dependents | none declared (reads consumers' `@rightkit/release` devDependency as data, git/sync.mjs:23-27) |

**Dependencies**

None declared (no dependencies, devDependencies or peerDependencies). Imports only `node:*` builtins.

**Overlap / split / merge**

- Imports nothing from release; the string `@rightkit/release` appears only as data it validates (sync.mjs:23-27). `adopt.mjs` and `manifest.mjs` share filenames with ax but not content.
- Split/merge: keep. 1382-line `manifest.mjs` is a candidate to be JSON/generated data rather than code (not verified).

## hooks

**Profile**

| Field | Value |
|---|---|
| Purpose | Neutral agent-hook lifecycle runtime (`HookHost`): normalize payload, run modules under a deadline, typed result. |
| Name / version | `@rightkit/hooks` 0.1.2 |
| Private | no |
| Source LOC | 102 (2 files); tests 76 LOC in 2 files; aux (e2e/preview/config) 2 LOC |
| Entry points | 1 exports, 1 bin (rightkit-hooks) |
| Test files | 2 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

None declared (no dependencies, devDependencies or peerDependencies). Imports only `node:*` builtins.

**Overlap / split / merge**

- See build-control-policy: no consumers inside the workspace. 102 LOC runtime, no deps.
- Split/merge: keep tiny; unclear why not a subpath of a bigger agent-tooling package, but not worth the churn.

## i18n

**Profile**

| Field | Value |
|---|---|
| Purpose | Locale catalog + React provider (`createI18n`, lazy locale loaders, RTL). |
| Name / version | `@rightkit/i18n` 0.1.0 |
| Private | no |
| Source LOC | 86 (1 files); tests 30 LOC in 1 files; aux (e2e/preview/config) 3 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@testing-library/jest-dom` | dev | ^6.6.3 | 0/1/0 | needed | test matchers (tests/setup only). |
| `@testing-library/react` | dev | ^16.1.0 | 0/1/0 | needed | component tests (tests only). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `jsdom` | dev | ^25.0.1 | 0/0/0 | needed | vitest DOM environment (selected by config/pragma, so zero import hits is expected). |
| `react` | dev+peer | ^19.0.0 | 1/0/0 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 8.

**Overlap / split / merge**

- No consumer in workspace or template; prior audit (docs/audits/2026-10-08-inventory-haiku-crosscutting.md:296) already flagged "zero consumers, delete if unused". 86 LOC, 8 devDependencies (jsdom, RTL, jest-dom, 2 @types, react, typescript, vitest) to test 86 LOC of source.
- Split/merge: delete or fold into app-shell if no app adopts it.

## legal

**Profile**

| Field | Value |
|---|---|
| Purpose | Legal manifest, acceptance receipt and material-digest primitives (pure TS). |
| Name / version | `@rightkit/legal` 0.3.1 |
| Private | no |
| Source LOC | 230 (1 files); tests 228 LOC in 1 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@types/node` | dev | ^22.10.0 | 0/0/0 | needed | Node typings for `node:*` imports. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 3.

**Overlap / split / merge**

- Pure-TS, 230 LOC. `legal-ui` re-declares the vocabulary instead of importing it: `LegalGateEligibilityBasis` (legal-ui/src/LegalGate.tsx:4) vs `LegalEligibilityBasis` (legal/src/index.ts:15). legal-ui README states the decoupling is deliberate (display-only).
- Split/merge: keep separate (pure vs React; different peer sets). Could expose legal-ui as `@rightkit/legal/ui` subpath with optional react peer to cut one package, but consumers (4 apps per prior audit) pin both independently.

## legal-ui

**Profile**

| Field | Value |
|---|---|
| Purpose | `LegalGate` React first-run / re-acceptance screen (display-only). |
| Name / version | `@rightkit/legal-ui` 0.2.0 |
| Private | no |
| Source LOC | 468 (2 files); tests 279 LOC in 2 files; aux (e2e/preview/config) 476 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 2 |
| Rightkit dependents | onboarding (peer+dev) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@testing-library/jest-dom` | dev | ^6.6.3 | 1/0/0 | needed | imported only by src/test-setup.ts:1 (counted as src because it lives in src/; confirm tsconfig excludes it from dist). |
| `@testing-library/react` | dev | ^16.1.0 | 0/3/0 | needed | component tests (tests only). |
| `@testing-library/user-event` | dev | ^14.5.2 | 0/1/0 | needed | interaction tests (tests only). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `axe-core` | dev | 4.12.1 | 0/0/1 | needed | dev only: preview/verify.mjs a11y check; exact-pinned `4.12.1`. |
| `esbuild` | dev | ^0.28.1 | 0/0/1 | needed | dev only: bundles the real-browser e2e/preview harness. Same `^0.28.1` pin copied into doc-render, legal-ui, media-ui, pdf-ui (cross-cutting #4). |
| `jsdom` | dev | ^25.0.1 | 0/0/0 | needed | vitest DOM environment (selected by config/pragma, so zero import hits is expected). |
| `react` | dev+peer | ^19.0.0 | 1/0/1 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 0/0/1 | needed | dev copy for tests/e2e harness; not imported by library source. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/2/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 12.

**Overlap / split / merge**

- See legal. Dev-dependency ratio is high: 12 devDeps for 468 LOC src (axe-core, esbuild, jsdom, 3x testing-library) because it owns a preview/a11y pipeline (preview/build.mjs, verify.mjs, 476 aux LOC).
- Split/merge: keep; onboarding embeds it (peer).

## license

**Profile**

| Field | Value |
|---|---|
| Purpose | License client: online token exchange with per-kind offline grace, tier/feature predicates. |
| Name / version | `@rightkit/license` 0.1.8 |
| Private | no |
| Source LOC | 299 (1 files); tests 252 LOC in 2 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 2 |
| Rightkit dependents | settings-ui (peer) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@tauri-apps/plugin-http` | dev+peer | ^2 | 1/0/0 | needed | already modelled correctly: optional peer (peerDependenciesMeta) + dev; loaded by dynamic `import()` at src/index.ts:110 so non-Tauri hosts never need it. |
| `@types/node` | dev | ^22.10.0 | 0/0/0 | needed | Node typings for `node:*` imports. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/2/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 4.

**Overlap / split / merge**

- Overlaps in shape (not code) with `logs`, `updates`, `tauri`: each defines its own `invoke`-like seam (`LogsInvoke` logs/src/index.ts:17, update host contract, license storage) while `tauri` wraps them again. Swift twin `RightKitLicense` shares golden vector swift/contracts/license-v2.json (duplicated into Tests/…/Contracts, same md5).
- Split/merge: keep.

## logs

**Profile**

| Field | Value |
|---|---|
| Purpose | Local-only structured logging/diagnostics client over three Tauri commands. |
| Name / version | `@rightkit/logs` 0.1.4 |
| Private | no |
| Source LOC | 190 (1 files); tests 73 LOC in 1 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | tauri (dep), settings-ui (peer), desktop-app-template (dep, pinned 0.1.4) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 2.

**Overlap / split / merge**

- `tauri` depends on it for 3 string constants only.
- Swift twin `RightKitDiagnostics` (150 LOC) and crate rightkit-logs (not in scope).
- Split/merge: keep.

## media-ui

**Profile**

| Field | Value |
|---|---|
| Purpose | Media player and image viewer/editor (fabric text overlay, react-image-crop). |
| Name / version | `@rightkit/media-ui` 0.3.0 |
| Private | no |
| Source LOC | 4487 (21 files); tests 649 LOC in 6 files; aux (e2e/preview/config) 499 LOC |
| Entry points | 3 exports, 0 bin |
| Test files | 6 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/qa` | dev | workspace:* | 0/1/0 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@testing-library/dom` | dev | ^10.4.0 | 0/0/0 | needed | required peer of @testing-library/react 16; zero direct imports is expected. |
| `@testing-library/react` | dev | ^16.1.0 | 0/2/0 | needed | component tests (tests only). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `esbuild` | dev | ^0.28.1 | 0/1/0 | needed | dev only: bundles the real-browser e2e/preview harness. Same `^0.28.1` pin copied into doc-render, legal-ui, media-ui, pdf-ui (cross-cutting #4). |
| `fabric` | dev+peer | ^7.4.0 | 1/0/0 | heavy | 28 MB install, used in ONE file (image/FabricTextOverlay.tsx:2: Canvas, Textbox for text overlay). Already an optional peer, so only image-editor consumers pay; consider replacing text overlay with DOM/canvas-2D and dropping fabric. |
| `jsdom` | dev | ^25.0.1 | 0/0/0 | needed | vitest DOM environment (selected by config/pragma, so zero import hits is expected). |
| `react` | dev+peer | ^19.0.0 | 13/2/0 | needed | peer + dev; 13 src imports. |
| `react-dom` | dev+peer | ^19.0.0 | 1/3/0 | needed | peer (src imports it) + dev. Correctly declared (unlike app-shell). |
| `react-image-crop` | dev+peer | ^11.1.2 | 2/0/0 | needed | image/ImageCropTool.tsx:2-3; small (168 KB); correctly optional peer + dev. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/6/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: replace-with 1, needed 11, heavy 1.

**Overlap / split / merge**

- Largest UI package (4.5k src LOC, 21 files); `./image` subpath already isolates fabric. `icons.tsx` (124 LOC) vs settings-ui/src/icons.tsx (19 LOC) minor icon duplication.
- Split/merge: split image editor (image/*, fabric + crop, ~2k LOC) into `@rightkit/image-ui` or keep subpath; subpath already works as a split. No change required.

## onboarding

**Profile**

| Field | Value |
|---|---|
| Purpose | First-run onboarding step framework (legal gate, permissions, models download, licence/trial). |
| Name / version | `@rightkit/onboarding` 0.3.0 |
| Private | no |
| Source LOC | 530 (5 files); tests 479 LOC in 2 files; aux (e2e/preview/config) 224 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 2 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/app-shell` | dev+peer | workspace:* | 2/0/0 | needed | peer: src/steps.tsx:2, Onboarding.tsx:3 import Button/Input/Badge from `@rightkit/app-shell/react` (three UI primitives pull the whole frame as a peer). |
| `@rightkit/legal-ui` | dev+peer | workspace:* | 2/0/0 | needed | peer: Onboarding.tsx:2 renders `LegalGate`; types.ts:2 type imports. |
| `@rightkit/qa` | dev | workspace:* | 0/0/1 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@testing-library/jest-dom` | dev | ^6.6.3 | 0/1/0 | needed | test matchers (tests/setup only). |
| `@testing-library/react` | dev | ^16.1.0 | 0/1/0 | needed | component tests (tests only). |
| `@testing-library/user-event` | dev | ^14.5.2 | 0/1/0 | needed | interaction tests (tests only). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `jsdom` | dev | ^25.0.1 | 0/0/0 | needed | vitest DOM environment (selected by config/pragma, so zero import hits is expected). |
| `react` | dev+peer | ^19.0.0 | 3/0/1 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 0/0/1 | needed | dev copy for tests/e2e harness; not imported by library source. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vite` | dev | ^6.0.0 | 0/0/1 | needed | dev only: e2e harness build (e2e/onboarding.e2e.test.ts:23). |
| `vitest` | dev | ^2.1.8 | 0/2/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 13, replace-with 1.

**Overlap / split / merge**

- `ModelEntry`/`ModelsBackend` interfaces duplicated verbatim in onboarding/src/types.ts:35,45 and settings-ui/src/types.ts:22,43; same for licence-state backend shapes (onboarding/src/types.ts:53-64). Move shared backend contracts to one place (e.g. `@rightkit/settings-ui` types or a new tiny `@rightkit/backends`).
- Peers app-shell + legal-ui for three Button/Input/Badge primitives and LegalGate.
- Split/merge: keep; extract shared backend types.

## pdf-ui

**Profile**

| Field | Value |
|---|---|
| Purpose | Read-only PDF viewer over pdf.js (search, links, text layer). |
| Name / version | `@rightkit/pdf-ui` 0.1.2 |
| Private | no |
| Source LOC | 842 (9 files); tests 28 LOC in 1 files; aux (e2e/preview/config) 213 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `pdfjs-dist` | dep | ^6.3.289 | 4/0/0 | needed | core of the package (PdfViewer.tsx:13-15, pdfJs.ts:4; worker URL pdf.worker.min.mjs). 35 MB installed: the heaviest dependency in the workspace, unavoidable for pdf.js. Declared as a regular dependency (apps get their own copy; consider peer so host controls worker/version). |
| `@rightkit/qa` | dev | workspace:* | 0/1/0 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `esbuild` | dev | ^0.28.1 | 0/1/0 | needed | dev only: bundles the real-browser e2e/preview harness. Same `^0.28.1` pin copied into doc-render, legal-ui, media-ui, pdf-ui (cross-cutting #4). |
| `react` | dev+peer | ^19.0.0 | 1/1/0 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 0/1/0 | needed | dev copy for tests/e2e harness; not imported by library source. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 8, replace-with 1.

**Overlap / split / merge**

- Only consumer-facing overlap is the duplicated e2e scaffold (see doc-render). `test/fixtures/*.pdf` 40 KB fine.
- Split/merge: keep.

## platform-ui

**Profile**

| Field | Value |
|---|---|
| Purpose | Platform detection, shortcut glyph labels, window-control adapter, QA platform bootstrap. |
| Name / version | `@rightkit/platform-ui` 0.2.0 |
| Private | no |
| Source LOC | 250 (2 files); tests 237 LOC in 4 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 4 |
| Rightkit dependents | app-shell (peer+dev), desktop-app-template (dep, pinned 0.1.2) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@tauri-apps/plugin-os` | dev+peer | ^2.3.2 | 1/0/0 | needed | static import at src/index.ts:1 makes this a hard requirement of the "brand-neutral" package; contrast license/ which uses dynamic import + optional peer. Consider the same lazy pattern so non-Tauri previews/tests need no mock (browser-platform.test.ts:3 vi.mocks it). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `react` | dev+peer | ^19.0.0 | 1/2/0 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/4/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 5.

**Overlap / split / merge**

- Small leaf (250 LOC): platform detect, shortcut glyph tables, WindowControlAdapter, QA platform bootstrap. `app-shell/src/shortcuts.ts` (342 LOC) and `app-shell/src/bridge.ts:12` consume it; `shell/src/index.ts` owns window actions (`WindowAction`, shell/src:68) and bridge.ts comment says WindowControlAdapter is "owned by platform-ui; shell returns an object that satisfies it": three packages describe the same window-control contract.
- Split/merge: keep leaf; do not merge into app-shell (template uses platform-ui alone).

## qa

**Profile**

| Field | Value |
|---|---|
| Purpose | QA evidence, run-owner locks, CDP page driver, loopback client, release-backdoor/guard validators. |
| Name / version | `@rightkit/qa` 0.4.0 |
| Private | no |
| Source LOC | 2270 (15 files); tests 1365 LOC in 13 files; aux (e2e/preview/config) 70 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 13 |
| Rightkit dependents | dev-only (all via `@rightkit/qa/chrome`): app-shell, doc-render, media-ui, onboarding, pdf-ui, settings-ui, theme |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/qa-chrome` | dep | workspace:* | 1/1/0 | needed | only runtime dep; used solely by `src/chrome.ts:1` (`export * from "@rightkit/qa-chrome"`) and one integration test. See merge note. |
| `@types/node` | dev | ^24.0.0 | 0/0/0 | needed | Node typings for `node:*` imports. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/12/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 4.

**Overlap / split / merge**

- `src/chrome.ts` is a one-line re-export of qa-chrome and is the ONLY qa API the workspace consumes (7 dev consumers). `ownership.ts` (run-owner locks) overlaps conceptually with release/process-liveness.mjs and background-process; `isProcessAlive` exists in qa/src/process.ts and release/process-liveness.mjs (TS vs mjs).
- README claims private but package.json is public with `./chrome` export.
- Split/merge: MERGE candidate with qa-chrome (see qa-chrome).

## qa-chrome

**Profile**

| Field | Value |
|---|---|
| Purpose | Zero-dependency Chrome-for-Testing resolver/installer/launcher/graceful close (+ `rightkit-qa-chrome` bin). |
| Name / version | `@rightkit/qa-chrome` 0.1.3 |
| Private | no |
| Source LOC | 638 (3 files); tests 104 LOC in 1 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 1 bin (rightkit-qa-chrome) |
| Test files | 1 |
| Rightkit dependents | qa (dep) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@types/node` | dev | ^24.0.0 | 0/0/0 | needed | Node typings for `node:*` imports. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 3.

**Overlap / split / merge**

- Superset relationship: qa depends on it (only dep) and re-exports it whole; 3 external apps depend directly on qa-chrome per prior audit (unverified). Zero-dep property is preserved if merged since qa has no other runtime deps.
- Split/merge: MERGE qa-chrome into qa as `@rightkit/qa/chrome` (keep `rightkit-qa-chrome` bin), publish a shim, or vice-versa have workspace dev consumers depend on qa-chrome directly. Either removes 1 package and 1 release stream.

## release

**Profile**

| Field | Value |
|---|---|
| Purpose | `right-release` CLI/SDK: build, sign, notarize, verify, publish (GitHub Releases / R2), ownership admission and key trust, Swift/MSIX/Cargo publishing. |
| Name / version | `@rightkit/release` 0.2.147 |
| Private | no |
| Source LOC | 23774 (138 files); tests 20145 LOC in 120 files; aux (e2e/preview/config) 952 LOC |
| Entry points | 0 exports (no exports map: all files deep-importable), 1 bin (right-release) |
| Test files | 120 |
| Rightkit dependents | desktop-app-template (dev, pinned 0.2.119 vs workspace 0.2.147); git reads it as data only |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `smol-toml` | dep | 1.7.0 | 5/1/0 | needed | 5 src files parse TOML (Cargo/rust-toolchain manifests); exact-pinned 1.7.0, 320 KB. Fine. |

Verdict counts: needed 1.

Undeclared imports found: `puppeteer` (0/1/0), `puppeteer-core` (0/2/0) (only test-string/fixture mentions: puppeteer is a banned-dependency fixture, not an import).

**Overlap / split / merge**

- See split table below. Overlaps: `background-process.mjs` (51 LOC) is imported by 76 non-test release files (16 of them ownership-*) and mirrored verbatim into build-control-policy; `managed-cargo.mjs`/`managed-roots.mjs` are recorded as still in release pending audit 4.1 (docs/audits/2026-10-10-plan-vs-current.md:97), and `heavy-command.mjs`/`cargo-guard.mjs` sit in the same build-control territory (adjacency only, not verified as duplication); `release-qa-*` (630 LOC) is adjacent to `@rightkit/qa` guard/evidence code (not verified as duplication); `publish-swift.mjs` + `swift-*` overlap with stale `swift/scripts/archive-package.mjs` (still present; only its own test references it).
- No `exports` map: every file is deep-importable and part of the public surface; `files` ships `*.json` including rightkit-ownership.json (4204 lines) and ownership-dependency-allowlist.json (1087).
- Split/merge: SPLIT (ownership first).

## settings-ui

**Profile**

| Field | Value |
|---|---|
| Purpose | Settings panel frame + standard sections (appearance, updates, license, legal, diagnostics, models, API keys, about). |
| Name / version | `@rightkit/settings-ui` 0.3.0 |
| Private | no |
| Source LOC | 2079 (11 files); tests 1280 LOC in 5 files; aux (e2e/preview/config) 571 LOC |
| Entry points | 2 exports, 0 bin |
| Test files | 5 |
| Rightkit dependents | none in workspace / template |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/qa` | dev | workspace:* | 0/0/1 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@rightkit/shell` | dev+peer | workspace:* | 1/0/0 | move-to-dev | peer + dev, but the ONLY import is `import type { Shell }` (src/backends.ts:11) and the file comment says it is "structural, so no import cycle". A type-only structural peer forces every host to install @rightkit/shell. Make it dev-only (or optional peer). |
| `@testing-library/jest-dom` | dev | ^6.6.3 | 0/1/0 | needed | test matchers (tests/setup only). |
| `@testing-library/react` | dev | ^16.1.0 | 0/3/0 | needed | component tests (tests only). |
| `@testing-library/user-event` | dev | ^14.5.2 | 0/2/0 | needed | interaction tests (tests only). |
| `@types/react` | dev | ^19.0.0 | 0/0/0 | needed | types for TSX source (dev-only is correct for a peer-dep React package). |
| `@types/react-dom` | dev | ^19.0.0 | 0/0/0 | needed | types for react-dom imports in source/tests. |
| `jsdom` | dev | ^25.0.1 | 0/0/0 | needed | vitest DOM environment (selected by config/pragma, so zero import hits is expected). |
| `react` | dev+peer | ^19.0.0 | 7/2/1 | needed | peer (host provides) + dev copy for tests; modelled correctly. |
| `react-dom` | dev | ^19.0.0 | 0/1/1 | needed | dev copy for tests/e2e harness; not imported by library source. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vite` | dev | ^6.0.0 | 0/0/0 | needed | dev only: e2e harness build via path (e2e/settings.e2e.test.ts:51); depcheck false-positive. |
| `vitest` | dev | ^2.1.8 | 0/5/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |
| `@rightkit/app-shell` | peer | >=0.2.0 <1 | 1/0/2 | needed | peer: runtime imports Badge/Button/Input/SegmentedControl/Toggle/createThemeStore/useMediaQuery/useShellOptional from `@rightkit/app-shell/react` (sections.tsx:2). |
| `@rightkit/license` | peer | >=0.1.0 | 2/3/1 | needed | peer: runtime imports `isLicensed` (sections.tsx:3), `isPro`, `tier` (hooks.ts:6). |
| `@rightkit/logs` | peer | >=0.1.0 | 1/0/1 | move-to-dev | peer, but type-only (`import type { LogsClient }`, sections.tsx:4). Optional peer/dev suffices. (Used at runtime only by e2e/harness/main.tsx:6.) |
| `@rightkit/theme` | peer | >=0.2.0 <1 | 1/0/0 | needed | peer: runtime `applyBrand`, `brandFromAccent` (sections.tsx:5). Note app-shell also supplies the theme store: one concern split across two packages. |
| `@rightkit/updates` | peer | >=0.2.0 | 1/2/1 | move-to-dev | peer, but type-only in src (`AutoUpdateClient`, `AutoUpdateStatus`, sections.tsx:6). Runtime use only in e2e harness (main.tsx:4) and tests. |

Verdict counts: replace-with 1, move-to-dev 3, needed 14.

**Overlap / split / merge**

- Six rightkit peers (app-shell, shell, license, logs, theme, updates) + react; three of them type-only (see table). `hooks.ts` (349 LOC) is settings-specific, not a duplicate of app-shell hooks.ts (107). Models section duplicates onboarding's ModelsStep contracts.
- Split/merge: keep; reduce peers to {app-shell, license, theme}.

## shell

**Profile**

| Field | Value |
|---|---|
| Purpose | Typed renderer bridge for the rightkit-shell Tauri crate (window, menu/tray, deep links, autostart, clipboard, engine calls). |
| Name / version | `@rightkit/shell` 0.1.0 |
| Private | no |
| Source LOC | 376 (1 files); tests 0 LOC in 0 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 0 (no test script either) |
| Rightkit dependents | settings-ui (peer+dev, type-only import) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |

Verdict counts: needed 1.

**Overlap / split / merge**

- No tests and no `test` script (the only package with 0 test files); build only. Defines `Invoke` (shell/src/index.ts:11) identically to tauri/src/index.ts:8 and logs `LogsInvoke`.
- Split/merge: MERGE candidate with tauri as `@rightkit/tauri/shell` (both are "typed Tauri invoke wrappers"; tauri 247 LOC + shell 376 LOC). Lower priority since shell is zero-dep and tauri has a runtime dep.

## tauri

**Profile**

| Field | Value |
|---|---|
| Purpose | Typed Tauri host adapters: license/logs/updates/device command constants and per-app client constructors. |
| Name / version | `@rightkit/tauri` 0.2.0 |
| Private | no |
| Source LOC | 247 (1 files); tests 285 LOC in 1 files; aux (e2e/preview/config) 43 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | desktop-app-template (dep, pinned 0.1.1 vs workspace 0.2.0) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/logs` | dep | ^0.1.4 | 1/0/0 | replace-with | regular `dependency`, but src imports only three string constants (`RIGHTKIT_LOGS_*_COMMAND`, index.ts:2-6). Forces every consumer to install @rightkit/logs at an exact `^0.1.4` range. Inline the constants, or make logs an (optional) peer like updates. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |
| `@rightkit/updates` | peer | ^0.2.5 | 1/0/0 | move-to-dev | peer, but type-only (`import type { AutoUpdateClient }`, index.ts:1). Optional peer/dev suffices. |

Verdict counts: replace-with 1, needed 2, move-to-dev 1.

**Overlap / split / merge**

- Contains product facts in shared code: per-app constructors `createViewRightTauriClient`, `createScrapeRightTauriClient`, ... `createCodeRightTauriClient` (index.ts:173-223): adding an app requires a shared-package release. Command constants for logs/updates/license duplicate those owned by logs/updates/license.
- Split/merge: merge with shell or fold constants into the owning packages.

## theme

**Profile**

| Field | Value |
|---|---|
| Purpose | Brand-file to CSS-token generator, WCAG contrast + hard-coded-colour checks, runtime `applyBrand`, boot script, `rightkit-theme` bin. |
| Name / version | `@rightkit/theme` 0.2.1 |
| Private | no |
| Source LOC | 559 (8 files); tests 242 LOC in 1 files; aux (e2e/preview/config) 191 LOC |
| Entry points | 2 exports, 1 bin (rightkit-theme) |
| Test files | 1 |
| Rightkit dependents | settings-ui (peer) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `@rightkit/qa` | dev | workspace:* | 0/0/1 | replace-with | dev only; every consumer imports just `@rightkit/qa/chrome`, a one-line re-export of `@rightkit/qa-chrome` (qa/src/chrome.ts:1). Depend on `@rightkit/qa-chrome` directly (or merge qa-chrome into qa). |
| `@types/node` | dev | ^24.0.0 | 0/0/0 | needed | Node typings for `node:*` imports. |
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/1 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: replace-with 1, needed 3.

**Overlap / split / merge**

- See app-shell (theme store and token source split). Bin `rightkit-theme` + `./node` subpath keep Node code out of browser entry (good).
- Split/merge: keep; absorb app-shell/src/theme.ts.

## updates

**Profile**

| Field | Value |
|---|---|
| Purpose | Runtime auto-update client/policy helper for Tauri apps (patch vs update tier, release-status fetch). |
| Name / version | `@rightkit/updates` 0.2.5 |
| Private | no |
| Source LOC | 304 (1 files); tests 564 LOC in 1 files; aux (e2e/preview/config) 0 LOC |
| Entry points | 1 exports, 0 bin |
| Test files | 1 |
| Rightkit dependents | tauri (peer), settings-ui (peer), desktop-app-template (dep, pinned 0.2.4) |

**Dependencies**

| Dependency | Kind | Version | Hits src/test/script | Verdict | Reason |
|---|---|---|---|---|---|
| `typescript` | dev | ^5.7.0 | 0/0/0 | needed | build + typecheck (`tsc`); identical `^5.7.0` literal is repeated in every TS package and not hoisted (cross-cutting #1). |
| `vitest` | dev | ^2.1.8 | 0/1/0 | needed | test runner. `^2.1.8` resolves to vitest 2.1.9 which pulls vite 5.4.21 while direct `vite ^6` resolves 6.4.3 (two vite majors, three esbuild versions in pnpm-lock.yaml). |

Verdict counts: needed 2.

**Overlap / split / merge**

- `tauri` peers on it; template pins 0.2.4. Single file 304 LOC; tests are 565 LOC (host contract vectors exported in the runtime bundle as `UPDATE_HOST_CONTRACT_VECTORS`, index.ts:12 — test data in runtime API).
- Split/merge: keep.


## packages/release: responsibility groups and the ownership split (audit item 4.2)

`packages/release` has no `exports` map, ships 275 top-level files (138 source .mjs/.sh/.py incl. `cli/`, 120 test files) and is 23,855 source LOC (23,774 .mjs + 81 .py/.sh) with 20,145 test LOC. Grouping is by filename; every source file is in exactly one group (scratchpad/audit/group.mjs).

| Group | Files (src) | Src LOC | Test files | Test LOC | Notes |
|---|---|---|---|---|---|
| ownership (`ownership*.mjs`) | 27 | 5,585 | 27 | 5,250 | plus 5,547 lines of JSON data (rightkit-ownership.json 4,204; ownership-dependency-allowlist.json 1,087; declaration 186; others) and 2.9 MB `fixtures/ownership` |
| build / packaging (build-release, dmg-*, nsis-*, tauri-bundle-marker, native-release-finalization, runtime-artifact-manifest, addon-*, asr-*, model-promote, target-bridge) | 15 | 2,982 | 13 | 2,118 | build-release.mjs alone 781 |
| signing / notarization (sign-*, codesign-*, macos-*, notar*, signing-*, rightkit-signing-agent, apple-profile-renew, protected-credentials, osascript-guard, development-signing) | 28 | 2,511 | 17 | 2,093 | candidate `@rightkit/signing` |
| publish (publish-update, publish-cargo, publication-*, github-release, upload-*, artifact-storage, registry-parity, r2/prune, mirror-root-artifact) | 16 | 2,226 | 14 | 1,551 | `publish-swift` counted under swift/msix |
| evidence / gates (inventory-check 980, supply-chain-evidence, hardening*, verifier, source-gate, committed-source, legal-contract, standalone-clone-*) | 10 | 2,114 | 10 | 933 | committed-source and standalone-clone-verify are imported by ownership |
| core orchestrator (release.mjs 1,150, release-state, release-invocation, preflight) | 4 | 1,985 | 4 | 1,671 | what should remain |
| dev / process infra (development*, portable-development, heavy-command, cache-*, background-process, process-liveness, progress-control, deps) | 12 | 1,847 | 10 | 1,299 | background-process is mirrored into build-control-policy |
| install-app (install-app 709, direct-bootstrap 643, installer-storage-doctor) | 3 | 1,481 | 4 | 1,428 | a runtime installer, not release orchestration |
| swift / msix (swift-*, msix-*, publish-swift) | 11 | 992 | 4 | 522 | |
| cargo (cargo-contract, cargo-guard, cargo-target, native-cargo-layout) | 4 | 790 | 3 | 367 | overlaps RustRight work (audit 4.1) |
| qa (qa-contract, release-qa-audit/command/receipt) | 4 | 630 | 3 | 443 | receipt imports ownership-approval/lockfiles/policy-bundle (release-qa-receipt.mjs:17-22) |
| rightkit-sync / drift | 2 | 334 | 2 | 375 | mirrors files into build-control-policy (rightkit-sync.mjs:185-187) |
| cli dispatcher (`cli/right-release.mjs`) | 1 | 241 | | | dispatches ownership, ownership-approval, ownership-report, hooks (cli/right-release.mjs:27-29,70) |
| managed-* (managed-cargo 68, managed-host-secrets 30, managed-roots 39) | 3 | 137 | 2 | 248 | still in release pending audit 4.1 (plan-vs-current.md:97) |
| cross-cutting contract tests (right-suite-contract 1,027, pipeline-normalization, release-cli-contract, ...) | | | 6 (+1 unclassified, 156 LOC) | 1,691 | |
| **Total** | **138** | **23,855** | **120** | **20,145** | |

Ownership (5,585 LOC) is 23% of release source and 26% of its tests. Internal sub-groups: key trust/authority/command/elevation/log + windows presence 1,851 LOC (6 files); scan/admission/inventory/lockfiles/staged/hooks 2,256 (12); evidence/report/policy bundle/trust summary 767 (5); approval 501 (2); dependency allowlist 210 (2).

Seams that decide the split (all verified by grep):

- Ownership imports out of release only four modules: `background-process.mjs` (16 ownership files), `committed-source.mjs` (ownership.mjs:13, ownership-evidence.mjs:7), `standalone-clone-verify.mjs` (`DEFAULT_APPS`, ownership-report.mjs:4) and `installer-storage-doctor.mjs` (ownership-report.mjs:6), plus `rightkit-ownership.json` (ownership.mjs:5, ownership-approval-command.mjs:8). The last two are only in `ownership-report` and are the awkward ones; inject them or move the report.
- Release imports into ownership: `verifyOwnershipEvidence`/`createOwnershipEvidence` (preflight.mjs:25, publication-artifacts.mjs:5, upload-release.mjs:11); `canonicalJson`, `canonicalRepositoryIdentity`, `OWNERSHIP_LOCKFILES`, policy bundle, `SCANNER_VERSION` (release-qa-receipt.mjs:17-22). Direction release to ownership is clean; keep it one-way.
- Three non-release copies already exist: build-control-policy mirrors 5 identical source files + allowlist JSON + 3 catalogs via rightkit-sync; ax has `ownership-review.mjs` (216 LOC); release/ownership-catalog.test.mjs:20 reads build-control-policy files. A new `@rightkit/ownership` (or `ownership-core`) becomes the single source for the mirror, the hooks package, and ax review.
- Suggested order after ownership: signing (2.5k, self-contained credentials/agent surface), install-app (1.5k, not a release concern), cargo + managed-* (follow audit 4.1). Release core would shrink from 23.9k to about 18k (ownership out) or about 12k (all four out).

## Cross-cutting findings

1. Dev tooling is repeated per package and not hoisted: root package.json has no devDependencies and pnpm-workspace.yaml has no `catalog:`. The same literals appear in typescript ^5.7.0 (18 packages), vitest ^2.1.8 (17), react ^19.0.0 (9) + @types/react (9) + @types/react-dom (8) + react-dom (7), jsdom ^25.0.1 (5), @testing-library/react (5), esbuild ^0.28.1 (4), jest-dom (4), vite ^6.0.0 (3), user-event (3). `@types/node` already drifts: ^22.10.0 (legal, license) vs ^24.0.0 (qa, qa-chrome, theme), and pnpm-lock.yaml carries both 22.20.0 and 24.13.3, jsdom 25.0.1 and 26.1.0, vite 5.4.21 and 6.4.3, esbuild 0.21.5/0.25.12/0.28.1. Devdeps do not reach consumers, so moving to pnpm catalogs (or root devDependencies) is safe and removes about 100 repeated lines.
2. E2E scaffolding is copy-pasted: `e2e/browser.ts` is byte-identical in app-shell, theme, onboarding (186 lines each; settings-ui 206 with a 39-line diff); doc-render/pdf-ui/media-ui each have their own `test/browser-e2e.mjs` + `harness.tsx` (27/122/297 and 8/91/202 lines). All 7 packages import only `@rightkit/qa/chrome`, which is a re-export of qa-chrome (qa/src/chrome.ts:1), while qa itself ships a CDP page driver (qa/src/page.ts:43,89, cdp.ts) they do not use. Move a shared static-server + esbuild-harness helper into qa and delete about 1,000 lines.
3. Token source of truth is split (app-shell/shell.css 152 `--rk-*` tokens vs theme/src/brand.ts:31-42 SUITE_DEFAULTS), and theme logic is split (app-shell/src/theme.ts vs theme/src/runtime.ts:39).
4. Backend contracts duplicated: `ModelEntry`/`ModelsBackend` in onboarding/src/types.ts:35,45 and settings-ui/src/types.ts:22,43.
5. Peer-dependency accuracy: app-shell imports `react-dom` at runtime (src/react/contextmenu.tsx:2) without declaring it; settings-ui peers on shell/logs/updates are type-only; tauri hard-depends on logs for three constants.
6. Template pins lag the workspace: desktop-app-template/package.json pins @rightkit/platform-ui 0.1.2 (workspace 0.2.0), tauri 0.1.1 (0.2.0), updates 0.2.4 (0.2.5), release 0.2.119 (0.2.147); logs 0.1.4 matches.
7. Test gaps: `shell` has no tests and no test script; `app-shell` (2.3k LOC) has one e2e test file and no unit tests despite vitest `src/**/*.test.*` include; `hooks`, `i18n`, `logs`, `updates`, `legal`, `license` have one test file each (fine for size).
8. Generated `.e2e-out` Chrome profiles live inside doc-render/media-ui/pdf-ui test dirs (12/62/62 MB, ignored by .gitignore:3).
9. `agent-room-mcp` and `build-control-policy` are private, have no declared dependencies, and no consumers in this workspace.

## Swift package: swift/RightKitSwift (Package.swift dependencies only)

- Dependencies: none. Package.swift has no `dependencies:` block (comment: "Foundation/CryptoKit only: no third-party dependency", Package.swift:5-6). Only system link: `linkedLibrary("z")` for RightKitArtifacts (Package.swift:23). swift-tools-version 6.0, iOS 17 / macOS 14, VERSION 0.3.8.
- Apple-framework imports in Sources (counts): Foundation 61, CoreML 10, AVFoundation 8, CryptoKit 4, Security 3; single uses of CloudKit, StoreKit, SwiftUI, Speech, Network, WatchConnectivity.
- 14 library products/targets, 11,101 source LOC, 5,323 test LOC:

| Target | Src LOC | Files | Internal dep |
|---|---|---|---|
| RightKitSpeech | 3,915 | 23 | none (bundled Resources/whisper_mel_128.bin 104 KB) |
| RightKitCloudSync | 2,186 | 9 | none |
| RightKitLicense | 1,534 | 3 | none |
| RightKitPurchases | 874 | 7 | RightKitLicense |
| RightKitStore | 506 | 5 | none |
| RightKitImportQueue | 432 | 4 | RightKitStore |
| RightKitAppleCredentials | 373 | 1 | none |
| RightKitArtifacts | 259 | 1 | none (zlib) |
| RightKitWatchBridge | 247 | 2 | none |
| RightKitLiveActivity | 177 | 2 | none |
| RightKitOverlay | 166 | 1 | none |
| RightKitDiagnostics | 150 | 2 | none |
| RightKitUI | 143 | 1 | none |
| RightKitIntents | 139 | 1 | none |

- Observations:
  - RightKitSpeech is 35% of Swift source and is the only target with a bundled model resource; the prior audit (docs/audits/2026-10-08-inventory-luna-npm-swift-crosscutting.md, not re-verified) lists HeardRight as its only consumer, as for Intents/LiveActivity/WatchBridge/Store/ImportQueue/Overlay. These are app-specific features inside a "shared platform infrastructure" package (Package.swift:4-5 says it owns licensing, diagnostics, artifact install and QA seams, never app UI or product policy; RightKitUI/Overlay/Speech/Intents sit outside that stated charter). Candidate split: keep License/Diagnostics/Artifacts/Purchases/CloudSync/Store/AppleCredentials as core; move Speech, Intents, LiveActivity, WatchBridge, ImportQueue, Overlay, UI to an app-feature package, so core consumers do not resolve CoreML/Speech targets.
  - Cross-language twins: RightKitLicense vs `@rightkit/license` (shared golden vector swift/contracts/license-v2.json, md5-identical copy under Tests/RightKitLicenseTests/Contracts/license-v2.json; root script test:swift-contracts); RightKitDiagnostics vs `@rightkit/logs`; RightKitArtifacts vs release `artifact-storage.mjs`/`install-app.mjs`.
  - Second Package.swift: swift/RightKitCloudKitHelper depends on RightKitSwift by `.package(path: "../RightKitSwift")` (its Package.swift:12), while rightkit/AGENTS.md says RightKitSwift is pinned by URL + tag, never local path (the helper is in-repo, but confirm the rule's scope). A third, unrelated manifest exists at crates/rightkit-record/helpers/macos/Package.swift (local RecordWire target only).
  - Stale: swift/scripts/archive-package.mjs (51 LOC) + test (25 LOC) is referenced only by its own test (prior audit item, still present); superseded by release/publish-swift.mjs.
  - Ignored Arcane receipt directory `swift/RightKitSwift/Sources/RightKitLicense/.audit/` sits inside a library target (gitignored by .gitignore:12; not shipped by git archive, but present on disk in the target dir).
