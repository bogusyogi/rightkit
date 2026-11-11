# rightkit-autoedit 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-autoedit =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- Editing rules: a channel rule beats platform, content-type and project rules for the same parameter; the losers are reported as overridden by it; the resolution is clean.

See the header of `src/main.rs` for what it deliberately does not prove.
