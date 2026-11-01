# rightkit-adapt-taste 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-adapt-taste =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate canonicalizes JSON, normalizes text, and derives stable preference ids.
