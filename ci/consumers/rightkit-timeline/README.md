# rightkit-timeline 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-timeline =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- Registering sources, inserting video clips, the timeline length in frames, a refused edit leaving the timeline unchanged, undo, and the OpenTimelineIO export passing the crate's own shape validator.

See the header of `src/main.rs` for what it deliberately does not prove.
