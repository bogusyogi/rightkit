# rightkit-automation 0.7.1 registry consumer (`paths`, `desktop-context` features)

Standalone consumer, outside the workspace, pinning `rightkit-automation =0.7.1`
exactly with `paths` and `desktop-context`, and `rightkit-daemon =0.9.0` exactly so the
returned `Suite` type can be named. No path, Git, workspace, patch, or
test-seam dependency, and no `Cargo.lock` committed in this directory (the
validator rejects it); the harness pins and retains lock output outside the
recipe.

Runs on any host: no desktop, window, or input is touched. No runner assets, no
network, no real user data. State is one per-run temp suite directory under the
OS temp dir, removed when the `Suite` drops.

## What it proves

On Windows, type-checks references to the pre-existing public UIA API without
initializing COM or reading UI state. On macOS, checks the retained tagged paste
probe's limits and start API without starting an event tap. New Windows delivery
surface is excluded from this candidate.

| Proof row | Executed behavior |
|---|---|
| `per_run_suite_type` | `paths::per_run_evidence_dir(label)` returns a value bound as `(rightkit_daemon::Suite, PathBuf)`, so the returned `Suite` is the daemon 0.9 type; `env_derived()` is false and `root()` is absolute and under the OS temp dir. |
| `evidence_dir_contract` | The `PathBuf` is absolute, exists, and equals `<suite root>/automation`; after the `Suite` drops both it and the suite root are gone. |
| `pure_helpers` | `credential_label` flags `Password` and `API key` and not `Search`; `sensitive_value` returns `a card number` for a Luhn-valid number and `None` for plain text. |

## Not covered

`paths::evidence_dir` is not called: it resolves the real per-user suite root
(honouring `RIGHTKIT_SUITE_ROOT`) and would touch real user data. Also not
exercised: the macOS AX and Windows UIA backends, the input ladder, capture,
focus guard, leases, and evidence journals. This recipe adds no feature,
version, or public API and runs no commands, builds, or tests while being
prepared.
