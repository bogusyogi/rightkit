# rightkit-color 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-color =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- The shipped presets parse and have unique ids; an unknown preset is none.
- An empty plan on Rec.709 builds a format-and-tag-only filter with no files.
- A malformed .cube LUT is refused.

See the header of `src/main.rs` for what it deliberately does not prove.
