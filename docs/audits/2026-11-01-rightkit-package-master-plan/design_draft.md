# Design: all suite code in RightKit packages, in two tiers

| Field | Value |
|---|---|
| Status | Draft v3, revised after EGM Goldfish review (stages 5–9) and the Stage 8 re-check (`consolidated_review_report.md`) |
| Date | 2026-11-01 |
| Author | Claude, for Adrian |
| Decision owner | Adrian |

## Problem

The suite has ten app repositories: `postright`, `scraperight`, `mailright`, `viewright`, `genright`, `CutRight`, `pulse`, `heardright`, `coderight` and `legion` (directory names under `/home/user`, case as on disk). Together they hold 3,874,805 lines of authored code. That count excludes docs, data and generated third-party notices. RightKit (`/home/user/rightkit`) is the shared kit: 69 Rust crates, 26 npm packages, and the Swift package `RightKitSwift` with 33 library modules. RightKit owns only a small share of that code. The same capability is written several times: four apps each keep their own media/transcript library, three apps each run their own approval flow, and two apps each have their own policy engine. A new app cannot be assembled mostly from existing packages, because most capabilities exist only inside one app.

## Goals

1. Every capability in app code becomes a RightKit package: a Rust crate, an npm package, a Swift library product, or an Android library.
2. Each app keeps only its shell: entry points, command tables, wiring, manifests and its own journey/e2e tests. The target is at most 10% of an app's code.
3. Duplicated capabilities are merged into one package.
4. Product code stays private. Generic code stays public, as it is today.

## Non-goals

- Changing product behaviour or scope.
- Setting release dates.
- Moving skills into RightKit. Skills, doctrine, agent personas and skill loaders are owned by Legion.

## Terms

- **App engine**: the Rust sidecar or daemon binary an app runs (for example `postright-engine`). Its *command table* is the list of commands it serves. That table stays in the app. *Wire types* are the request and response structs of those commands, and they ship in the package that implements the command.
- **Gate**: `rightkit-gate check` from `@rightkit/release`. It runs the app checks (G13–G22) and the ownership checks against `.rightkit/ownership-exceptions.json`.
- **Ownership exception**: an entry in an app's `.rightkit/ownership-exceptions.json`. It names a path in the app that should be owned by a RightKit package, plus the reason (usually an API gap in that package).
- **Membrane**: Pulse's context engine (`pulse/membrane/`): context pull, durable memory, repository graph and learning.
- **Tier**: *public* packages are generic. They are MIT/Apache licensed and published by CI to crates.io and npm (current RightKit). *Private* packages are product domain code. They are proprietary and published only to a private registry.

## Inputs

- Master plan, with every package, its tier, wave, consumers, size and contents: [`2026-11-01-rightkit-package-master-plan.md`](../2026-11-01-rightkit-package-master-plan.md).
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
| ADOPT (delete app copy) | 103,182 |
| LEGION (from other apps 63k, already in Legion 55k) | 117,695 |
| VENDOR-OWN + VENDOR-KEEP | 336,454 |
| DROP | 111,079 |
| APPTEST (stays in app) | 276,901 |
| SHELL + CONFIG (stays in app) | 181,121 (4.7%) |

New packages: 85 in total. By kind, 63 crates, 16 npm packages and 6 Swift packages. By tier, 40 public and 45 private. Of the 73 existing packages that gain code, 23 gain it from only one app, and each of those needs a tier check (D4).

## Proposed architecture

### Two tiers

```
                 apps (shell + journey tests only)
                    │ pins published versions
        ┌───────────┴────────────────────────────┐
  private tier (45 pkgs, repo rightkit-private)                   Legion (skills)
  product domain: mail, social,             skill packs, doctrine,
  video editor, code audit, agent server,   personas, skill loaders
  dictation, system utilities, editors,     & host projection
  product UIs
        │ depends on (never the reverse)
  public tier (existing 128 + 41 new)
  generic: foundation (fs, db, http, logs, shell …), add-ons
  (library, pipeline, approvals, agent engine, mesh, context …)
  repo: public rightkit (unchanged)
```

Dependency rules:

1. A public package never depends on a private package.
2. Within a tier, the layers are foundation, then domain, then UI. A package depends only on the same layer or a lower one. A cycle fails the RightKit gate, which already resolves the crate graph in dependency order.
3. Apps depend on packages. Packages never depend on apps or on Legion.
4. Mock data and mock servers stay with the app as APPTEST (for example PostRight's social platform mock); shared QA tooling lives in `rightkit-qa` / `@rightkit/qa` as dev-dependencies only. They never compile into a release build (§6, "no QA, test, mock or dev-control code in a release build").

### Private tier: repository, pipeline and distribution

- **Source.** A separate private repository, `rightkit-private`, holds all private-tier packages and their own catalog: its own `rightkit-ownership.json` and `rightkit-versions.json`. Private code never enters public `rightkit`, its catalog, or the public `@rightkit/release` package.
- **Pipeline.** It uses the existing private-repository route (`docs/PIPELINE-ARCHITECTURE.md` §1 ③): qualify locally on the Mac and the Dell, then stage. It does not use the public GitHub CI, which refuses private repositories.
- **Distribution.** One mechanism for all package kinds: git dependencies pinned to a tagged commit of `rightkit-private` (Cargo `git` + `rev`, npm git URL + commit, SwiftPM exact revision). This avoids running three private registries. The gate must accept these pins for private-tier packages only. That gate change is Wave 0 work.
- **Shared with Legion.** Legion is proprietary too and must ship crates to CodeRight (D6). It uses the same channel.
- **Dependencies.** Private packages depend on public RightKit through normal published pins.

### Development loop

No new override. RightKit already has a fast local route: `right-release publish npm|cargo … --local` (`packages/release/local-publish.mjs`, Adrian's 2026-10-14 local-build grant), which batches by dependency layer. The release-throughput mitigation is: batch changes per family and publish locally when CI is too slow. Pointing an app at an editable checkout would break GATE-CONTRACT GC-T06 and GC-T14, so it is not proposed.

### Relationship to P13

P13 (`docs/PIPELINE-ARCHITECTURE.md`, 2026-10-24) says app-specific product domain logic and presentation stay in the app. This design changes that rule: the same code moves to the **private** tier instead. That keeps P13's intent, which is to keep product code out of the public kit, and also meets the factory goal. Adopting this design requires an amended P13. The proposed text: "product domain logic and presentation is a private-tier package; the app keeps only its shell. Until its wave, product code stays in the app, tracked by this plan rather than by 90-day ownership exceptions."

A narrower amendment was considered and rejected: product code *may* stay in the app, and *must* move when a second app needs it. It is cheaper (Wave 4 would shrink to the 6 multi-consumer private packages), but it does not meet Adrian's goal that no app code sits outside the kit.

## Alternatives considered

1. **Move only foundation and add-on code (P13 as written).** Rejected: product code stays locked inside each app, and a new app cannot reuse a video editor, mail mirror or dictation runtime. It does keep extraction cost lower; see Risks.
2. **Extract only capabilities with two or more consumers.** Rejected as the end state, because 60 packages have one consumer today. It is adopted as the ordering: multi-consumer packages go first (Wave 2).
3. **One suite monorepo with path dependencies, no publishing.** Rejected: it breaks the published-pin and sealed-bytes rules (P6, P8) and the per-app release model, and it puts private and public code in one repository.
4. **Everything in public RightKit.** Rejected: it publishes proprietary product source under MIT/Apache.
5. **DOCX editor stays an upstream npm dependency.** This is open, and Adrian decides (D3). A fork is proposed because ViewRight already modifies it.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Extraction cost exceeds reuse for single-consumer packages | Private-tier packages come last (Wave 4). Each extraction needs a reviewed design doc (EGM). A package can be deferred by recording it as an app-owned exception with an expiry. |
| Large packages become dependency hubs (`rightkit-code-audit` 256k, `rightkit-agent-engine` 129k, `rightkit-agent-server` 117k, `rightkit-context` 99k) | The design doc for each must split any package over 50k lines into layered crates before code moves. |
| Release throughput: every change needs a RightKit release | Local publish route (`--local`, batched by layer). Batched releases per family. Hotfix route: a patch release from a release branch. |
| Product IP is exposed | The tier rule, plus a gate check that refuses a public package depending on a private one. |
| Security-sensitive code moves (approval tokens, credentials, SQLCipher stores, privileged helper, licence enforcement) | The privileged helper is a private-tier package. App-side licence enforcement code (entitlement resolvers, pro bridges, trials) is reviewed under D4: any part that weakens enforcement if public goes to the private tier instead of extending `rightkit-license`. Each move keeps its existing tests and adds a security review in its design doc. |
| Mapping errors | Sample 50 random files per app against the mapping before each wave. Corrections go into `rules/`. |
| ADOPT deletes code RightKit does not replace | ADOPT rows that have an exception are already EXTEND. Before deleting, each ADOPT row needs a parity test: the app test suite must pass against the owner. |

## Implementation plan

### Preconditions (decisions D1–D6, by Adrian)

- D1: Accept two tiers and amend P13.
- D2: Choose the private registry.
- D3: Vendored code, one decision per row of the master plan's vendored table:
  - DOCX: fork or keep as a dependency.
  - Tree-sitter grammars: switch to crates.io.
  - WasmKit: switch to SwiftPM.
  - tauri-runtime-wry: upstream the patch.
  - sherpa-onnx: replace with `rightkit-asr`.
  - GenRight legacy server: drop.
- D4: Confirm the tier of each new package (the master plan proposes one), and of each of the 23 extended packages marked `check`. Check every tier against the import edges in the mapping data, not only by family: a public package must not import private code.
- D5: Lift the publication hold in `docs/GATE-CONTRACT.md`, or schedule waves after it.
- D6: Legion distributes its skill crates to CodeRight through the private channel (crate names).

### Procedure per action

- **NEW / EXTEND**:
  1. Write a design doc and review it with EGM (`egm-draft`, then `egm-review`). It records the public API, which app implementation wins where apps differ, the layer and tier, and the consumers.
  2. Create the package, or add a module to the existing one, under `crates/`, `packages/` or `swift/` of `rightkit` (public tier) or `rightkit-private` (private tier).
  3. Register it in that repository's `rightkit-ownership.json` and `rightkit-versions.json`.
  4. Move the code and its unit tests.
  5. Publish: through CI qualification or `--local` (public), or by tagging `rightkit-private` after local qualification (private).
  6. Switch each consumer app to the published pin, one app per change.
  7. Delete the app copy, and its ownership exception if one exists.
  8. Pass that app's `rightkit-gate check` and journeys.
- **ADOPT**: add a parity check: the app's tests pass against the owner. Then rewire the call sites, delete the copy, and pass the gate.
- **LEGION**: move the code to Legion. The consumer then pins Legion's published crate (D6).
- **VENDOR / DROP**: follow D3. Delete the copy, and add the dependency or the fork.

### Waves

Every package has a wave in the master plan's tables.

- **Wave 0, decisions, private channel and deletions.** Decisions D1–D6. Create `rightkit-private` and its catalog. Gate support for private git pins. Then ADOPT rows without an exception, DROP rows, LEGION moves (the cross-app moves need the private channel) and vendored-code resolution.
- **Wave 1, extend existing packages (73).** These close the API gaps that ownership exceptions record today. Order by number of consumer apps, then by line count.
- **Wave 2, new public packages with two or more consumers (20).** Examples: `rightkit-library`, `rightkit-pipeline`, `rightkit-approvals`, `rightkit-agent-gateway`, `rightkit-agent-engine`, `rightkit-agent-orchestration`, `rightkit-agent-policy`, `rightkit-memory`, `rightkit-adapt`, `rightkit-repo-graph`, `rightkit-doctor`, `@rightkit/config`. Rust config presets in `@rightkit/config` are written into each app by `rightkit-gate`, not read from npm. Within the wave, order by dependency layer. Mesh core goes first; it needs a decoupling design (see its design doc).
- **Wave 3, new public packages with one consumer (20).**
- **Wave 4, new private packages (45).** Needs D1 and D2. Order by dependency layer, then by the app whose release is next on Adrian's release list.

### Done when

For each app:

1. Regenerating the mapping against `main` shows only SHELL, CONFIG, APPTEST, NOTCODE and LEGION rows. LEGION rows are allowed in `legion` only.
2. Every ownership exception that named moved code is deleted. Exceptions on shell files are reviewed separately.
3. The app passes `rightkit-gate check` and its journeys against published pins.
