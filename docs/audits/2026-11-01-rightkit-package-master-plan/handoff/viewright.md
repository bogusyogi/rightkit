# Port handoff: ViewRight

Repo: bogusyogi/viewright. Branch of this doc: `claude/fervent-ramanujan-661oob` in bogusyogi/rightkit (read-only for you; do not push rightkit main).
Read first: `docs/audits/2026-11-01-rightkit-package-master-plan/PORT-EXECUTION.md`, `DECISIONS.md`, `per-app/viewright-rightkit-mapping.md` (every row: NEW/EXTEND/ADOPT/VENDOR-KEEP/DROP/SHELL/CONFIG/APPTEST). Commission (Adrian): every capability moves into RightKit; app keeps only thin shell, exceptions, recipes, journey/e2e tests. An earlier survey said much was "app-specific, stays": WRONG, the mapping overrides it.

Per row: (a) RightKit package published -> delete app code + moved tests, depend on exact latest published pin, adapt call sites. (b) exists in rightkit but unpublished -> record BLOCKED-PUBLISH, wait. (c) published package lacks something / no package -> record NEEDS-EXTEND or MISSING-EXTRACT in the "RightKit asks" section of `handoff/ASKS-viewright.md`-style note to the publishing session (do not hack around). (d) DROP -> delete. VENDOR-KEEP -> plain dependency.

Gate rules: only `[profile.release]` with `panic = "unwind"`; no dev/debug builds, verify with `cargo check --release -p <crate>`; no test-mode features (tests isolate by injection); no `debug_assertions` dev/release switch (QA code behind `qa-native`); OS-specific code cfg-gated for 3 OSes; keep `@rightkit/release` =0.6.29; exact `=x.y.z` pins of PUBLISHED versions only (never path/git/unpublished).
Commits: small, per row group; verify (cargo check --release, tsc/vite where applicable) BEFORE pushing to main. Check published versions: crates.io index / `npm view`. Unpublished list and publish order are owned by the publishing session; check the registry before each row.

## State
No work done on this port. OPEN foundation row M1 (Window host): add `@rightkit/tauri`. Vendored docx-editor fork (209k lines): use published eigenpal core/i18n + the small patch carried by `@rightkit/docx-editor` (unpublished: BLOCKED-PUBLISH). 363k lines mapped.
