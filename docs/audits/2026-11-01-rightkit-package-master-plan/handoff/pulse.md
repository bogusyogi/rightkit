# Port handoff: Pulse

Repo: bogusyogi/pulse. Branch of this doc: `claude/fervent-ramanujan-661oob` in bogusyogi/rightkit (read-only for you; do not push rightkit main).
Read first: `docs/audits/2026-11-01-rightkit-package-master-plan/PORT-EXECUTION.md`, `DECISIONS.md`, `per-app/pulse-rightkit-mapping.md` (every row: NEW/EXTEND/ADOPT/VENDOR-KEEP/DROP/SHELL/CONFIG/APPTEST). Commission (Adrian): every capability moves into RightKit; app keeps only thin shell, exceptions, recipes, journey/e2e tests. An earlier survey said much was "app-specific, stays": WRONG, the mapping overrides it.

Per row: (a) RightKit package published -> delete app code + moved tests, depend on exact latest published pin, adapt call sites. (b) exists in rightkit but unpublished -> record BLOCKED-PUBLISH, wait. (c) published package lacks something / no package -> record NEEDS-EXTEND or MISSING-EXTRACT in the "RightKit asks" section of `handoff/ASKS-pulse.md`-style note to the publishing session (do not hack around). (d) DROP -> delete. VENDOR-KEEP -> plain dependency.

Gate rules: only `[profile.release]` with `panic = "unwind"`; no dev/debug builds, verify with `cargo check --release -p <crate>`; no test-mode features (tests isolate by injection); no `debug_assertions` dev/release switch (QA code behind `qa-native`); OS-specific code cfg-gated for 3 OSes; keep `@rightkit/release` =0.6.29; exact `=x.y.z` pins of PUBLISHED versions only (never path/git/unpublished).
Commits: small, per row group; verify (cargo check --release, tsc/vite where applicable) BEFORE pushing to main. Check published versions: crates.io index / `npm view`. Unpublished list and publish order are owned by the publishing session; check the registry before each row.

## State
On main (0fe5beea, 918878b9): core cleanup/scan/duplicates/folder_growth/snapshot -> rightkit-disk-cleanup =0.1.0; monitor/processes/gpu_temp/drive_health -> rightkit-system-monitor =0.1.0; `core/src/host.rs::init()`; root + engine `cargo check --release` clean; hub + Windows/macOS paths NOT compiled.
UNFINISHED: branch `claude/port-wip` (6e967003): an agent was stopped mid-run, 39 files, -30k lines, UNVERIFIED. Review the diff, compile, then merge or discard.
Skipped, needs RightKit: rightkit-mesh swap (published 0.1.1 lacks `agent-files` in hello.rs `KNOWN_CAPS`; without it `chat send --file` breaks), rightkit-app-manager (needs home-provider hook: Pulse's `account_home()` handles ~/ClaudeProfiles + QA fixture home). `[profile.dev]` blocks still in root, engine, mesh-ffi, hub, swift native, windows, winadmin: DELETE (gate rule).
Mapping is 673k lines (NEW 448k, EXTEND 84k, ADOPT 35k, vendor 71k, drop 23k): almost all still to do; check each against the registry.
