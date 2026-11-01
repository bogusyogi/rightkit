# rightkit-context-protocol 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-context-protocol =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate embeds every contract shape with its schema and fixture, exposes the operation index, and produces stable canonical JSON and `sha256:` digests.
