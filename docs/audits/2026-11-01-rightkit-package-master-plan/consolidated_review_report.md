# EGM Goldfish review: consolidated report (design_draft v1)

Date: 2026-11-01. Protocol: EGM `egm-review`, consolidated preset. Each reviewer was a fresh-context subagent that read the EGM persona file and document standard from the plugin (`egm@egm` 0.1.0). Stages run: 5 (Zoom Out), 6 (Comprehension), 7 (Design Critic, Structural, Style) and 9 (Implementation Readiness, Codebase Details). Stage 8 was run on v2 and is recorded at the end.

## Blockers

| # | Finding | Sources | Resolution in v2 |
|---|---|---|---|
| B1 | Moving proprietary app code into public RightKit would publish it under MIT/Apache: RightKit CI uploads every qualified unit to crates.io/npm, while the apps are "All rights reserved" | Zoom Out 1, Design Critic 2 | Added two tiers: public stays as today; product domain code goes to a private registry. D1/D2 added as preconditions. |
| B2 | The goal contradicts P13 ("app-specific product logic and presentation stays in the app") | Zoom Out 2 | v2 states the conflict and proposes amending P13 so app-specific code goes to the private tier (D1). |
| B3 | No Alternatives Considered section | Structural 1, Design Critic 1 | Added five alternatives. |
| B4 | 59 EXTEND packages and several NEW packages were in no wave | Readiness 5, Codebase 7 | Every package now has a wave in the master plan. Wave 1 = all 73 EXTEND. |
| B5 | Wave 0 contained open decisions (vendored code, DOCX, sherpa) | Readiness 4, Codebase 4, Zoom Out 11 | These are now decisions D1–D6, ahead of any wave. |
| B6 | ADOPT rows include files whose ownership exception says RightKit lacks the API | Codebase 1 | Mapping fixed: any ADOPT file with an exception becomes EXTEND ("API gap first"). |
| B7 | About 122k "ADOPT" lines were generated third-party licence HTML, which each app must ship | Codebase 2, Design Critic 6, Zoom Out 8 | Reclassified as NOTCODE and removed from code totals. ADOPT is now 103,520 lines. |
| B8 | "Done when" could not be met: no ownership-exceptions file in Legion, exceptions on shell files, VENDOR-KEEP and DROP rows | Codebase 3, Readiness 12, Comprehension 7 | Done-when rewritten: allowed rows listed, LEGION allowed only in `legion`, shell exceptions reviewed separately. |
| B9 | The extraction procedure left the registry, the catalog registration and the publication route open, and did not fit ADOPT, LEGION or vendored code | Readiness 2 | Procedure is now written per action, naming the catalog files and routes. The publication hold is D5. |
| B10 | `rightkit-mesh` is not a mechanical move: the crate is `pulse-mesh`, `publish = false`, and its `pulse-link` feature depends on `pulse-core` | Readiness 3, Codebase 5 | Steps added: rename, publishable, trust-adapter trait. Core first, then the other mesh parts. |
| B11 | Wave 1 "short design note" left the merge decisions to the implementer | Readiness 1 | Every NEW/EXTEND package now needs an EGM-reviewed design doc before code moves. |

## Major

| # | Finding | Resolution |
|---|---|---|
| M1 | Risks had no mitigations; security was not covered | Risks table with mitigations, plus a security row. |
| M2 | No architecture, layering or dependency rules | Proposed architecture section: tiers, layers, four dependency rules. |
| M3 | Result table did not add up; the Legion figure was wrong; master coverage table header was missing a column | Table regenerated with every action; header fixed. |
| M4 | Release-first cadence was a dead end | Development override, batched releases and a hotfix route. |
| M5 | Mock and e2e code was placed in shipped library packages | New action APPTEST: app journeys and e2e tests stay with the app (252k lines). Mock packages are dev-dependencies only. |
| M6 | Package boundaries follow the source paths, not cohesive capabilities | Boundaries are provisional until each package's design doc. Packages over 50k lines must be split in their doc. |
| M7 | Skills boundary leaked: orchestration skill checks, chat skill install, and CodeRight skill routes | Legion runtime skill/designer checks and Pulse chat skill install now map to LEGION. CodeRight's skill HTTP routes stay in the shell. |
| M8 | HeardRight settings were misplaced in `rightkit-library`; `app_paths` is shell | Settings now map to `rightkit-app-settings` EXTEND; `app_paths` is SHELL. |
| M9 | MailRight approvals were mapped to the gateway, not to approvals | MailRight approval files now map to `rightkit-approvals`; the IPC client maps to `@rightkit/agent-ui`. |
| M10 | Undefined terms: gate, ownership exception, Membrane, engine wire types | Terms section added. |
| M11 | Rust config presets cannot come from an npm package | `rightkit-gate` writes them into each app. |
| M12 | "Thin shell" was not true for Pulse, Legion, CodeRight and HeardRight | Shell target stated as at most 10% of code. All apps meet it (max 7%). Runtime pieces stay marked for review in the design docs. |
| M13 | The new Python mapper duplicates RightKit's ownership catalog and fingerprints | Not adopted for this audit. Proposed follow-up: record each package's tier and class in `rightkit-ownership.json`, so the gate checks it. |
| M14 | "Exactly one rule" was not proven (first match wins) | Wording corrected. |

## Contested (not changed)

- Zoom Out 3 / Design Critic 10 (single-consumer extraction is YAGNI): kept, because Adrian set the factory goal. Mitigated by ordering (private and single-consumer packages last) and by per-package design docs, which can defer a package.

## Stage 8 (Intermediate Zoom Out on v2)

See `intermediate_zoom_out_report.md`.

Stage 8 result on v2: NEEDS REVISION. Resolved in v3 (see `intermediate_zoom_out_report.md`), except M-4, which is contested on Adrian's goal.

## v4 (Adrian, 2026-11-01)

Adrian ruled: publishing everything public is fine, and product-specific logic and UI stay in apps (P13 as written). As a result:
- The private tier is dropped. This resolves B1, the Stage 8 findings B-1, M-1, M-2, M-4 and M-5, and N-3.
- 39 single-app product packages now map to PRODUCT and stay in their apps.
- The 6 multi-app ones remain RightKit add-ons.
- 46 new packages remain.

## v5 (Adrian, 2026-11-01)

Adrian clarified that "product-specific" means only an app's look, its composition of the shared shell, and its declared exceptions. Example: ViewRight's devtools in the HTML iframe. Every capability goes to RightKit, single-app ones included. As a result:
- The v4 PRODUCT reclassification is reverted: 85 new packages, all public.
- DOCX: use the published eigenpal packages and carry the React patch; no fork.
- CodeRight's skill crates are dropped, because CodeRight uses the installed Legion.
- The sherpa iOS copy is dropped after the iOS test.
- The publication hold is lifted. Publishing is per package, on Adrian's go-ahead.
