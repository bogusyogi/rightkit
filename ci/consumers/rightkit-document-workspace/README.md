# rightkit-document-workspace 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-document-workspace =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate opens a fresh store in a temp folder, records and lists a recent file, toggles a pin and purges stored paths.

It needs no external tools, network or model assets.
