# rightkit-context-client 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-context-client =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published client reports `NotRunning` (safe to fall back in process) when no resident listens under an empty state root, and exposes its timing budgets.
