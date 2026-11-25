# rightkit-assist 0.1.1 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-assist =0.1.1` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate's public items build and behave as documented: pure values only, no network and no disk.
