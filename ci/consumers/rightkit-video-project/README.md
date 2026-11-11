# rightkit-video-project 0.1.3 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-video-project =0.1.3` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- A project is created, re-created idempotently, opened for writing with an empty timeline at the requested frame rate, saves its timeline and metadata, and opens again read-only.

See the header of `src/main.rs` for what it deliberately does not prove.
