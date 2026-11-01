# rightkit-system-monitor 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-system-monitor =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate reads system status and the process table on the runner, groups an empty process list, and takes the host app's state folder.
