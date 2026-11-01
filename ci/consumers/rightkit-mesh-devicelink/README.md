# rightkit-mesh 0.1.1 (devicelink) registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-mesh =0.1.1` exactly with the `devicelink` feature. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the additive `devicelink` module is present in the published crate: a device identity derives a stable `dev_` id from its seed, the registry file starts empty, and circle pairing ids are symmetric.
