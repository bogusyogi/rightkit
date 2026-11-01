# rightkit-compositor 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-compositor =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- Blend modes: distinct shader codes and CSS kebab-case names.
- The frame descriptor: parsed from and serialised back to its camelCase wire shape.

See the header of `src/main.rs` for what it deliberately does not prove.
