# rightkit-reframe 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-reframe =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- The face-detector model is pinned (file, size, SHA-256, fixed-commit https URL) and needs no entitlement.
- An empty model root resolves to nothing, with no download.

See the header of `src/main.rs` for what it deliberately does not prove.
