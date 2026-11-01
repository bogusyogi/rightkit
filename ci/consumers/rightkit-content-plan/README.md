# rightkit-content-plan 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-content-plan =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate enforces the first-sentence disclosure rule and the per-platform posted-link host table.

It needs no external tools, network or model assets.
