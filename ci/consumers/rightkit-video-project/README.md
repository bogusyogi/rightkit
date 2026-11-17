# rightkit-video-project registry consumer

Standalone consumer outside the workspace. Its manifest pins the exact crate version. It installs from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- A project is created, re-created idempotently, opened for writing with an empty timeline at the requested frame rate, saves its timeline and metadata, and opens again read-only.
- The `engine-commands` feature exports the five batch delivery commands.

See the header of `src/main.rs` for what it deliberately does not prove.
