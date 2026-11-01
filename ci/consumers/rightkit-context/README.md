# rightkit-context 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-context =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate exposes its exit codes, versioned output schema ids, and the protocol and runtime re-exports.
