# Port execution: the commission, so it is not lost again

**Commission (Adrian, 2026-11-01):** survey every app, find all code that is not in RightKit, and bring it into RightKit. Every capability goes to RightKit, single-app ones included. After the port an app keeps only: a thin shell (entry points, command tables, wiring; at most 2%), declared per-app exceptions, app recipes/skills, and its own journey/e2e tests. Unit tests move with their code. See `DECISIONS.md` (resolved 2026-11-01) and the master plan `../2026-11-01-rightkit-package-master-plan.md`.

**Scale (master plan):** 3,873,978 lines across 10 apps; ~3.0M lines move (NEW 2.04M, EXTEND 0.83M, ADOPT 0.10M, to Legion 0.07M, vendor 0.36M, DROP 0.09M). 86 new packages: 64 crates, 16 npm, 6 Swift (`plan-86-packages.txt`).

**Per-app worklist:** `per-app/<app>-rightkit-mapping.md` lists every RightKit target with action (NEW / EXTEND / ADOPT / VENDOR-KEEP / DROP / SHELL / CONFIG / APPTEST) and the app paths. Per-file assignment: `<app>-rightkit-files.json` (scratch copy of the generator output). "Done" for an app = every NEW/EXTEND/ADOPT/DROP/VENDOR-KEEP row has its app code deleted and the app consuming the published RightKit package at the exact latest published pin.

**Order of work per package:** (1) RightKit package exists with the app's code (extraction); (2) package published (crates.io / npm) via the direct route; (3) app deletes its copy and depends on the published exact pin; (4) matrix regenerated (`node scripts/generate-index.mjs ...`): cell `adopted`.

**Gate rules for apps (rkn, 2026-11-02/03):** only `[profile.release]` with `panic = "unwind"`; no dev/debug builds anywhere; no test-mode features (tests isolate by injection); no `debug_assertions` as a dev/release switch (QA code behind `qa-native`); three-OS checks, OS-specific code cfg-gated.

**Status of the RightKit side (2026-11-03):** ~130 of ~170 crates and 32+4 npm packages published; 40 crates and 9 npm packages pending (qualify/publish runs in flight); `rightkit-asr` 0.4.0 (Rust KWS, Sherpa removed) merged to main, unpublished.
