# rightkit-privileged-helper 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-privileged-helper =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published broker answers an empty poll, refuses an unknown answer, and runs work directly when no UI is polling. It never talks to a helper.
