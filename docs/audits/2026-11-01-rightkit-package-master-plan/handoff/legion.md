# Port handoff: Legion

Repo: orthic-labs/legion. Branch of this doc: `claude/fervent-ramanujan-661oob` in bogusyogi/rightkit (read-only for you; do not push rightkit main).
Read first: `docs/audits/2026-11-01-rightkit-package-master-plan/PORT-EXECUTION.md`, `DECISIONS.md`, `per-app/legion-rightkit-mapping.md` (every row: NEW/EXTEND/ADOPT/VENDOR-KEEP/DROP/SHELL/CONFIG/APPTEST). Commission (Adrian): every capability moves into RightKit; app keeps only thin shell, exceptions, recipes, journey/e2e tests. An earlier survey said much was "app-specific, stays": WRONG, the mapping overrides it.

Per row: (a) RightKit package published -> delete app code + moved tests, depend on exact latest published pin, adapt call sites. (b) exists in rightkit but unpublished -> record BLOCKED-PUBLISH, wait. (c) published package lacks something / no package -> record NEEDS-EXTEND or MISSING-EXTRACT in the "RightKit asks" section of `handoff/ASKS-legion.md`-style note to the publishing session (do not hack around). (d) DROP -> delete. VENDOR-KEEP -> plain dependency.

Gate rules: only `[profile.release]` with `panic = "unwind"`; no dev/debug builds, verify with `cargo check --release -p <crate>`; no test-mode features (tests isolate by injection); no `debug_assertions` dev/release switch (QA code behind `qa-native`); OS-specific code cfg-gated for 3 OSes; keep `@rightkit/release` =0.6.29; exact `=x.y.z` pins of PUBLISHED versions only (never path/git/unpublished).
Commits: small, per row group; verify (cargo check --release, tsc/vite where applicable) BEFORE pushing to main. Check published versions: crates.io index / `npm view`. Unpublished list and publish order are owned by the publishing session; check the registry before each row.

## State
No work done on this port. 663k lines (NEW 375k, EXTEND 199k, to-Legion 71k). Legion owns its own skills. engine/crates is huge: take mapping rows by line count, largest first.
