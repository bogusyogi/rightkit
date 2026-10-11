# Source scope

Fresh cutover inventory covers 97 current RightKit component rows: 57 Cargo crates, 25 npm packages & 15 separately importable RightKitSwift products. RightKitSwift umbrella is represented by its 15 products rather than a duplicate summary row. CloudKit & recording helper executables are implementation details of their parent owners. Private build-control infrastructure remains visible & is normally NA for consumer apps.

29 columns cover 20 app/tool checkouts, seven additional operational tool groups, plus two explicitly retained/archived app families. Each app has its own reviewer; source/status evidence is in `apps/*.json`. Every component is assessed for actual feature overlap & runtime compatibility, including app-owned alternatives without any RightKit import.

Review used Legion Foundation evidence rules, per-app delegation, & both iOS-development/macOS-development skill guidance for Apple targets. Previous audit conclusions were excluded as implementation evidence.

## App & tool roots

Primary apps: CodeRight, HeardRight, ViewRight, MailRight, ScrapeRight, CutRight, GenRight, VoiceRight, WorkRight, Pulse, SwiftRight, Ember, Legion, RustRight, SellRight, RightSites, Xtool & PostRight under `/Volumes/D/claude/<name>`. LightCraft has a distinct checkout at `/Volumes/D/claude/workflows/lightcraft`; it is not collapsed into Ember. SEO CLI/MCP source is `/Volumes/D/claude/workflows/seo`.

Additional tools: Rhook (`tools/rhook`), Groundwork (`mcps/groundwork`), KDP formatter (`workflows/kdp`), render/CAD tools (`workflows/render`), workspace utilities (`tools`, excluding separately reviewed Rhook), SEO operations (`workflows/seo-ops`) & waitlist worker (`bogusyogi/workers/waitlist`). Workspace utilities include source backends, pipelines, runners, hooks, shared libraries & Remotion compositions where operative. SEO operations is distinct from SEO CLI; `/Volumes/D/claude/SEO` resolves to SEO operations & is not double-counted.

Retained source: ScreenRight archive at `/Volumes/D/claude/archive/screenright-2026-10-08`; Membrane family under `/Volumes/D/worktrees/membrane-*`, with canonical audit root & snapshot details recorded in corpus/reviewer output. These columns describe retained source readiness, not active rollout obligations. App/source activity is a scope attribute, never hidden inside Y/N/M/NA.

## Discovery & exclusions

Discovery combined current workspace root directories, package/Cargo/Swift/Python/Go manifest searches, source entrypoints, shallow home project-directory census & desktop configured projects. `/Users/adrdsouza/claude` contained no additional package manifest; Hyperframes studio-starter is a composition project rather than shared desktop/service app. Third-party installed applications are outside user's owned app/tool migration scope.

Excluded as independent apps: third-party reference clones under `repos`, FreeCAD/build123d upstream forks, caches/dependencies, `target`, `.build`, `node_modules`, build/release staging clones, generated bundles, saved app data, content/websites without an app backend, one-off data reconciliation scripts, operational reports, empty retired Orthic/RightSuite shells, old HeardRight release checkout, `/Volumes/D/rightkit-publish`, `/Volumes/D/sr0-build` & publish/build artifacts. First-party wrapper code around these tools remains included where it belongs to an audited tool root. No private account data or credentials were used as evidence.

Additional Swift source-presence pass is recorded in `evidence/native-target-census.json`; reviewers resolved whether those sources belong to active targets, installers, fixtures or retained code. This surfaced nested SellRight/RightSites iOS targets & Legion/CutRight native helpers.

## Interpretation

Y requires live shared routing, with no competing local mechanism found inside matching owner scope. N requires active app-local implementation of applicable owner capability. M requires active shared plus overlapping local code. NA requires absent feature or incompatible runtime; no-import alone is insufficient. A Rust crate is not automatically a migration target for Node/Python/Swift code: an actual compatible host/bridge must exist. UI wrappers, schemas, policy & product algorithms remain local when shared owner does not supply those behaviors.

Versions, path overrides & publication readiness are separate delivery facts. Catalog source version is never an instruction to install an unverified release. Local availability ledger records are preserved in `evidence/availability.json`; choose a verified published version during implementation.

This is static source inspection, not execution qualification. Matrix does not assert installed builds use inspected source or claim successful builds/tests/publication. Source baseline hashes, reviewer HEADs & final citation excerpts identify inspected state despite other chats editing concurrently. Final validation lists cited files that changed during audit.
