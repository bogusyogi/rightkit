# rightkit-disk-cleanup 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-disk-cleanup =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate scans a small temp tree without following links, refuses malformed snapshot ids, and takes the host app's state folder name.
