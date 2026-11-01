# rightkit-context-runtime 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-context-runtime =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published runtime builds (including its build-script release identity), reports a release generation, keeps its on-disk product folder name, and digests bytes as `sha256:` hex.
