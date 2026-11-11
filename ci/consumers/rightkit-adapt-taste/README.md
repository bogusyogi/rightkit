# rightkit-adapt-taste 0.1.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-adapt-taste =0.1.2` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the deprecated name still resolves its old paths (it re-exports `rightkit-adapt` 0.2.0): the published crate canonicalizes JSON and derives stable preference ids.
