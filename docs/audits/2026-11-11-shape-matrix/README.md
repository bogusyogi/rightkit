# RightKit shape matrix

Date: 2026-11-11. Issue: rightkit#1572.

Adrian, 2026-11-11: "find the best shape for rightkit, for all apps to move majority code to rk, but each crate/ package must be in the best shape. Create the full matrix then publish crates and packages". Earlier the same day: "What I had asked the agent to do was to check the cross app code and decide the best shape that all apps should adopt. HeardRight is the live app, followed by viewright and pulse is a product I use often, everything else is tbd and some like cut and post were started just before I decided on the right kit move", and "Final shape is no duplicate code".

## What is here

| File | What it is |
|---|---|
| `matrix.md` | One row per RightKit crate, npm package and Swift module; one column per app. Shape verdict, whose code the package holds, publication state, and per app: fit, pin state, planned lines and the share found in the package. Then the wrong-shape list, the missing list and the action list per package. |
| `matrix.json` | The same data, for tools. |
| `rows/` | The verdict rows each review produced. |
| `reports/` | The reviews the verdicts come from: nine on HeardRight against its shipped builds, and one per package family across all apps. `review-brief.md` is the instruction every family review followed. |
| `build-matrix.mjs` | Regenerates `matrix.md` and `matrix.json` from the port matrix, registry state, plan coverage and `rows/`. |

## How the verdicts were reached

- **Reference behaviour comes from shipped builds first.** HeardRight Windows 0.1.162 (commit `4cefb239`, 6 Sep) and Mac 0.1.213 (`12b7d8ce`, 30 Sep); ViewRight Mac 0.1.123 (`c9034ccb`, 19 Sep). Then Pulse, then the rest. Both HeardRight and ViewRight shipped with seven RightKit packages only (legal, legal-ui, license, logs, release, the Tauri adapter, updates); everything else in those builds was the app's own code.
- **Every verdict is from reading source.** Nothing was built or run for the reviews. Each report says where it read deeply and where lightly, and marks suspicions.
- **Shapes:** `best` = every feeding app can adopt it as it is; `gaps` = right design, listed changes needed; `wrong` = boundaries or API should be redrawn; `missing` = planned but not in RightKit.
- **Not covered:** Legion's source was not available, so packages only Legion feeds are judged on the package alone. Eight large agent-platform packages got a light read only (agent-server, agent-tools, terminal-ui, agent-orchestration, llm, agent-ui, chat-bridge, ai-accounts).

## What the 1 November plan got wrong

The master plan (`../2026-11-01-rightkit-package-master-plan.md`) sorted files into packages by path patterns. It did not compare implementations between apps. Its own review (finding M6) said boundaries "follow the source paths, not cohesive capabilities" and deferred the comparison to a design doc per package; those were never written. The consequences show up across the matrix:

1. **Where several apps feed one package, nobody's shape won by design.** Each app's code sits in its own crate or folder under a shared name: `rightkit-agent-engine` (Pulse's task contract in the base crate, CodeRight's turn loop in fourteen `-*` crates, nothing depends on the base), `rightkit-agent-gateway` (MailRight's) beside `rightkit-remote-gateway` (Pulse's), approvals in five places, `rightkit-memory` (CodeRight's client) beside `rightkit-memory-core` (Pulse's engine), `rightkit-adapt` and `rightkit-adapt-taste` (two unrelated subsystems), `@rightkit/library-ui` (two apps' screens in two folders).
2. **Product code was copied into foundation crates behind feature flags.** CodeRight's agent configuration is in `rightkit-app-settings`, its telemetry in `rightkit-logs`, its local-model code in `rightkit-models`, its daemon runtime in `rightkit-daemon`. These are crates every desktop app must adopt.
3. **Whole apps were moved as packages.** `rightkit-agent-server` is CodeRight's application (164k lines); `@rightkit/document-workspace` is the ViewRight application; `rightkit-ml-lab` is HeardRight's experiment scripts with machine paths in 60 files.
4. **Path rules sent code to the wrong package.** Examples: Apple Watch files to `RightKitWatch` (a Mac file watcher); iOS tests to `RightKitHubQA` (desktop fakes); HeardRight's usage-telemetry uploader to `rightkit-crash`; CloudKit, keychain and App Intents code to `rightkit-overlay`; CutRight's editorial decision log to `rightkit-spend`; 14k lines of ScrapeRight HTML fixtures to `rightkit-bench`.
5. **"Lines moved" was the wrong measure for the foundation set.** By 1 November most apps already pinned the same foundation versions. The lines the plan mapped there are mostly app configuration and product screens, which will not move.

## What is in good shape

- **The foundation packages the live apps would adopt are mostly right.** For HeardRight, 24 packages are best shape and 39 are right design with gaps; where a package took HeardRight code it is a faithful port of the shipped behaviour. By reading, main opens the live build's database, keychain items, data folder, device id and models in place.
- **Small, single-purpose crates are generally fine:** fs, child-process, sidecar, credentials, crypto, http, vad, audio, device, embed, search, ocr, typeset, doc-render.

## Decisions that are Adrian's

These cannot be settled from code.

1. **Transcript delivery success rule.** The two shipped builds differ: Windows 0.1.162 reports success on the clipboard receipt; Mac 0.1.213 also requires an exact read-back and never sends Enter without it. Main follows the Windows rule on both systems. Recommendation in `reports/hr-delivery-readback-design.md`: receipt or read-back on both; on Mac a bounded wait and no Return to editors that cannot be read.
2. **Delivery allowlist.** `rightkit-automation` permanently denies Claude desktop (`src/policy.rs`). Dictation needs to skip the allowlist and refuse only itself and credential surfaces.
3. **Approvals.** One core with store and proof traits, with Pulse's digest binding as the reference, replacing five implementations.
4. **Agent engine.** CodeRight's loop as the reference for the turn loop; Pulse's task contract as the reference for task authority and approval binding. One engine, one contract.
5. **`RightKitAgent` on phones.** CodeRight's Rust mobile runtime and Pulse's Swift loop are two different answers; one has to be chosen before the module is written.
6. **Agent product code in mandatory crates.** Move the `agent_*` features out of `rightkit-app-settings`, `rightkit-logs`, `rightkit-models` and `rightkit-daemon` into agent crates.
7. **DOCX.** The decision (published eigenpal packages plus a small toolbar patch) is not delivered: the patch is a source diff and the published editor ships only minified output. Either upstream the toolbar prop first or accept eigenpal's toolbar.
8. **ml-lab.** Not a shared package as it stands. Extract the ASR bake-off, WER harness and CoreML conversion scripts; the rest stays with HeardRight or is deleted.
9. **Crates no surveyed app imports:** `rightkit-passkeys`, `rightkit-sync-keys`, `rightkit-decimal`, `rightkit-identity`, `rightkit-scheduler`, `rightkit-github`, `rightkit-qa-driver`. Keep, or drop from the mandatory set.
10. **Changes after the live HeardRight build that reverse a shipped choice:** the active pill staying visible in full-screen apps; the pill visibility check moving from a 250 ms poll to OS events; the web default font moving from "hanken" to "system" while Rust still defaults to "hanken" and rejects "system".

## Order of work

1. **HeardRight, because it is live.** Switch main onto the RightKit copies it already duplicates, bottom-up (order in `reports/hr-review-dictation-chain.md`): about 132k lines on main already exist in RightKit, including ten `rightkit-dictation-*` crates (about 62k lines) that nothing consumes. Before that, eleven RightKit changes listed in that report, starting with the model-host rename bug in `rightkit-dictation-models` and bumping pins that are behind published fixes (license 0.2.9, overlay-win, hotkeys, app-settings 0.3.3+).
2. **Delivery into `rightkit-automation`** (17 steps, `reports/hr-delivery-readback-design.md`), after decisions 1 and 2.
3. **Take agent product code out of the mandatory crates** (decision 6), and split `rightkit-daemon` and `rightkit-models` along the lines in `reports/family-foundation-runtime.md`.
4. **Agent platform cores:** approvals, gateway, engine contract, then the agent-server split (`reports/family-agent-platform.md`).
5. **Documents:** dissolve `@rightkit/document-core`, make `@rightkit/document-workspace` a host-configured shell, one PDF viewer (`reports/family-documents.md`).
6. **Swift:** fix the mapping, one published pin, create `RightKitDictation` and `RightKitLibrary` (`reports/hr-review-ios-swift.md`).
7. Everything else per the action list in `matrix.md`.

## Redraws made on the branch (`claude/busy-hypatia-e30h7l`, not on `main`)

Each follows the recommendation above. Decisions 3 to 8 were not answered while Adrian was travelling, so these were made by the agent on the recommended option, are recorded as pending his confirmation in each crate's CHANGELOG, and can be reverted commit by commit.

| Change | Crates | Decision |
|---|---|---|
| CodeRight's agent configuration out of the settings store | `rightkit-app-settings` 0.4.0, new `rightkit-agent-config` 0.1.0 | 6 |
| CodeRight's telemetry out of logging | `rightkit-logs` 0.4.0, new `rightkit-agent-telemetry` 0.1.0 | 6 |
| Agent local-model code out of the model store | `rightkit-models` 0.6.0, new `rightkit-agent-local-models` 0.1.0 | 6 |
| One on-disk model store: `Store` writes HeardRight's suite layout and migrates MailRight's and GenRight's entries in place; multi-revision rollback is dropped | `rightkit-models` 0.7.0 and eleven consumers | model store (`reports/family-foundation-runtime.md`) |
| Agent runtime out of the daemon; remote access and app roots split out | `rightkit-daemon` 0.6.0 to 0.8.0, new `rightkit-agent-daemon`, `rightkit-daemon-remote`, `rightkit-roots` 0.1.0 | 6 and the daemon split |
| One neutral Tauri client; per-app wrappers deprecated | `@rightkit/tauri` 0.4.0 | `@rightkit/tauri` row |

Still to redraw: approvals (decision 3), the agent engine (decision 4), documents (decision 7 and the documents review).

## Publication state (measured 2026-11-11)

- crates.io: 164 of 210 publishable crates have their repository version published; 46 do not. npm: 28 of 49 public packages published; 21 do not. `packages/release/rightkit-versions.json` still lists 210 crates as staged, which is why generated docs print "not published" for crates that are live.
- Adrian, 2026-11-11: "Nah override that, publish via ci". The CI route (`qualify-direct.yml` then `publish.yml`) was dispatched for the first dependency wave of 13 crates and published nothing: 11 were refused because they have no tests (the publish code requires a passing test in four places, although GATE-CONTRACT allows a unit that ran zero tests), and the two that qualified failed in `publish.yml`'s scans job, which still expects one `Cargo.lock` per crate (the repository became one workspace with one lock in rightkit#1505). The route needs repairs that must land on `main`.
- Shape and publication: of the unpublished packages, the ones judged wrong shape should not be published as the shape apps adopt until redrawn: `rightkit-adapt`, `rightkit-app-host`, `rightkit-app-settings`, `rightkit-daemon`, `rightkit-models`, `@rightkit/document-core`, `@rightkit/document-workspace`, `@rightkit/tauri`, `@rightkit/video-ui`. Their unpublished versions are patch bumps of crates already on the registry, so publishing them changes nothing about shape; it only brings the registry level with the source.
