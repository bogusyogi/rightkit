# rightkit-memory-core 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-memory-core =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published core estimates tokens, embeds deterministically with the hash embedder, and exposes the memory tier types.
