# rightkit-context-provider-sdk 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-context-provider-sdk =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published SDK builds an empty provider registry, re-exports the protocol operation index, and canonicalizes JSON.
