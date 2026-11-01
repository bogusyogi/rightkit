# rightkit-app-manager 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-app-manager =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate validates bundle ids, compares versions, and takes the host app's identity once. It installs, quits and removes nothing.
