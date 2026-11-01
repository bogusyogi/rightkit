# rightkit-library 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-library =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate opens an in-memory index, round-trips a record, and enriches text deterministically without network or model access.

It needs no external tools, network or model assets.
