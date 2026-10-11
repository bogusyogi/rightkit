# 1. Dependencies & publication readiness

**Decision:** perform two bounded dependency removals after focused validation. Keep `windows-core`: current proc-macro expansion uses it. Publication & runtime evidence gaps are more significant than dependency pruning.

Fresh source review, 11 October 2026. Baseline HEAD `f60f116514467e83952f5e5c12a71b34e3341389` plus existing working-tree changes. Earlier audit conclusions were excluded. Each assertion below comes from current manifests, source, local lifecycle records or this run's evidence. No product source was edited or published.

Scope covers 25 npm packages, 57 workspace Rust crates, 10 secondary Cargo packages, root/template npm manifests, 15 RightKitSwift products, CloudKit helper & recording helpers. Publication inventory counts **84 units: 71 public, 12 private, 1 internal helper**; Swift's 15 library products belong to one release unit. App discovery covers 20 current first-party product/tool checkouts, retained Membrane snapshots, archives, workflow roots & 9 installed first-party bundles. These counts describe different populations & must not be added together.

Evidence labels: **source-confirmed** means a bounded source path supports the finding; **candidate** needs execution or compatibility proof; **recorded** means local release ledger, not remote registry confirmation; **integrated** means a source call, not just a manifest entry; **qualified** requires executed target/installed evidence. Existing tests were inspected, not rerun. “Satisfactory” in package rows is a source-review verdict, never a test pass.

The native Legion run remains **incomplete**: 17 of 79 providers completed; packets include generated/cache material & bounded omissions, Blueprint graph is unavailable, & project execution lacks a host network-sandbox receipt. `legion verify` validates report consistency, not release readiness. Its 6,678 raw scanner candidates are not accepted defects. Independent bounded lens results & source reviews supply the findings in these reports.

[Legion Audit SKILL.md](</Users/adrdsouza/.claude/skills/legion/skills/audit/SKILL.md:59>) states: “Project-executing checks need trusted host network-sandbox receipt; without it they are `UNPROVEN`; file-only providers still run.” Its [execution contract](</Users/adrdsouza/.claude/skills/legion/skills/audit/references/execution-contract.md>) requires skipping builds/tests/runtime without that receipt. That is why no new package build, installed-scale benchmark or Apple simulator/device test is claimed here. Both [iOS](</Users/adrdsouza/.claude/skills/legion/skills/ios-development/SKILL.md>) & [macOS](</Users/adrdsouza/.claude/skills/legion/skills/macos-development/SKILL.md>) skills were applied. Apple native catalog/preflight & local tool versions were inspected.

Snapshot receipts: [initial-state.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/initial-state.json>), [current-inventory.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/current-inventory.json>), [source-hashes.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-hashes.json>), [source-drift.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/source-drift.json>), [native-summary.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/native-summary.json>). Mechanical source census is not an authoritative symbol graph. Nested fixture/vendor/cache/generated/personal-data boundaries are recorded explicitly; no absence-of-import result proves absence of runtime use.

This was a live review interval, not an immutable checkout. Of 36,395 initially hashed files, 59 changed during review; none disappeared at comparison time. Changes span RightKit, CutRight, ScrapeRight, ViewRight, RustRight & SwiftRight. Local override counts & affected dependency declarations were reread. No unchanged-source or reproducible-build claim applies to concurrently changed files; closure uses the recorded hashes & final chosen release revision.

## Prioritized decisions

| ID | Priority / status | Owner & finding | Acceptance |
|---|---|---|---|
| D01 | P3 · candidate | [settings-ui](</Volumes/D/claude/rightkit/packages/settings-ui/package.json:33>): Remove unused shell peer, optional-peer metadata & dev dependency | Focused type/test & packed-consumer install; retain emitted type dependencies. |
| D02 | P3 · candidate | [desktop template](</Volumes/D/claude/rightkit/desktop-app-template/src-tauri/Cargo.toml:41>): Remove direct rightkit-service dependency; protocol crate already owns it | Compile template on supported targets, retaining protocol dependency. |
| D03 | P1 · source-confirmed | [npm publication](</Volumes/D/claude/rightkit/packages/app-shell/package.json:40>): Publish platform-ui 0.2.2 before app-shell 0.6.1 peer floor | Coherent registry-confirmed peer set & isolated install. |
| P01 | P1 · qualification gap | [all public units](</Volumes/D/claude/rightkit/packages/release/publication-gate.mjs:34>): Current publication receipts do not qualify this source revision | Exact version/HEAD, clean unit path, host results & artifact hash per release. |
| P02 | P1 · source-confirmed | [release runtime gate](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:88>): README regex gate does not verify measured budgets or installed adoption | Structured measurement/status/scale artifacts checked by publish & adoption gates. |
| P03 | P1 · qualification gap | [continuous crates](</Volumes/D/claude/rightkit/docs/RUNTIME-BUDGETS.md:7>): All 11 named owners lack required Runtime budget sections; no fresh scale runs | Declare ceilings, measure on representative data, expose status & enforce receipts. |

## Dependency review boundaries

Npm review covers 27 manifests: root, 25 packages & desktop template. Rust review covers 57 workspace crates plus 10 secondary packages, with 445 direct dependency edges. Target conditions, optional features, renamed crates, workspace inheritance, proc macros, build scripts, tests & emitted TypeScript declarations are separate use classes. A type-only or macro-generated import can still require a dependency.

All direct edges remain available in [rightkit-dependencies.csv](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/rightkit-dependencies.csv>), [Rust edge ledger](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/rust-dependency-edges.json>) & per-package rows below. Swift package has no external third-party package dependency; its target edges, helper edge & app pins are in report 2/3. No transitive vulnerability, license-compliance, outdated-registry or binary-size clearance is claimed.

Older app pins are alignment inventory, not automatic defects. Unpublished newer source must not be substituted into consumers merely to make versions match. `registry-recorded` values require registry/archive verification in publication work.

## npm dependency review — 2026-10-11

## Scope & method

File-only review of root `package.json`, 25 tracked `packages/*/package.json` manifests, & `desktop-app-template/package.json` (27 manifests). Read current source, tests, build configs, lockfiles, workspace policy, exports, package `files`, & `packages/release/rightkit-versions.json`. No install, network, build, test, publication, or product edits ran.

`use` labels: `runtime` reaches emitted JS; `type` reaches emitted declarations or compiler; `test` is test-only; `build` is config/CLI/build-only. `runtime+test` means both. Every item below is declared directly in current manifest; Node built-ins excluded.

## Findings

1. **Confirmed unnecessary peer & development edge — `@rightkit/settings-ui` → `@rightkit/shell`.** Both peer `>=0.1.0 <1` & dev `workspace:*` appear at `packages/settings-ui/package.json:33,42`, but source has no import; `shell-adapter` declares structural interfaces specifically to avoid runtime **or type** import at `packages/settings-ui/src/shell-adapter.ts:3-5,18-26`. Current emitted declaration contains comments only, no shell module specifier. Remove only after focused package type/test & packed-consumer verification; no token-only edit is justified.

2. **Confirmed source/lifecycle peer-release mismatch.** Current unstaged `@rightkit/app-shell` `0.6.1` requires `@rightkit/platform-ui >=0.2.2 <1` at `packages/app-shell/package.json:3,40`, while lifecycle records only `app-shell 0.6.0` & `platform-ui 0.2.1` as staged/registry at `packages/release/rightkit-versions.json:16,29,417,430`. Current source `platform-ui` is `0.2.2` at `packages/platform-ui/package.json:3`, but has no matching staged/registry record. Publishing app-shell source before platform-ui `0.2.2` is staged & registry-confirmed produces an unsatisfied peer floor.

### Candidates, not confirmed defects

- `@rightkit/app-shell` repeats optional `@tauri-apps/plugin-os` peer at `packages/app-shell/package.json:41,75-78`; platform-ui already owns lazy loading plus optional peer at `packages/platform-ui/package.json:32,58-61` & `packages/platform-ui/src/index.ts:47-79`. Keep until packed app-shell consumer proves peer warning/loader behavior unchanged.
- Public, unpublished `@rightkit/update` source uses production `@rightkit/git: workspace:^` at `packages/update/package.json:39`. This is a standalone-source candidacy, not an automatic app-invariant violation; pack-rewrite proof remains pending. Confirm with an isolated packed install before changing it.
- `@rightkit/media-ui` marks `fabric` & `react-image-crop` optional peers at `packages/media-ui/package.json:32-43`; root entry is deliberately fabric-free at `packages/media-ui/src/index.ts:36-37`, while image entry statically imports fabric/crop at `packages/media-ui/src/image/FabricTextOverlay.tsx:2` & `packages/media-ui/src/image/ImageCropTool.tsx:2-3`. Subpath contract looks intentional; packed image-entry smoke remains needed.

## Per-manifest edge inventory & verdicts

| Manifest & anchors | Direct declarations: `scope:name@spec [use]` | Verdict |
|---|---|---|
| `package.json:1-42` | none | Workspace orchestrator; no direct npm edge. |
| `desktop-app-template/package.json:42-63` | `dep:@rightkit/app-shell@0.4.1 [runtime]`; `dep:@rightkit/logs@0.1.4 [runtime]`; `dep:@rightkit/platform-ui@0.2.0 [runtime]`; `dep:@rightkit/tauri@0.2.0 [runtime]`; `dep:@rightkit/updates@0.2.5 [runtime]`; `dep:@tauri-apps/api@^2.11.1 [runtime]`; `dep:@tauri-apps/plugin-os@2.4.0 [runtime]`; `dep:@tauri-apps/plugin-process@2.3.1 [runtime/native config]`; `dep:@tauri-apps/plugin-updater@2.10.1 [runtime]`; `dep:react@^19.2.8 [runtime]`; `dep:react-dom@^19.2.8 [runtime]`; `dev:@rightkit/release@0.2.151 [build]`; `dev:@tailwindcss/vite@^4.3.3 [build]`; `dev:@tauri-apps/cli@^2.11.4 [build]`; `dev:@types/react@^19.2.18 [type]`; `dev:@types/react-dom@^19.2.5 [type]`; `dev:@vitejs/plugin-react@^6.1.0 [build]`; `dev:tailwindcss@^4.3.3 [build]`; `dev:typescript@^6.0.3 [type/build]`; `dev:vite@^8.2.2 [build]` | Locked registry pins resolve in template lockfile; app-shell `0.4.1` peer floor is `platform-ui >=0.1.1` at `desktop-app-template/pnpm-lock.yaml:94-99`, so template’s `0.2.0` is valid. |
| `packages/agent-room-mcp/package.json:1-11` | none | Private Node package; built-ins only. |
| `packages/app-shell/package.json:39-54` | `peer:@rightkit/platform-ui@>=0.2.2 <1 [runtime+type]`; `peer:@tauri-apps/plugin-os@>=2 <3 [optional runtime]`; `peer:react@>=18 <21 [runtime+type]`; `peer:react-dom@>=18 <21 [runtime]`; `dev:@rightkit/platform-ui@workspace:* [runtime+test]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vite@^6.0.0 [build]`; `dev:vitest@^2.1.8 [test]` | Valid declarations; current lifecycle mismatch is finding 2. Optional plugin duplicate is candidate above. |
| `packages/ax/package.json:1-45` | none | Standalone Node CLI; built-ins only. |
| `packages/build-control-policy/package.json:1-7` | none | Private Node policy package; built-ins only. |
| `packages/doc-render/package.json:27-50` | `peer:react@>=18 <21 [runtime+type]`; `dep:@types/mdast@^4.0.4 [type]`; `dep:dompurify@^3.4.16 [runtime]`; `dep:github-slugger@^2.0.0 [runtime]`; `dep:remark-frontmatter@^5.0.0 [runtime]`; `dep:remark-gfm@^4.0.1 [runtime]`; `dep:remark-math@^6.0.0 [runtime]`; `dep:remark-parse@^11.0.0 [runtime]`; `dep:remark-smartypants@^3.0.3 [runtime]`; `dep:shiki@^4.4.3 [runtime]`; `dep:unified@^11.0.5 [runtime]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:esbuild@^0.28.1 [test/build]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid. `@types/mdast` stays production dependency: emitted declarations import `mdast` at `packages/doc-render/dist/parseMarkdown.d.ts:1`. |
| `packages/git/package.json:1-31` | none | Standalone Node CLI; built-ins only. |
| `packages/hooks/package.json:1-33` | none | Standalone Node runtime/CLI; built-ins only. |
| `packages/i18n/package.json:12-21` | `peer:react@>=18 <21 [runtime+type]`; `dev:@testing-library/jest-dom@^6.6.3 [test]`; `dev:@testing-library/react@^16.1.0 [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:jsdom@^25.0.1 [test]`; `dev:react@^19.0.0 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid React peer + local test toolchain. |
| `packages/legal-ui/package.json:27-42` | `peer:react@>=18 <21 [runtime+type]`; `dev:@testing-library/jest-dom@^6.6.3 [test]`; `dev:@testing-library/react@^16.1.0 [test]`; `dev:@testing-library/user-event@^14.5.2 [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:axe-core@4.12.1 [preview/test]`; `dev:esbuild@^0.28.1 [preview build]`; `dev:jsdom@^25.0.1 [test]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid. |
| `packages/legal/package.json:20-24` | `dev:@types/node@^22.10.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid Node typings/toolchain. |
| `packages/license/package.json:20-30` | `peer:@tauri-apps/plugin-http@^2 [optional runtime]`; `dev:@tauri-apps/plugin-http@^2 [runtime+test]`; `dev:@types/node@^22.10.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid lazy optional peer: dynamic import & fallback at `packages/license/src/index.ts:155-183`. Source `0.2.0` exceeds staged/registry `0.1.8` (`rightkit-versions.json:24,425`); lifecycle tracking needed before publish. |
| `packages/logs/package.json:23-25` | `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid zero-runtime-dependency package. |
| `packages/media-ui/package.json:31-58` | `peer:fabric@^7.4.0 [optional image runtime]`; `peer:react@>=18 <21 [runtime+type]`; `peer:react-dom@>=18 <21 [runtime]`; `peer:react-image-crop@^11.1.2 [optional image runtime]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@testing-library/dom@^10.4.0 [test]`; `dev:@testing-library/react@^16.1.0 [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:esbuild@^0.28.1 [test/build]`; `dev:fabric@^7.4.0 [runtime+test]`; `dev:jsdom@^25.0.1 [test]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [runtime+test]`; `dev:react-image-crop@^11.1.2 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Intentional optional image subpath model; packed subpath smoke candidate. |
| `packages/onboarding/package.json:26-45` | `peer:@rightkit/app-shell@>=0.2.0 <1 [runtime]`; `peer:@rightkit/legal-ui@>=0.1.2 <1 [runtime+type]`; `peer:react@>=18 <21 [runtime+type]`; `dev:@rightkit/app-shell@workspace:* [runtime+test]`; `dev:@rightkit/legal-ui@workspace:* [runtime+type]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@testing-library/jest-dom@^6.6.3 [test]`; `dev:@testing-library/react@^16.1.0 [test]`; `dev:@testing-library/user-event@^14.5.2 [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:jsdom@^25.0.1 [test]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vite@^6.0.0 [e2e build]`; `dev:vitest@^2.1.8 [test]` | Valid peer/development pairs; runtime imports at `packages/onboarding/src/Onboarding.tsx:1-3`. |
| `packages/pdf-ui/package.json:27-41` | `peer:react@>=18 <21 [runtime+type]`; `dep:pdfjs-dist@^6.3.289 [runtime]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:esbuild@^0.28.1 [test/build]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid; worker import depends on package at `packages/pdf-ui/src/pdfJs.ts:4-27`. |
| `packages/platform-ui/package.json:31-40` | `peer:@tauri-apps/plugin-os@>=2 <3 [optional runtime]`; `peer:react@>=18 <21 [runtime+type]`; `dev:@tauri-apps/plugin-os@^2.3.2 [runtime+test]`; `dev:@types/react@^19.0.0 [type]`; `dev:react@^19.0.0 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid optional lazy import. Source `0.2.2` has no staged/registry equivalent; see finding 2. |
| `packages/qa-chrome/package.json:28-31` | `dev:@types/node@^24.0.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid zero-third-party-runtime QA library. |
| `packages/qa/package.json:33-39` | `dep:@rightkit/qa-chrome@0.1.3 [runtime]`; `dev:@types/node@^24.0.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid re-export at `packages/qa/src/chrome.ts:1`. Exact published pin supports standalone install. |
| `packages/release/package.json:38-40` | `dep:smol-toml@1.9.0 [runtime]` | Valid parser is used by release command paths, e.g. `packages/release/ownership-lockfiles.mjs:2`. Source/staged `0.2.153`, registry record `0.2.151` (`rightkit-versions.json:32,433`): expected pre-publication state, verify registry after publication. |
| `packages/settings-ui/package.json:31-53` | `peer:@rightkit/app-shell@>=0.2.0 <1 [runtime+type]`; `peer:@rightkit/shell@>=0.1.0 <1 [optional, unused]`; `peer:@rightkit/license@>=0.1.0 [runtime+type]`; `peer:@rightkit/logs@>=0.1.0 [type]`; `peer:@rightkit/theme@>=0.2.0 <1 [runtime]`; `peer:@rightkit/updates@>=0.2.0 [type]`; `peer:react@>=18 <21 [runtime+type]`; `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@rightkit/shell@workspace:* [unused]`; `dev:@testing-library/jest-dom@^6.6.3 [test]`; `dev:@testing-library/react@^16.1.0 [test]`; `dev:@testing-library/user-event@^14.5.2 [test]`; `dev:@types/react@^19.0.0 [type]`; `dev:@types/react-dom@^19.0.0 [type]`; `dev:jsdom@^25.0.1 [test]`; `dev:react@^19.0.0 [runtime+test]`; `dev:react-dom@^19.0.0 [runtime+test]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vite@^6.0.0 [e2e build]`; `dev:vitest@^2.1.8 [test]` | Remove shell peer/dev edge after focused validation; remaining peer edges match source & emitted declarations at `packages/settings-ui/src/sections.tsx:1-6` & `packages/settings-ui/dist/sections.d.ts:2-5`. |
| `packages/shell/package.json:22-24` | `dev:typescript@^5.7.0 [type/build]` | Valid zero-runtime-dependency bridge. |
| `packages/tauri/package.json:18-32` | `dep:@rightkit/logs@^0.1.4 [runtime]`; `peer:@rightkit/updates@^0.2.5 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid: logs emits runtime import & updates emits declaration import at `packages/tauri/dist/index.js:1` & `packages/tauri/dist/index.d.ts:1`. |
| `packages/theme/package.json:31-35` | `dev:@rightkit/qa-chrome@workspace:* [test]`; `dev:@types/node@^24.0.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid Node/QA development toolchain. |
| `packages/update/package.json:38-45` | `dep:@rightkit/git@workspace:^ [runtime]`; `dep:smol-toml@1.9.0 [runtime]`; `dev:@types/node@^24.0.0 [type]`; `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Runtime imports at `packages/update/src/check.ts:5-9`; standalone-source workspace protocol is candidate above. |
| `packages/updates/package.json:23-25` | `dev:typescript@^5.7.0 [type/build]`; `dev:vitest@^2.1.8 [test]` | Valid zero-runtime-dependency policy helper. |

## Export, peer, & portability checks

- Public TypeScript packages define `exports`, `types`, `files`, & `prepack` where build output is needed: examples app-shell `packages/app-shell/package.json:7-35,56-63`; doc-render `packages/doc-render/package.json:7-25,52-57`; media-ui `packages/media-ui/package.json:7-30,60-65`; QA `packages/qa/package.json:8-31`.
- `workspace:` appears on internal development edges in app-shell, doc-render, media-ui, onboarding, pdf-ui, theme, & settings-ui; it also appears on public-unpublished update's production git edge at `packages/update/package.json:39`. No consumer template dependency uses `file:`, `link:`, Git, or workspace protocol; template uses registry pins at `desktop-app-template/package.json:42-55` & sealed resolutions at `desktop-app-template/pnpm-lock.yaml:11-25,94-120`.
- Required type-only dependencies remain declared where emitted `.d.ts` imports them: doc-render → `mdast`; tauri → `@rightkit/updates`; settings-ui → logs/updates. They are not removal candidates.
- No undeclared third-party module import was found after excluding Node built-ins, generated `dist`, snapshots, & test output. Apparent `mdast` module imports resolve through declared `@types/mdast` and are type-only.

## Lifecycle snapshot

`packages/release/rightkit-versions.json` is record, not a registry query. Current source differs from records for app-shell (`0.6.1` vs staged/registry `0.6.0`), license (`0.2.0` vs `0.1.8`), platform-ui (`0.2.2` vs `0.2.1`), & release (source/staged `0.2.153` vs registry `0.2.151`); anchors `packages/release/rightkit-versions.json:16,24,29,32,417,425,430,433`. This is an observed captured-version snapshot. All other public package source versions match staged & recorded registry values. Private/unpublished agent-room-mcp & build-control-policy, plus public-unpublished update, lack staged/registry entries.

## Rust dependency review — 2026-10-11

Scope: 57 workspace crates + 10 secondary Cargo packages; 445 declared dependency edges. No Cargo, rustc, builds, tests, network, or installs ran.

## Findings

One direct-edge candidate met manifest + Rust/build evidence. Full inventory includes targets, dev/build roles, optionality, renames, features, workspace inheritance, intent & first source evidence in `rust-dependency-edges.json`. Token evidence needs target-aware compilation before deletion.

### RK-RUST-KEEP-001: windows-core

- Manifest: `crates/rightkit-audio/Cargo.toml:44`; scope: target cfg(windows), optional capture feature; status: keep.
- Proof: `capture` enables it at `crates/rightkit-audio/Cargo.toml:18`. `#[implement(IMMNotificationClient)]` at `crates/rightkit-audio/src/lifecycle.rs:198` expands through locked `windows-implement 0.60.2`; current generator emits `::windows_core::IUnknownImpl` at `/Users/adrdsouza/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/windows-implement-0.60.2/src/gen.rs:182-184`. Lock anchors: `Cargo.lock:8338-8347`, `Cargo.lock:8373-8381`.
- Decision: retain direct edge; source-token absence does not cover proc-macro output.

### RK-RUST-001: rightkit-service

- Manifest: `desktop-app-template/src-tauri/Cargo.toml:41`; scope: template production dependency; confidence: high.
- Proof: Shell imports `rightapp_protocol::app_data_root` at `desktop-app-template/src-tauri/src/lib.rs:12`; diagnostics does same at `desktop-app-template/src-tauri/src/diagnostics.rs:8`. Protocol owns service at `desktop-app-template/crates/protocol/Cargo.toml:11` & resolves `Suite` at `desktop-app-template/crates/protocol/src/lib.rs:7,27-32`.
- Validation: Delete direct template edge only if protocol remains API boundary; compile template on supported targets before accepting.

## Package coverage

| Package | Manifest | scope | intent | edges | evidence |
|---|---|---:|---|---:|---|
| agent-room-core | `crates/agent-room-core/Cargo.toml` | workspace | private | 10 | source, test |
| rightkit-agent-tools | `crates/rightkit-agent-tools/Cargo.toml` | workspace | private | 7 | source, test |
| rightkit-align | `crates/rightkit-align/Cargo.toml` | workspace | private | 3 | source, test |
| rightkit-asr | `crates/rightkit-asr/Cargo.toml` | workspace | public-explicit | 17 | source, test |
| rightkit-audio | `crates/rightkit-audio/Cargo.toml` | workspace | public-implicit | 14 | source, test |
| rightkit-automation | `crates/rightkit-automation/Cargo.toml` | workspace | private | 4 | source, test, example |
| rightkit-browser | `crates/rightkit-browser/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-cache | `crates/rightkit-cache/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-channel-memory | `crates/rightkit-channel-memory/Cargo.toml` | workspace | private | 3 | source |
| rightkit-compose | `crates/rightkit-compose/Cargo.toml` | workspace | private | 3 | source |
| rightkit-contracts | `crates/rightkit-contracts/Cargo.toml` | workspace | private | 3 | source |
| rightkit-control | `crates/rightkit-control/Cargo.toml` | workspace | public-implicit | 7 | example, source |
| control-test-app | `crates/rightkit-control/examples/control-test-app/Cargo.toml` | secondary | private | 3 | build, example |
| rightkit-crash | `crates/rightkit-crash/Cargo.toml` | workspace | public-implicit | 6 | source |
| rightkit-daemon | `crates/rightkit-daemon/Cargo.toml` | workspace | private | 21 | source, bench, test |
| rightkit-db | `crates/rightkit-db/Cargo.toml` | workspace | public-implicit | 6 | source, test |
| rightkit-embed | `crates/rightkit-embed/Cargo.toml` | workspace | public-implicit | 9 | source |
| rightkit-framed-sidecar | `crates/rightkit-framed-sidecar/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-fs | `crates/rightkit-fs/Cargo.toml` | workspace | public-implicit | 6 | source |
| rightkit-fsindex | `crates/rightkit-fsindex/Cargo.toml` | workspace | public-explicit | 4 | source, bench |
| rightkit-hotkeys | `crates/rightkit-hotkeys/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-http | `crates/rightkit-http/Cargo.toml` | workspace | public-implicit | 13 | source |
| rightkit-jobs | `crates/rightkit-jobs/Cargo.toml` | workspace | public-implicit | 11 | source, example, test |
| rightkit-license | `crates/rightkit-license/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-llm | `crates/rightkit-llm/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-logs | `crates/rightkit-logs/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-mcp | `crates/rightkit-mcp/Cargo.toml` | workspace | public-implicit | 12 | source, example, test |
| rightkit-media | `crates/rightkit-media/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-models | `crates/rightkit-models/Cargo.toml` | workspace | public-implicit | 17 | source, test |
| rightkit-oauth | `crates/rightkit-oauth/Cargo.toml` | workspace | public-implicit | 16 | example, source, test |
| rightkit-ocr | `crates/rightkit-ocr/Cargo.toml` | workspace | public-explicit | 12 | source, test |
| rightkit-ort | `crates/rightkit-ort/Cargo.toml` | workspace | public-implicit | 8 | source, test |
| rightkit-overlay | `crates/rightkit-overlay/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-ownership | `crates/rightkit-ownership/Cargo.toml` | workspace | public-explicit | 12 | source |
| rightkit-pdf | `crates/rightkit-pdf/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-playback | `crates/rightkit-playback/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-process | `crates/rightkit-process/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-qa | `crates/rightkit-qa/Cargo.toml` | workspace | public-implicit | 13 | source |
| rightkit-qts-ggml-sys | `crates/rightkit-qts-ggml-sys/Cargo.toml` | workspace | public-implicit | 2 | generated, source |
| rightkit-qts-ggml | `crates/rightkit-qts-ggml/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-qts | `crates/rightkit-qts/Cargo.toml` | workspace | public-implicit | 7 | source |
| rightkit-record | `crates/rightkit-record/Cargo.toml` | workspace | private | 9 | source |
| rightkit-review | `crates/rightkit-review/Cargo.toml` | workspace | public-implicit | 2 | source |
| rightkit-sandbox | `crates/rightkit-sandbox/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-scenario | `crates/rightkit-scenario/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-search | `crates/rightkit-search/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-secrets | `crates/rightkit-secrets/Cargo.toml` | workspace | public-implicit | 10 | source, test |
| rightkit-send | `crates/rightkit-send/Cargo.toml` | workspace | private | 7 | source |
| rightkit-service | `crates/rightkit-service/Cargo.toml` | workspace | public-implicit | 6 | source, example |
| rightkit-settings | `crates/rightkit-settings/Cargo.toml` | workspace | public-implicit | 5 | source, test |
| rightkit-shell | `crates/rightkit-shell/Cargo.toml` | workspace | public-implicit | 17 | source, test |
| shell-example-app | `crates/rightkit-shell/example-app/Cargo.toml` | secondary | private | 9 | build, source, test |
| rightkit-speech-client | `crates/rightkit-speech-client/Cargo.toml` | workspace | public-implicit | 7 | source, example, test |
| rightkit-sync | `crates/rightkit-sync/Cargo.toml` | workspace | public-implicit | 3 | source, bench |
| rightkit-tauri | `crates/rightkit-tauri/Cargo.toml` | workspace | public-implicit | 2 | source |
| rightkit-transcript | `crates/rightkit-transcript/Cargo.toml` | workspace | public-explicit | 3 | source |
| rightkit-tts | `crates/rightkit-tts/Cargo.toml` | workspace | public-explicit | 9 | source |
| rightkit-vad | `crates/rightkit-vad/Cargo.toml` | workspace | public-explicit | 3 | source, test |
| rightkit-watch | `crates/rightkit-watch/Cargo.toml` | workspace | public-implicit | 5 | source, bench |
| rightapp-engine | `desktop-app-template/crates/engine/Cargo.toml` | secondary | private | 3 | source |
| rightapp-protocol | `desktop-app-template/crates/protocol/Cargo.toml` | secondary | private | 2 | source |
| rightapp-tools | `desktop-app-template/crates/tools/Cargo.toml` | secondary | private | 1 | source |
| rightapp | `desktop-app-template/src-tauri/Cargo.toml` | secondary | private | 14 | build, source |
| rightkit-managed-agent-probe | `docs/plans/evidence/rightkit-build-control-plane-v5/windows/managed-agent-fixture/Cargo.toml` | secondary | public-implicit | 0 | none |
| rk2-fixture | `spikes/rk2-macos/fixture-app/Cargo.toml` | secondary | private | 3 | build, source |
| rk2-svc-call | `spikes/rk2-macos/svc-call/Cargo.toml` | secondary | private | 2 | example |
| rk2-rs-probe | `spikes/rk2-windows/rs-probe/Cargo.toml` | secondary | private | 1 | source |

## Feature review

Feature-heavy resolver/consumer review candidates: `rightkit-agent-tools` (5; crates/rightkit-agent-tools/Cargo.toml), `rightkit-asr` (18; crates/rightkit-asr/Cargo.toml), `rightkit-control` (5; crates/rightkit-control/Cargo.toml), `rightkit-db` (7; crates/rightkit-db/Cargo.toml), `rightkit-fs` (5; crates/rightkit-fs/Cargo.toml), `rightkit-http` (5; crates/rightkit-http/Cargo.toml), `rightkit-oauth` (5; crates/rightkit-oauth/Cargo.toml), `rightkit-ort` (28; crates/rightkit-ort/Cargo.toml), `rightkit-pdf` (5; crates/rightkit-pdf/Cargo.toml), `rightkit-qts-ggml-sys` (6; crates/rightkit-qts-ggml-sys/Cargo.toml), `rightkit-qts-ggml` (5; crates/rightkit-qts-ggml/Cargo.toml), `rightkit-qts` (24; crates/rightkit-qts/Cargo.toml), `rightkit-shell` (5; crates/rightkit-shell/Cargo.toml), `rightkit-tts` (6; crates/rightkit-tts/Cargo.toml), `rightkit-vad` (5; crates/rightkit-vad/Cargo.toml). No binary-size claim follows from feature count.

## Method

- Parsed every current Cargo manifest, including target, dev, build, optional, renamed, path, workspace-inherited & feature declarations.
- Scanned Rust source, macro tokens, checked-in generated Rust, tests, examples, benches & build.rs; JSON retains first evidence location per edge.
- `publish = false` is private; `publish = true` is explicit public; omission is recorded as implicit public intent.

## Dependency removal skeptic — 2026-10-11

Fresh source-only review of removal candidates from `evidence/npm-review.md` & `rust-review.md`. No prior audit used; no build, test, install, network, ingest, or product edit ran.

## Accepted candidates

| Candidate | Current evidence | Decision | Required validation |
|---|---|---|---|
| `@rightkit/settings-ui` → `@rightkit/shell` peer + dev | Manifest declares peer/dev at `packages/settings-ui/package.json:33,42`; source says structural adapter needs no runtime or type import at `packages/settings-ui/src/shell-adapter.ts:3-5`; checked-in declaration also has no module specifier at `packages/settings-ui/dist/shell-adapter.d.ts:3-21`. | Remove peer, optional-peer metadata, & dev edge. | Focused typecheck/test & packed-consumer check. |
| `desktop-app-template/src-tauri` → `rightkit-service` | Shell imports `rightapp_protocol::app_data_root` at `desktop-app-template/src-tauri/src/lib.rs:12`; diagnostics does same at `src/diagnostics.rs:8`; protocol owns service edge at `crates/protocol/Cargo.toml:11` & resolves `Suite` at `crates/protocol/src/lib.rs:7,27-32`. | Remove direct template edge at `src-tauri/Cargo.toml:41`; retain protocol edge. | Compile template on supported targets. |

## Rejected or deferred candidates

| Candidate | Skeptic evidence | Decision |
|---|---|---|
| `rightkit-audio` → `windows-core` | Source invokes `#[implement(IMMNotificationClient)]` at `crates/rightkit-audio/src/lifecycle.rs:198`. Locked `windows-core 0.62.2` depends on `windows-implement 0.60.2` (`Cargo.lock:8338-8347`, `:8373-8381`); current cached macro generator emits `::windows_core::IUnknownImpl` at `/Users/adrdsouza/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/windows-implement-0.60.2/src/gen.rs:182-184`. Direct `windows-core` supplies macro-expanded absolute crate path despite source importing `windows::core`. | Reject removal; token absence is insufficient for proc-macro output. |
| `@rightkit/app-shell` → `@tauri-apps/plugin-os` peer | App-shell peer is optional at `packages/app-shell/package.json:41,75-78`; platform-ui owns dynamic loader plus optional peer at `packages/platform-ui/package.json:31-36,58-61` & `src/index.ts:47-82`. Npm peer resolution across a consumer's direct/peer graph needs packed-consumer proof. | Defer; no manifest edit from source-only evidence. |
| `@rightkit/update` → `@rightkit/git` production edge | Runtime source imports `@rightkit/git/drift.mjs` at `packages/update/src/check.ts:9`. `workspace:^` concerns publish rewrite or standalone packing, not dependency necessity. | Retain; validate pack rewrite separately. |
| `@rightkit/media-ui` image peers | Image entry statically imports optional image packages, while root remains fabric-free, per `evidence/npm-review.md:18`. | Retain; subpath contract needs packed-image smoke. |
| Type/build/test edges called out by review | Emitted declarations retain `mdast` in doc-render, `@rightkit/updates` in tauri, plus logs/updates in settings-ui (`evidence/npm-review.md:57`). Compiler, test, & build dependencies were intentionally classified in review scope. | No deletion claim. |

## Lens result

`evidence/lenses/reasoning.minimize.json` records two accepted low-severity findings with `verifyStatus: unverified`. Packet excerpt cache examined 29 of 1,996 denominator paths & omitted 1,967, so result declares `complete: false`.

## Publication review — 2026-10-11

Snapshot: `f60f116514467e83952f5e5c12a71b34e3341389`; working tree had 526 changed/untracked entries. No network, build, test, pack, install, publication, credential read, or product change ran. One pure read-only `runtimeBudgetCheck` invocation read manifests & READMEs; it did not build, test, qualify, or mutate state. `registry_recorded` means local ledger only; remote availability remains unverified.

## Lifecycle inventory

Implemented means source manifest/VERSION exists. Staged, registry-recorded, & adopted come only from `packages/release/rightkit-versions.json`. Swift `swift.rightkit-swift` URL/version is a local registry-pinned tag record, not an adoption record. `—` means no local entry. Receipt status evaluates each public unit’s expected gate file against current tree identity; stale means former evidence fails current exact-commit/version/path-clean gate, not that former publication was false.

| Ecosystem | Unit | Implemented | Staged | Registry-recorded | Adopted | Receipt | Declared closure |
| --- | --- | --- | --- | --- | --- | --- | --- |
| npm | @rightkit/agent-room-mcp | 0.1.0 | — | — | — | not-required-private | files:0; exports:no; bin:no |
| npm | @rightkit/app-shell | 0.6.1 | 0.6.0 | 0.6.0 | — | stale | files:10; exports:yes; bin:no |
| npm | @rightkit/ax | 0.2.4 | 0.2.4 | 0.2.4 | — | missing | files:7; exports:yes; bin:yes |
| npm | @rightkit/build-control-policy | 0.1.0-phase1 | — | — | — | not-required-private | files:0; exports:no; bin:no |
| npm | @rightkit/doc-render | 0.1.6 | 0.1.6 | 0.1.6 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/git | 0.2.39 | 0.2.39 | 0.2.39 | 0.2.0 | stale | files:5; exports:no; bin:yes |
| npm | @rightkit/hooks | 0.1.2 | 0.1.2 | 0.1.2 | — | stale | files:3; exports:yes; bin:yes |
| npm | @rightkit/i18n | 0.1.0 | 0.1.0 | 0.1.0 | — | stale | files:4; exports:yes; bin:no |
| npm | @rightkit/legal | 0.3.1 | 0.3.1 | 0.3.1 | 0.2.0 | missing | files:2; exports:yes; bin:no |
| npm | @rightkit/legal-ui | 0.2.2 | 0.2.2 | 0.2.2 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/license | 0.2.0 | 0.1.8 | 0.1.8 | 0.1.5 | missing | files:1; exports:yes; bin:no |
| npm | @rightkit/logs | 0.1.4 | 0.1.4 | 0.1.4 | 0.1.3 | missing | files:2; exports:yes; bin:no |
| npm | @rightkit/media-ui | 0.3.2 | 0.3.2 | 0.3.2 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/onboarding | 0.3.2 | 0.3.2 | 0.3.2 | — | stale | files:5; exports:yes; bin:no |
| npm | @rightkit/pdf-ui | 0.1.5 | 0.1.5 | 0.1.5 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/platform-ui | 0.2.2 | 0.2.1 | 0.2.1 | 0.1.0 | stale | files:5; exports:yes; bin:no |
| npm | @rightkit/qa | 0.4.1 | 0.4.1 | 0.4.1 | 0.1.0 | stale | files:4; exports:yes; bin:no |
| npm | @rightkit/qa-chrome | 0.1.3 | 0.1.3 | 0.1.3 | — | stale | files:4; exports:yes; bin:yes |
| npm | @rightkit/release | 0.2.153 | 0.2.153 | 0.2.151 | 0.2.47 | stale | files:7; exports:no; bin:yes |
| npm | @rightkit/settings-ui | 0.3.4 | 0.3.4 | 0.3.4 | — | stale | files:6; exports:yes; bin:no |
| npm | @rightkit/shell | 0.1.0 | 0.1.0 | 0.1.0 | — | stale | files:2; exports:yes; bin:no |
| npm | @rightkit/tauri | 0.2.0 | 0.2.0 | 0.2.0 | 0.1.0 | stale | files:3; exports:yes; bin:no |
| npm | @rightkit/theme | 0.2.3 | 0.2.3 | 0.2.3 | — | stale | files:6; exports:yes; bin:yes |
| npm | @rightkit/update | 0.2.0 | — | — | — | missing | files:6; exports:yes; bin:yes |
| npm | @rightkit/updates | 0.2.5 | 0.2.5 | 0.2.5 | 0.2.3 | stale | files:2; exports:yes; bin:no |
| cargo | agent-room-core | 0.3.1 | 0.3.0 | — | — | not-required-private | include:no |
| cargo | rightkit-agent-tools | 0.1.2 | — | — | — | not-required-private | include:no |
| cargo | rightkit-align | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-asr | 0.2.0 | 0.1.0 | 0.0.2 | — | stale | include:yes |
| cargo | rightkit-audio | 0.2.1 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-automation | 0.3.0 | 0.1.0 | — | — | not-required-private | include:no |
| cargo | rightkit-browser | 0.2.4 | 0.2.4 | 0.2.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-cache | 0.2.0 | 0.1.0 | 0.1.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-channel-memory | 0.1.0 | — | — | — | not-required-private | include:no |
| cargo | rightkit-compose | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-contracts | 0.1.4 | 0.1.2 | — | — | not-required-private | include:no |
| cargo | rightkit-control | 0.2.0 | 0.2.0 | 0.1.11 | — | stale | include:yes |
| cargo | rightkit-crash | 0.2.1 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-daemon | 0.1.3 | — | — | — | not-required-private | include:no |
| cargo | rightkit-db | 0.4.0 | 0.3.2 | 0.3.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-embed | 0.3.2 | 0.3.2 | 0.3.1 | — | stale | include:yes |
| cargo | rightkit-framed-sidecar | 0.2.0 | 0.1.0 | 0.1.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-fs | 0.3.4 | 0.3.3 | 0.3.3 | 0.1.0 | stale | include:yes |
| cargo | rightkit-fsindex | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-hotkeys | 0.1.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-http | 0.3.0 | 0.2.5 | 0.2.4 | — | stale | include:yes |
| cargo | rightkit-jobs | 0.2.8 | 0.2.7 | 0.2.6 | — | stale | include:yes |
| cargo | rightkit-license | 0.1.3 | 0.1.3 | 0.1.3 | 0.1.2 | missing | include:yes |
| cargo | rightkit-llm | 0.2.0 | 0.2.0 | — | — | missing | include:yes |
| cargo | rightkit-logs | 0.2.2 | 0.2.1 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-mcp | 0.5.3 | 0.5.3 | 0.5.2 | — | stale | include:yes |
| cargo | rightkit-media | 0.2.6 | 0.2.5 | 0.2.4 | 0.1.0 | stale | include:yes |
| cargo | rightkit-models | 0.2.9 | 0.2.8 | 0.2.7 | — | stale | include:yes |
| cargo | rightkit-oauth | 0.3.1 | 0.3.1 | 0.3.0 | — | stale | include:yes |
| cargo | rightkit-ocr | 0.2.1 | 0.2.0 | 0.1.2 | — | stale | include:yes |
| cargo | rightkit-ort | 0.2.3 | 0.2.2 | 0.2.2 | — | stale | include:yes |
| cargo | rightkit-overlay | 0.1.1 | 0.1.1 | — | — | missing | include:yes |
| cargo | rightkit-ownership | 0.2.1 | 0.2.1 | 0.2.1 | — | stale | include:yes |
| cargo | rightkit-pdf | 0.2.1 | 0.2.0 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-playback | 0.2.0 | 0.1.2 | 0.1.1 | 0.1.0 | stale | include:yes |
| cargo | rightkit-process | 0.3.4 | 0.3.3 | 0.3.3 | 0.1.0 | stale | include:yes |
| cargo | rightkit-qa | 0.2.16 | 0.2.15 | 0.2.14 | — | stale | include:yes |
| cargo | rightkit-qts | 0.2.6 | 0.2.5 | 0.2.4 | — | stale | include:yes |
| cargo | rightkit-qts-ggml | 0.1.2 | 0.1.2 | 0.1.1 | — | stale | include:yes |
| cargo | rightkit-qts-ggml-sys | 0.1.2 | 0.1.2 | 0.1.1 | — | stale | include:yes |
| cargo | rightkit-record | 0.2.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-review | 0.2.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-sandbox | 0.3.4 | 0.3.4 | 0.3.3 | — | stale | include:yes |
| cargo | rightkit-scenario | 0.1.0 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-search | 0.2.0 | 0.2.0 | 0.2.0 | 0.1.0 | stale | include:yes |
| cargo | rightkit-secrets | 0.1.3 | 0.1.3 | 0.1.2 | — | stale | include:yes |
| cargo | rightkit-send | 0.1.0 | — | — | — | not-required-private | include:yes |
| cargo | rightkit-service | 0.1.5 | 0.1.4 | 0.1.3 | — | stale | include:yes |
| cargo | rightkit-settings | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-shell | 0.3.0 | 0.2.0 | 0.1.4 | — | stale | include:yes |
| cargo | rightkit-speech-client | 0.3.3 | 0.3.3 | 0.3.2 | — | stale | include:yes |
| cargo | rightkit-sync | 0.1.1 | 0.1.0 | — | — | missing | include:yes |
| cargo | rightkit-tauri | 0.1.3 | 0.1.2 | 0.1.2 | 0.1.0 | stale | include:yes |
| cargo | rightkit-transcript | 0.3.0 | 0.2.0 | 0.2.0 | — | stale | include:yes |
| cargo | rightkit-tts | 0.2.0 | 0.1.1 | 0.1.0 | — | stale | include:yes |
| cargo | rightkit-vad | 0.1.0 | 0.0.3 | 0.0.2 | — | stale | include:yes |
| cargo | rightkit-watch | 0.3.3 | 0.3.2 | 0.3.1 | — | stale | include:yes |
| swift | RightKitSwift | 0.6.0 | — | 0.4.0 | — | stale | libraries:15 |
| swift | RightKitCloudKitHelper | — | — | — | — | not-applicable | executables:1 |

Detailed machine-readable rows, receipt identity reasons, closure declarations, & source paths: [publication-inventory.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/publication-inventory.json>).

## Receipts, identity & portability

Gate schema is `1` in `packages/release/publication-gate.mjs`. A public npm/Cargo/Swift release needs evidence for exact current 40-hex `HEAD`, clean unit path, same name/version, successful host receipt(s), & a 64-hex artifact hash. Of 84 units, 71 are public release units, 12 private units, & one internal Swift helper. Current snapshot has **0 current public-unit gate receipts**: 59 stale (18 npm + 40 Cargo + Swift) & 12 missing (5 npm + 7 Cargo). Current path dirtiness independently blocks app-shell & license; exact-HEAD mismatch blocks each existing receipt.

Declared npm `files`/exports/bin & Cargo `include` describe intended archive closure, but no package archive or fresh install was made here, so standalone-install portability is **unverified for every release unit**. Internal `workspace:*` npm & Cargo `path` edges are source-package wiring; they are not app-consumer violations by themselves. RightKitCloudKitHelper’s `.package(path: "../RightKitSwift")` is an internal helper edge, recorded separately; it is neither a tagged release nor an app adoption. App violations require an app manifest with a local/file/link/path shared dependency; this review did not infer one from internal package wiring.

## Runtime-budget enforcement

`docs/RUNTIME-BUDGETS.md` requires declaration of resident memory ceiling, CPU, disk operations, measured run; repeatable bench/test; runtime exposure; publish gate; & scale-consumer record. `runtime-budgets.mjs` only inspects README text for exact heading plus resident/CPU figures & one measured line; it does not execute/verify bench, disk-op or ceiling figure, runtime `status()`, or installed scale evidence. `publish-cargo.mjs` invokes this static check before cargo publish, so README failure blocks real cargo publication.

Static invocation of `runtimeBudgetCheck` returned:

| Class crate | Result | Reason |
| --- | --- | --- |
| rightkit-fsindex | fail | no `## Runtime budget` section |
| rightkit-watch | fail | no `## Runtime budget` section |
| rightkit-jobs | fail | no `## Runtime budget` section |
| rightkit-service | fail | no `## Runtime budget` section |
| rightkit-daemon | fail | no `## Runtime budget` section |
| rightkit-cache | fail | no `## Runtime budget` section |
| rightkit-sync | fail | no `## Runtime budget` section |
| rightkit-logs | fail | no `## Runtime budget` section |
| rightkit-shell | fail | no `## Runtime budget` section |
| rightkit-qa | fail | no `## Runtime budget` section |
| rightkit-send | fail | no `## Runtime budget` section |

`rightkit-fsindex` has measured resident-memory prose & `status()` metrics, but no required heading; remaining ten lack a Runtime budget section. Thus docs’ broader rule is not enforced or demonstrated by current source; no claim of runtime compliance follows from README comments or bench references.

## Runtime-budget review — 2026-10-11

## Verdict

**No continuously-running crate has fresh, installed-app, scale-run proof.** All 11 rule-listed crates lack `## Runtime budget` in current README files. `rightkit-fsindex`, `rightkit-watch`, `rightkit-sync`, & `rightkit-daemon` have macOS-only source harnesses, but no fresh `RESULT` artifact exists. `rightkit-process` is omitted despite `StallWatchdog`; `rightkit-models` owns long-lived model-use leases & is omitted too.

This review used current source, checked-in non-audit evidence, & passive process samples only. No build, test, bench, network, installed-app action, or product change ran. Baseline commit: `f60f116514467e83952f5e5c12a71b34e3341389` ([initial-state.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/initial-state.json:2>)).

## Required proof

Rule requires resident-memory ceiling/per-unit, steady CPU, disk operations under change, named machine/data/command run, runtime `status()` equivalent, publish rejection, & first-consumer whole-disk/mailbox/repo installed-app run before adoption ([RUNTIME-BUDGETS.md](</Volumes/D/claude/rightkit/docs/RUNTIME-BUDGETS.md:5>)). Rule class names 11 crates ([RUNTIME-BUDGETS.md](</Volumes/D/claude/rightkit/docs/RUNTIME-BUDGETS.md:15>)).

`runtime-budgets.mjs` names same 11 only ([runtime-budgets.mjs](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:26>)); optional manifest opt-in could add owners ([runtime-budgets.mjs](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:44>)). No current selected manifest opts in; no selected README contains heading.

| Crate | Continuous owner / scale path | Declaration & fresh named run | Runtime exposure | Bound / fidelity finding | Current native command |
|---|---|---|---|---|---|
| `rightkit-fsindex` | Whole-disk crawl, FSEvents worker, relist loop. | No required heading. README carries older unqualified figures, but no machine, absolute ceiling, CPU, disk-I/O, or fresh artifact ([README.md](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/README.md:135>)). | `Index::status()` includes roots, entries, resume & counters ([index.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/src/index.rs:132>), [index.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/src/index.rs:647>)); not host RSS/CPU. | `budget` measures memory & real-temp-tree churn; `mem` is synthetic. Source comment says 400 changes/s, implementation executes 200 creates/s, then deletes ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/benches/budget.rs:4>), [budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/benches/budget.rs:252>)). FSEvents public stream remains unbounded ([README.md](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/README.md:60>)); opt-in content postings scale with indexed content ([content.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/src/content.rs:11>)). No whole-disk proof. | `rightkit cargo bench -p rightkit-fsindex --bench budget`; source also supports `-- mem 1000000` / `-- churn 50000` ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/benches/budget.rs:12>)). |
| `rightkit-watch` | Async, typed & directory watcher loops. | No heading; no fresh result. | `TypedWatcher::metrics()` exposes queue depth, overflow & errors, not host RSS/CPU/disk ([typed.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/src/typed.rs:74>), [typed.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/src/typed.rs:151>)). | Current macOS harness uses 50,400 temp files, 400 rewrite ops/s, 60 s, drains every tick ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/benches/budget.rs:3>), [budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/benches/budget.rs:130>)). Useful synthetic OS-watch proof; not installed-app/repo scale. Typed queue & async raw Tokio channel are unbounded ([README.md](</Volumes/D/claude/rightkit/crates/rightkit-watch/README.md:93>), [subscribe.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/src/subscribe.rs:392>)). | `rightkit cargo bench -p rightkit-watch --bench budget` ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-watch/benches/budget.rs:15>)). |
| `rightkit-jobs` | Engine supervisor, health/restart, sidecar client threads. | No heading, bench, or fresh result. | `SupervisorStatus` exposes lifecycle, PID & restarts; no queue, RSS, CPU, disk ops, child RSS, or health latency ([supervisor.rs](</Volumes/D/claude/rightkit/crates/rightkit-jobs/src/supervisor.rs:154>), [supervisor.rs](</Volumes/D/claude/rightkit/crates/rightkit-jobs/src/supervisor.rs:552>)). | Persistent jobs & recovery need long-duration restart/backlog proof. Event subscriber collection appends senders ([supervisor.rs](</Volumes/D/claude/rightkit/crates/rightkit-jobs/src/supervisor.rs:540>)); no declared bound or observable backlog. | None. Existing `rightkit cargo test -p rightkit-jobs -- --test-threads=1` is correctness-only, not runtime proof ([publish-cargo.mjs](</Volumes/D/claude/rightkit/packages/release/publish-cargo.mjs:49>)). |
| `rightkit-service` | Resident local server, takeover monitor, connection serving. | No heading, bench, or fresh result. | No public runtime status/metrics located. | Monitor polls every 25 ms & waits in-flight requests during handoff ([server.rs](</Volumes/D/claude/rightkit/crates/rightkit-service/src/server.rs:495>)). No scale proof for clients, frames, threads, IPC throughput, or idle CPU. | None; correctness command only: `rightkit cargo test -p rightkit-service -- --test-threads=1` ([publish-cargo.mjs](</Volumes/D/claude/rightkit/packages/release/publish-cargo.mjs:49>)). |
| `rightkit-daemon` | Tokio HTTP/TLS listener, durable state, lifecycle events. | No heading; no fresh result. | `DaemonMetrics` exposes requests, state write bytes & retained counts, not RSS/CPU/disk ops ([daemon.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/src/daemon.rs:102>), [daemon.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/src/daemon.rs:314>)). | `budget` drives in-process `Daemon::handle`, 10,000 signed writes; it intentionally bypasses transport/TLS ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/benches/budget.rs:1>), [budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/benches/budget.rs:97>)). State maps grow by device/job/idempotency/nonce/webhook entries ([state.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/src/state.rs:11>)); in-memory events cap at 10,000 but JSONL retains all ([events.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/src/events.rs:72>)). | `rightkit cargo bench -p rightkit-daemon --bench budget` ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-daemon/benches/budget.rs:15>)). |
| `rightkit-cache` | Persistent cache with eviction, likely resident across app life. | No heading, bench, or fresh result. | Stats exist in cache API but no continuous host status with RSS/CPU/disk rate was found. | Byte cap prevents stored-byte growth; in-memory `HashMap` grows per key & LRU eviction snapshots every entry then sorts ([disk.rs](</Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:115>), [disk.rs](</Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:755>)). No high-cardinality/cache-churn proof. | None. |
| `rightkit-sync` | App-driven recurring passes, offline ledger & retry. | No heading; no fresh result. | `SyncMetrics` exposes pass time, counters & ledger cardinalities, not RSS/CPU/disk ops ([engine.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/src/engine.rs:89>), [engine.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/src/engine.rs:324>)). | macOS harness seeds 100,000 memory-backend records, runs 100 bidirectional passes, prints memory/CPU/ledger size ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/benches/budget.rs:1>), [budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/benches/budget.rs:166>)). Good synthetic engine exercise; no remote transport, installed app, background cadence, or offline-replay scale. Source says tombstones persist forever ([engine.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/src/engine.rs:104>)). | `rightkit cargo bench -p rightkit-sync --bench budget` ([budget.rs](</Volumes/D/claude/rightkit/crates/rightkit-sync/benches/budget.rs:15>)). |
| `rightkit-logs` | Tracing writer worker & consented outbox/uploader. | No heading, bench, or fresh result. | `OutboxStatus` exposes total/error only ([outbox.rs](</Volumes/D/claude/rightkit/crates/rightkit-logs/src/outbox.rs:88>), [outbox.rs](</Volumes/D/claude/rightkit/crates/rightkit-logs/src/outbox.rs:221>)); tracing worker has pending/dropped internals but no host status ([tracing_file.rs](</Volumes/D/claude/rightkit/crates/rightkit-logs/src/tracing_file.rs:454>)). | Outbox event count is capped by configuration ([outbox.rs](</Volumes/D/claude/rightkit/crates/rightkit-logs/src/outbox.rs:158>)); no sustained log-rate, slow-disk, uploader outage, retention, CPU, or RSS proof. | None. |
| `rightkit-shell` | Watchdog, idle/presence pollers, desktop lifetime. | No heading, bench, or fresh result. | Autostart/idle/notify status APIs do not expose watchdog RSS/CPU/poll work. | Watchdog runs one sleep-loop per configured thread ([watchdog.rs](</Volumes/D/claude/rightkit/crates/rightkit-shell/src/watchdog.rs:105>)); no idle desktop, engine restart, or multi-window scale proof. | None. |
| `rightkit-qa` | Harness controls child app/processes while run exists. | No heading, bench, or fresh result. | `PaintMetrics` is renderer-specific; no harness resource status found. | Step retry loops follow supplied `until_timeout_s` / interval ([scenario.rs](</Volumes/D/claude/rightkit/crates/rightkit-qa/src/scenario.rs:381>)); no concurrent-scenario, orphan-sweep, evidence-volume, child-RSS, or 5+ minute proof. | None. |
| `rightkit-send` | Acceptor, discovery, announce, scan & liveness threads. | No heading, bench, or fresh result. | No public runtime status/metrics found. | Connection handlers cap at 64 ([lib.rs](</Volumes/D/claude/rightkit/crates/rightkit-send/src/lib.rs:945>)); discovery can spawn per announcement ([lib.rs](</Volumes/D/claude/rightkit/crates/rightkit-send/src/lib.rs:971>)) & liveness spawns per stale device ([lib.rs](</Volumes/D/claude/rightkit/crates/rightkit-send/src/lib.rs:1076>)). Network sweep pool caps at 48 ([discovery.rs](</Volumes/D/claude/rightkit/crates/rightkit-send/src/discovery.rs:431>)). No device-count or transfer-load ceiling/proof. | None. |
| `rightkit-process` **omitted owner** | `StallWatchdog` runs until stop/drop. | No heading, manifest opt-in, bench, or fresh result. | Emits stall events only; no resource/status counters. | Default wakes every 50 ms; runtime loop calls caller-supplied `ping` each poll ([stall.rs](</Volumes/D/claude/rightkit/crates/rightkit-process/src/stall.rs:47>), [stall.rs](</Volumes/D/claude/rightkit/crates/rightkit-process/src/stall.rs:124>)). This is continuous supervision, so omission from rule list defeats gate/adoption checks. | None. |
| `rightkit-models` **omitted model-supervision owner** | Model-use lease persists while model stays loaded; idle reader owns helper thread during download. | No heading, manifest opt-in, bench, or fresh result. | `ModelStatus` reports install state, not model-process RSS/CPU/accelerator use. | README-level source says one process may hold roughly 700 MB speech model & second process waits ([lease.rs](</Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/lease.rs:1>)); download reader uses bounded two-chunk channel, but abandoned helper can remain blocked ([http.rs](</Volumes/D/claude/rightkit/crates/rightkit-models/src/http.rs:195>)). Measure model-host process & accelerator at full installed-model load. | None. |

## Existing figures & passive samples

`fsindex` README claims 7.7M generated entries: 3.5 MB footprint / 22.6 MB resident after reopen; 185 MB resident after name search; 29 MB after trim ([README.md](</Volumes/D/claude/rightkit/crates/rightkit-fsindex/README.md:149>)). These are **declared historical figures**, not fresh named-run evidence: no machine, CPU, absolute ceiling, disk-op rate, raw `RESULT`, or installed-app scale proof.

Passive sample at 2026-10-11 01:31:44 UTC observed Pulse RSS 161,776 KiB / 5.7% CPU & `pulse-hub` RSS 32,480 KiB / 0.0% CPU; HeardRight app/engine rows also appear ([runtime-sample-1.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/runtime-sample-1.json:1>)). It cannot assign cost to any crate, establish sustained load, or satisfy a crate budget. Installed inventory only establishes candidate app versions, not adoption measurement ([installed-first-party-apps.json](</Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-11/evidence/installed-first-party-apps.json:1>)).

## Gate & adoption gaps

1. `right-release publish cargo` invokes runtime-budget check ([publish-cargo.mjs](</Volumes/D/claude/rightkit/packages/release/publish-cargo.mjs:49>)), but checker itself only searches README text for resident/CPU/`Measured` syntax ([runtime-budgets.mjs](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:88>)); it explicitly does **not** run bench ([runtime-budgets.mjs](</Volumes/D/claude/rightkit/packages/release/runtime-budgets.mjs:1>)). It omits required absolute ceiling, disk-op figure, exposed runtime values, artifact, scale proof, process/models membership, & benchmark fidelity.
2. Gate is cargo-publication scoped. `rightkit-daemon` & `rightkit-send` set `publish = false` ([daemon Cargo.toml](</Volumes/D/claude/rightkit/crates/rightkit-daemon/Cargo.toml:1>), [send Cargo.toml](</Volumes/D/claude/rightkit/crates/rightkit-send/Cargo.toml:1>)); they can reach app adoption without this cargo publish path.
3. Adoption registry records version states, while project lifecycle defines adoption as app pin ([README.md](</Volumes/D/claude/rightkit/README.md:41>)); neither records required installed-app figures. Current passive app samples therefore leave all 13 owners unqualified for scale adoption.

## Native measurement worklist

Run source-supplied harnesses on native macOS host, save raw `RESULT` output beside this review with machine model, OS, RAM, app version, commit, workload bytes/entry count, duration, peak & steady RSS, CPU, disk reads/writes or equivalent, status snapshot, & installed-app attribution:

```sh
rightkit cargo bench -p rightkit-fsindex --bench budget
rightkit cargo bench -p rightkit-watch --bench budget
rightkit cargo bench -p rightkit-sync --bench budget
rightkit cargo bench -p rightkit-daemon --bench budget
```

Before publication/adoption, add equivalent native, repeatable workload commands for `jobs`, `service`, `cache`, `logs`, `shell`, `qa`, `send`, `process`, & `models`; record one first-consumer installed-app run at whole disk, whole mailbox, or whole repository scale as applicable.

## Publication order & durable closure

1. Finish source/contract decisions & fix confirmed correctness defects in report 2.
2. Establish runtime budgets for all continuous owners, including omitted process watchdog/model leases; enforce structured evidence at both publication & adoption.
3. Validate each changed package on applicable target/feature matrix, then release contracts. Required repository commands include focused builds/tests, `pnpm --dir rightkit test:release`, `pnpm --dir rightkit doctor:release` & app parity contract tests from suite root.
4. Produce standalone package archives/install receipts. Publish foundational dependencies before consumers; platform-ui/app-shell requires explicit order. Cargo publication stays behind RightKit release command.
5. Record immutable release versions, archive hashes & verified availability. Each app then applies report 3's bounded handoff & acceptance checks.

Close each backlog ID with source revision, execution command, environment, artifact path/hash, result & consuming app version. Reopen only affected IDs when their inputs change.
