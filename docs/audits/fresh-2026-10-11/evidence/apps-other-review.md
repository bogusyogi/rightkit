# Apps other — current adoption review

Scope: current source under assigned paths only; no build, install, network, or product mutation. `INDEX.md` & `packages/release/rightkit-versions.json` supplied availability/pin comparison. `RUNTIME-BUDGETS.md` requires declared, measured, exposed installed-scale evidence for continuous crates; no such run was performed here.

## Result matrix

| product / checkout | state | declared RightKit surface | source integration | pin / boundary result |
| --- | --- | --- | --- | --- |
| Legion | active native CLI | hooks `0.1.2`; ax `0.2.4`; git `0.2.37`; release `0.2.151` | release bridge imports/calls `@rightkit/release`; native release code uses it for archive, SBOM, provenance | hooks & ax equal staged; git `0.2.37` & release `0.2.151` are legacy, behind staged `0.2.39` / `0.2.153`; no local RightKit spec |
| RustRight | active broker | ownership `=0.2.0`; fs `0.3.2`; process `0.3.1` | broker admission uses ownership; state/install use fs; process wrapper uses process | all real integrations; ownership behind staged `0.2.1`; fs/process are loose & behind staged `0.3.3`; no local RightKit dependency |
| SellRight | active commerce app | legal `0.3.1`; RightKitSwift `from: 0.1.0` | API validates LegalManifest; iOS target depends on RightKitDiagnostics | legal equals staged; Swift is range pin, URL lacks `.git`, & is behind required exact `https://github.com/bogusyogi/rightkit-swift.git@0.4.0`; release config omits required provider |
| RightSites | active SellRight downstream | legal `0.3.1`; same Swift declaration | copied API legal integration & iOS target | same Swift defect; downstream product tree duplicates SellRight identity; `r2-rightsites` differs from invariant provider `r2` |
| Xtool | active headless plugin | none | none found | no RightKit adoption or local RightKit link; local MCP/document/process implementation remains product-owned, so no Index capability migration claim follows from this scan |
| PostRight | template / integration fixture | app-shell `0.5.0`, logs `0.1.4`, platform-ui `0.2.1`, tauri `0.2.0`, updates `0.2.5`, release `0.2.151`; 12 Rust crates | imports app-shell/logs/platform-ui/updates; Tauri & engine use shell/jobs/process/logs/service/db/settings/secrets/http/crash/qa | broad real integration; no local RightKit spec; stale staged pins listed below; `r2-postrights` differs from invariant provider `r2`; release config & updater still contain product placeholders |
| Workflows | operational repository | none; KDP has no RightKit deps | none | not an app. `render/cad_engine` is historical prototype; nested child roots are independently classified below |
| LightCraft | active first-party desktop/photo app | none | none found | own Rust workspace; no RightKit declaration, import, local spec, or adoption claim |
| SEO | active first-party CLI/MCP operational tool | none | none found | own Python/plugin runtime; not desktop app; no RightKit declaration, import, local spec, or adoption claim |
| FreeCAD | upstream dependency fork | none | none found | `upstream` is `FreeCAD/FreeCAD`; `contrib/headless` is workflow prototype, not RightKit consumer |
| build123d | upstream dependency/research fork | none | none found | user-origin fork carries upstream build123d identity/maintainers; not first-party product |
| ScreenRight 2026-10-08 | archived product checkout | release `0.2.66` | release config invokes `right-release` | archive path; legacy release pin; no source import found; no current adoption claim |
| RightSuite / Orthic archives | archived suite shell / retired Hub shell | none | none found | neither is product source, package, add-on, or release lane |
| Membrane (three paths) | one product, three untracked snapshots | root hooks `0.1.0`, ax `0.1.1`, git `0.2.0`; Hub platform-ui `0.1.0`, release `0.2.67` | Blueprint/MCP import hooks; Hub copies installed platform-ui dist into its packaged vendor tree | all pins are legacy vs staged; Hub release config omits required provider; platform-ui integration is real but vendor output is build packaging, not local RightKit source |

## Source anchors & findings

### Legion

- Manifest declarations: `/Volumes/D/claude/legion/package.json:106-111`; permitted legacy set: `pnpm-workspace.yaml:2-5`.
- Runtime release bridge: `engine/xtask/src/rightkit_release_bridge.rs:1-190`; callers: `engine/xtask/src/finalize_macos_candidate.rs:366,539,551` & `prepare_unsigned_candidate.rs:460,549,561`.
- App release config declares `github-releases`: `right-release.config.mjs:141-162`.
- `rightkit-versions.json` staged targets: hooks `0.1.2`, ax `0.2.4`, git `0.2.39`, release `0.2.153`. Current package sources are hooks `0.1.2`, ax `0.2.4`, git `0.2.39`, release `0.2.153`; Index/version file therefore identifies two old consumer pins.

### RustRight

- Declared registry dependencies: `/Volumes/D/claude/rustright/Cargo.toml:22,37-38`.
- Production ownership gate: `crates/rustright-broker/src/job_service/assemble.rs:26-27` & `ownership.rs:10,48,125-129`.
- Shared fs/process calls: `crates/rustright-state/src/fsops.rs:35`; `crates/rustright-process/src/tree.rs:16-19`; `crates/rustright-service/src/cutover.rs:416`.
- `=0.2.0` ownership is exact but not current staged `0.2.1`; fs/process use compatible ranges, violating RightKit exact-pin policy & resolving below staged `0.3.3`.
- `/Volumes/D/claude/rightkit` appears only in parity discovery/default fixtures (`parity/apps.mac.json:16`, `crates/rustright-parity/src/broker.rs:622`), never as a Cargo source dependency.

### SellRight & RightSites

- Both API manifests declare legal `0.3.1`: `/Volumes/D/claude/sellright/packages/api/package.json:30`; `/Volumes/D/claude/rightsites/packages/api/package.json:31`. Both call `assertLegalManifest`: `sellright/packages/api/src/legal/manifests.ts:19`; `rightsites/packages/api/src/legal/manifests.ts:19`.
- Both iOS project files resolve `RightKitSwift` from `https://github.com/bogusyogi/rightkit-swift` with `from: 0.1.0`, then attach RightKitDiagnostics to target: `sellright/ios/project.yml:17-29`; `rightsites/ios/project.yml:17-29`. This is source integration, yet not required immutable URL + exact tag. Version catalog says `https://github.com/bogusyogi/rightkit-swift.git@0.4.0`.
- RightSites self-identifies as SellRight web-layer fork: `/Volumes/D/claude/rightsites/README.md:3-7`; its root package still names itself `sellright`: `package.json:2-5`; release config calls app `sellright` with `r2-rightsites`: `right-release.config.mjs:3-4`. Treat it as one downstream product branch, not independent SellRight adoption.
- SellRight config has no `distribution` field: `sellright/right-release.config.mjs:1-6`. RightKit `AGENTS.md` requires explicit provider `github-releases` for public products or `r2` for private products; RightSites `r2-rightsites` also differs from that locked provider value.
- Neither product references `RightKitCloudKitHelper`; no private Swift helper compatibility path exists in assigned current source.

### PostRight

- Template status & intended shared contract: `/Volumes/D/claude/postright/README.md:1-21`; runtime adoption table: `README.md:10-19`.
- Frontend imports: `src/App.tsx:2`, `src/lib/logs.ts:1`, `src/lib/platform.ts:2`, `src/lib/updates.ts:1`.
- Cargo pins: `src-tauri/Cargo.toml:28-40`; engine: `crates/engine/Cargo.toml:20-29`; QA: `crates/e2e/Cargo.toml:12-14`. Runtime calls include supervision `src-tauri/src/engine.rs:9-11`, shell host `src-tauri/src/lib.rs:11-13,85`, jobs/db/service `crates/engine/src/main.rs:18`, `channels.rs:9-10`, `runtime.rs:7`.
- Staged drift: app-shell `0.5.0 < 0.6.0`; release `0.2.151 < 0.2.153`; jobs `0.2.6 < 0.2.7`; secrets `0.1.2 < 0.1.3`; shell `0.1.4 < 0.2.0`; qa `0.2.14 < 0.2.15`. Other listed pins equal corresponding staged entries.
- Product completion remains blocked by source-marked placeholders: updater endpoint/key `src-tauri/tauri.conf.json:31-35`, platform scripts `right-release.config.mjs:73-74,104-105`. These are product configuration gaps, not RightKit local-copy findings.
- `right-release.config.mjs:21-22` uses `r2-postrights`, while RightKit's locked provider value for private products is `r2`.

### Xtool, workflows, ScreenRight

- Xtool identifies itself as headless MCP/laptop plugin: `/Volumes/D/claude/xtool/README.md:1-15`; its manifest has no `@rightkit/*`: `package.json:1-37`. No RightKit source import/declaration found.
- Workflows owns orchestration/operational data, while child tool repos retain separate histories: `/Volumes/D/claude/workflows/README.md:1-19`; `render/cad_engine` is explicitly historical: `README.md:46-47`. KDP manifest carries only external dependencies: `kdp/package.json:1-9`.
- Docs-only entries: `workflows/runbooks`, `workflows/brands`, & root operational guidance have no app manifest or executable product target. They are not adoption targets.
- Full workflow-root classification: `brands` docs/assets; `render` orchestration plus historical CAD prototype; `runbooks` docs; `scripts` bootstrap helpers; `seo-ops` historical operator notes; `kdp` private publishing helper; root `workflows` orchestration. None is an application adoption target. Evidence: `workflows/README.md:1-19`, `render/README.md:1-2`, `runbooks/README.md:1-15`, `seo-ops/README.md:1-10`, `kdp/README.md:1-38`.
- LightCraft is first-party application source (`Orthic-Labs/lightcraft` origin): `/Volumes/D/claude/workflows/lightcraft/README.md:10-18`; native app/CLI/web members appear in `Cargo.toml:1-34` & `apps/lightcraft*/Cargo.toml`. Search found no RightKit reference in current source/manifests.
- SEO is first-party independent skill/CLI/local-MCP tool, not desktop product: `/Volumes/D/claude/workflows/seo/README.md:1-20,64-96`; it states independent runtime & no host framework requirement. Search found no RightKit reference in current source/manifests.
- FreeCAD is a dependency/tool fork: origin `bogusyogi/FreeCAD`, upstream `FreeCAD/FreeCAD`; `/Volumes/D/claude/workflows/freecad/README.md:1-8` identifies FreeCAD, while `README.md:3-5` limits local interest to `contrib/headless` prototype. build123d is an upstream Python CAD framework: `/Volumes/D/claude/repos/build123d/README.md:23-35`, `pyproject.toml:10-14`; its user-origin fork does not turn it into first-party app source.
- ScreenRight is scoped only because path is `/Volumes/D/claude/archive/screenright-2026-10-08`; package release pin is `0.2.66`: `package.json:28-33`; config names archived `screenright`: `right-release.config.mjs:35-37`. No `@rightkit/release` import exists in product source; config invokes CLI at `right-release.config.mjs:96-97`.
- `/Volumes/D/claude/archive/retired-2026-10-09/rightsuite` is archived suite catalog shell: `README.md:1-34` links product sites but exposes no product manifest. `/Volumes/D/claude/archive/retired-2026-10-09/orthic` is explicitly retired historical provenance, never launchable product/package/add-on/release lane: `README.md:1-10`. Both have no RightKit manifest or integration.

### Membrane duplicate snapshots

- Paths: `/Volumes/D/worktrees/membrane-main-f602-baseline`, `membrane-blueprint-findings`, `membrane-live-diagnostics-phase1-repair`. Each lacks Git metadata (`git -C <path> rev-parse --is-inside-work-tree` fails), so none is an active checkout for release claims.
- All three have identical SHA-256 manifests: root `package.json` `96218bc8…`, root workspace `bd776b41…`, Hub `package.json` `126b6e1f…`, Hub workspace `1fbe1280…`. They are duplicate product snapshots with differing directory intent, not three consumers.
- Representative declarations: `membrane-main-f602-baseline/package.json:31-35`; Hub: `apps/membrane-hub/package.json:17,22`. Real hook use: `blueprint/src/lib/hooks/blueprint-graph-index.mjs:1`, `mcp/hooks/membrane-hook-runtime.mjs:1`; Hub integration/build copy: `apps/membrane-hub/src/popover.mjs:87`, `scripts/build-frontend.mjs:28-29`.
- `hooks 0.1.0`, ax `0.1.1`, git `0.2.0`, platform-ui `0.1.0`, release `0.2.67` trail staged `0.1.2`, `0.2.4`, `0.2.39`, `0.2.1`, `0.2.153`.
- Hub release config has no `distribution.provider`: `apps/membrane-hub/right-release.config.mjs:18-34`; this misses RightKit's explicit-provider invariant.

## Runtime-scale status

`docs/RUNTIME-BUDGETS.md:5-15` makes declaration, repeatable measurement, runtime exposure, & first-consumer scale record mandatory for continuous crates. Static source proves PostRight runs `rightkit-jobs` supervision, `rightkit-shell` host/watchdog, `rightkit-service`, & `rightkit-logs`; those four are listed continuous classes at `RUNTIME-BUDGETS.md:17-18`. This scope has no installed-app runtime status/measurement record for PostRight, so its adoption is integration-level only, never installed-scale qualified. RustRight & Membrane contain resident services, but their direct shared-crate adoption does not establish any RightKit continuous-crate installed-scale run. No other scoped product declares a shared continuous RightKit runtime.

## Exclusions & local-primitive outcome

- No `file:`, `link:`, `workspace:`, Cargo `path`, or Git dependency points at RightKit in consumer manifests/configs. Product-internal Cargo path dependencies remain internal primitives, not RightKit copies.
- No source-backed duplication maps to a distinct RightKit primitive beyond copied SellRight/RightSites & Membrane snapshot checkouts above. PostRight intentionally owns protocol schemas/tooling around shared components; that is source-local integration glue, not shared primitive replacement.
- Missing from scoped active app adoption: RightKitCloudKitHelper; every unmentioned Index package/crate. Absence means no declaration/source integration was found, never that consumer need exists.

## Census boundary

`machine-repositories.json` current census records every root below. This handoff classifies each so no first-party root is silently treated as third-party; detailed adoption evidence remains limited to assigned paths.

| census roots | classification | handling here |
| --- | --- | --- |
| `/Volumes/D/claude/cutright`, `genright`, `voiceright`, `scraperight`, `mailright`, `workright`, `viewright`, `coderight`, `heardright` | first-party product roots | outside assigned path; separate product-app review owns adoption evidence |
| `/Volumes/D/claude/pulse`, `swiftright` | first-party shared/service roots | outside assigned path; not consumer-app evidence here |
| `/Volumes/D/claude/rightkit` | shared-package source | audit subject, never consumer |
| `/Volumes/D/claude` | workspace shell | orchestration root, never product consumer |
| `repos/coderight/**` external clones; `repos/build123d`; `workflows/freecad` | dependency/research checkouts | excluded; build123d & FreeCAD classification anchored above |
| `archive/screenright-2026-10-08`, `archive/retired-2026-10-09/rightsuite`, `archive/retired-2026-10-09/orthic` | archived | classified above; never active adoption |
| `workflows/lightcraft`, `workflows/seo` | first-party roots | classified above |

## Final shallow machine-scope addendum

Shallow census added `/Volumes/D/{archive,sr0-build,rightkit-publish,tmp}` & `/Users/adrdsouza/{claude,Hyperframes,Sites,CodeBuddy,WorkBuddy AI,Desktop,Documents}`. No additional first-party application source root was found.

- `/Volumes/D/sr0-build/r3-final` (`4496474d…`) & `r3-final-011` (`c946d1d4…`) are SwiftRight staging clones from `bogusyogi/swiftright`; their README calls SwiftRight planned SDK, never released application. Primary source already exists at `/Volumes/D/claude/swiftright`.
- `/Volumes/D/rightkit-publish` holds RightKit publish staging, release artifacts, & duplicate `rightkit` clones (`claude/rightkit`, `w2-w4`, `v1-v4`, `npm`); it is source/release evidence, never consumer-app source. `/Volumes/D/archive/codex-archived-sessions-2026-10-09` is archived session data; `/Volumes/D/tmp` has no shallow candidate root.
- `/Users/adrdsouza/Hyperframes/studio-starter` is private Hyperframes composition project (`package.json:1-10`), not first-party app source. `/Users/adrdsouza/claude/{sellright,tools}` has no shallow Git/manifest identity. Sites, CodeBuddy, WorkBuddy AI, Desktop, & Documents were excluded as personal content, installed-app state, or test artifacts without deep inspection.

Structured scope record: `docs/audits/fresh-2026-10-11/evidence/machine-scope-addendum.json`.
