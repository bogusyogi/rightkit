# rightkit-clipboard-history 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-clipboard-history =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate's pure logic API works with no operating system access.
