# Design: all suite capabilities in RightKit packages

| Field | Value |
|---|---|
| Status | Draft v5. Revised after the EGM Goldfish review (v3), then by Adrian's 2026-11-01 rulings (v4, v5) (`consolidated_review_report.md`) |
| Date | 2026-11-01 |
| Author | Claude, for Adrian |
| Decision owner | Adrian |

## Problem

The suite has ten app repositories: `postright`, `scraperight`, `mailright`, `viewright`, `genright`, `CutRight`, `pulse`, `heardright`, `coderight` and `legion` (directory names under `/home/user`, case as on disk). Together they hold 3,874,805 lines of authored code. That count excludes docs, data and generated third-party notices. RightKit (`/home/user/rightkit`) is the shared kit: 69 Rust crates, 26 npm packages, and the Swift package `RightKitSwift` with 33 library modules. RightKit owns only a small share of that code. The same capability is written several times: four apps each keep their own media/transcript library, three apps each run their own approval flow, and two apps each have their own policy engine. A new app cannot be assembled mostly from existing packages, because most capabilities exist only inside one app.

## Goals

1. Every capability in app code becomes a public RightKit package (Rust crate, npm package, Swift library product or Android library). This includes capabilities only one app uses today, so that the next app can find them in RightKit instead of inside another app.
2. Each app keeps only:
   - its look and the way it composes the shared shell (for example, the HeardRight hub looks different from CodeRight's, on the same fused title bar and sidebar);
   - its declared per-app exceptions (for example, ViewRight allows devtools in its HTML iframe, as an app declaration over a `rightkit-tauri-shell` option);
   - its entry points, command tables, manifests, and journey/e2e tests.
3. Duplicated capabilities are merged into one package.

## Non-goals

- Changing product behaviour or scope.
- Setting release dates.
- Moving skills into RightKit. Skills, doctrine, agent personas and skill loaders are owned by Legion.

## Terms

- **App engine**: the Rust sidecar or daemon binary an app runs (for example `postright-engine`). Its *command table* is the list of commands it serves. That table stays in the app. *Wire types* are the request and response structs of those commands, and they ship in the package that implements the command.
- **Gate**: `rightkit-gate check` from `@rightkit/release`. It runs the app checks (G13–G22) and the ownership checks against `.rightkit/ownership-exceptions.json`.
- **Ownership exception**: an entry in an app's `.rightkit/ownership-exceptions.json`. It names a path in the app that should be owned by a RightKit package, plus the reason (usually an API gap in that package).
- **Membrane**: Pulse's context engine (`pulse/membrane/`): context pull, durable memory, repository graph and learning.
- **Product-specific**: an app's look, its composition of shared pieces, and its declared exceptions. Capability code is never product-specific, even if one app uses it.

## Inputs

- Master plan, with every package, its wave, consumers, size and contents: [`2026-11-01-rightkit-package-master-plan.md`](../2026-11-01-rightkit-package-master-plan.md).
- Per-package data: [`packages.json`](packages.json).
- Per-app file mapping: in each app repository, `docs/rightkit-inventory/<app>-rightkit-mapping.md` and `<app>-rightkit-files.json` (a pattern, not a link).
- Mapping rules and scripts: [`rules/`](rules/), [`tools/`](tools/). To regenerate, run `python3 tools/mapper.py <app> rules/<app>.tsv out`, then `tools/render.py` and `tools/master.py`, against each app's `main`.
- Review record: [`consolidated_review_report.md`](consolidated_review_report.md).

## Method

1. List every tracked code file on each app's `main` (commit recorded in each mapping file).
2. Assign each file one action using ordered path rules per app (the first match wins; a script fails if any file matches no rule):
   - NEW: becomes part of a new RightKit package.
   - EXTEND: added to an existing RightKit package. This includes ADOPT candidates that still have an ownership exception, because the RightKit owner lacks the API.
   - ADOPT: RightKit already ships it. Delete the app copy.
   - LEGION: skill code. It moves to Legion.
   - APPTEST: app journey or e2e test. It stays in the app.
   - VENDOR-OWN: vendored third-party code. Fork it into RightKit and remove the unused parts.
   - VENDOR-KEEP: vendored third-party code. Replace it with the upstream dependency and delete the copy.
   - DROP: delete.
   - SHELL / CONFIG: stays in the app.
   - NOTCODE: not counted.
3. Merge packages whose scope overlaps across apps. There are 35 merges, each with a reason, listed in the master plan.

## Result

| Measure | Lines |
|---|---:|
| Authored code in apps | 3,874,805 |
| NEW (85 new packages) | 1,975,724 |
| EXTEND (73 existing packages) | 772,649 |
| ADOPT (delete app copy) | 103,126 |
| LEGION (CutRight skills 43k, already in Legion 62k, Pulse 2k) | 106,600 |
| VENDOR-OWN + VENDOR-KEEP | 364,653 |
| DROP (includes CodeRight's skills crates: it uses the installed Legion) | 93,975 |
| APPTEST (stays in app) | 276,901 |
| SHELL + CONFIG (stays in app) | 181,177 (4.7%) |

New packages: 85 in total (63 crates, 16 npm packages, 6 Swift packages), all public. 26 have two or more consumers today; the rest have one.

## Proposed architecture

```
   apps: product logic & UI, shell, journey tests
        │ pin published versions
   ┌────┴───────────────────────────┐
 RightKit (public, crates.io / npm)   Legion (skills)
 foundation: fs, db, http, logs, shell …
 add-ons: library, pipeline, approvals, agent engine, mesh,
          context, timeline & editor, code audit, mail, social …
```

Dependency rules:

1. Apps depend on RightKit and Legion. RightKit never depends on an app or on Legion.
2. Within RightKit the layers are foundation, then add-on, then UI. A package depends only on its own layer or a lower one. A cycle fails the RightKit gate.
3. Mock data and mock servers stay with the app as APPTEST. Shared QA tooling lives in `rightkit-qa` and `@rightkit/qa`, as dev-dependencies only (§6).

### Development loop

RightKit's existing fast route is `right-release publish npm|cargo … --local` (`packages/release/local-publish.mjs`), batched by dependency layer. Changes are batched per family. Packages publish one at a time: each has its own `Cargo.lock`, compiles locally, qualifies on GitHub and is published automatically. RightKit is not released as one repo.

## Alternatives considered

1. **Extract only capabilities with two or more consumers** (Adrian's original rule). Rejected: a capability that sits inside one app cannot be found by the next app. Kept only as an ordering (Wave 2 before Wave 3).
2. **Product logic and UI stay in apps** (v4). Rejected by Adrian: product-specific means look, composition and declared exceptions, not capability.
3. **A private tier** (v3). Dropped: publishing is fine.
4. **One suite monorepo with path dependencies.** Rejected: it breaks published per-package pins and sealed bytes (P6, P8).
5. **Fork the DOCX editor (171k lines).** Rejected: `core` and `i18n` are identical to the published npm 1.9.0. Only `react` carries ViewRight's host-toolbar patch, which is small, carried by `@rightkit/docx-editor` and offered upstream.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Extraction cost for single-consumer packages | They come last (Wave 3). Each extraction needs a reviewed design doc (EGM). |
| Large packages become dependency hubs (`rightkit-code-audit` 256k, `rightkit-agent-engine` 129k, `rightkit-agent-server` 117k, `rightkit-context` 99k) | The design doc for each must split any package over 50k lines into layered crates before code moves. |
| Release throughput | Product changes stay in the app. Common code uses the local publish route, batched by layer. Hotfix route: a patch release from a release branch. |
| Security-sensitive code moves (approval tokens, credentials, SQLCipher stores, privileged helper, licence enforcement) | Each package's design doc includes a security review. Existing tests move with the code. |
| Mapping errors | Sample 50 random files per app against the mapping before each wave. Corrections go into `rules/`. |
| ADOPT deletes code RightKit does not replace | ADOPT rows that have an exception are already EXTEND. Before deleting, each ADOPT row needs a parity test: the app test suite must pass against the owner. |

## Implementation plan

### Decisions (resolved by Adrian, 2026-11-01)

- Every capability goes to RightKit. Product-specific means look, composition and declared exceptions.
- DOCX: use the published eigenpal packages and carry the React patch (see Alternatives 5).
- sherpa-onnx: already rewritten; delete the iOS copy once the iOS `rightkit-asr` build is tested.
- GenRight Node server: drop.
- Tree-sitter, WasmKit, ONNX Runtime binary, tauri-runtime-wry: use upstream dependencies; upstream the Tauri patch.
- Publication hold: lifted. Publishing starts on Adrian's go-ahead.
- Skills: owned by Legion. CodeRight uses the installed Legion, so its skill crates are deleted.

### Procedure per action

- **NEW / EXTEND**:
  1. Write a design doc and review it with EGM (`egm-draft`, then `egm-review`). It records the public API, which app implementation wins where apps differ, the layer, and the consumers.
  2. Create the package (its own `Cargo.lock` or lockfile), or add a module to the existing one, under `crates/`, `packages/` or `swift/`.
  3. Register it in `packages/release/rightkit-ownership.json` and `rightkit-versions.json`.
  4. Move the code and its unit tests.
  5. Compile locally, qualify on GitHub, publish automatically, one package at a time.
  6. Switch each consumer app to the published pin, one app per change.
  7. Delete the app copy, and its ownership exception if one exists.
  8. Pass that app's `rightkit-gate check` and journeys.
- **ADOPT**: add a parity check: the app's tests pass against the owner. Then rewire the call sites, delete the copy, and pass the gate.
- **LEGION**: move skill packs to Legion. Apps use the installed Legion.
- **VENDOR / DROP**: follow the decisions above. Delete the copy, and add the dependency or the fork.

### Waves

Every package has a wave in the master plan's tables.

- **Wave 0, decisions and deletions.**
  - ADOPT rows without an exception.
  - DROP rows.
  - LEGION moves.
  - Vendored-code resolution.
  - `rightkit-mesh`: Pulse built it to publish straight into RightKit, but it lives in the Pulse repo as `pulse-mesh`, with `publish = false`. Its default `pulse-link` feature depends on `pulse-core`, and it sits outside the Pulse workspace. Fix: move it into RightKit, rename it, make it publishable, and replace `pulse-link` with a trust trait that Pulse implements.
- **Wave 1, extend existing packages (73).** These close the API gaps that ownership exceptions record today. Order by number of consumer apps, then by line count.
- **Wave 2, new packages with two or more consumers.** Examples: `rightkit-library`, `rightkit-pipeline`, `rightkit-approvals`, `rightkit-agent-gateway`, `rightkit-agent-engine`, `rightkit-agent-orchestration`, `rightkit-agent-policy`, `rightkit-memory`, `rightkit-adapt`, `rightkit-repo-graph`, `rightkit-doctor`, `@rightkit/config`, `@rightkit/agent-ui`, `@rightkit/library-ui`, `rightkit-social-analytics`. Order by dependency layer.
- **Wave 3, new packages with one consumer today.** Order by dependency layer.

### Done when

For each app:

1. Regenerating the mapping against `main` shows only SHELL, CONFIG, APPTEST, NOTCODE and LEGION rows. LEGION rows are allowed in `legion` only.
2. Every ownership exception that named moved code is deleted. Exceptions on shell files are reviewed separately.
3. The app passes `rightkit-gate check` and its journeys against published pins.
