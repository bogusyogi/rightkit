# rightkit-media-gen-client 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-gen-client =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate exposes the orchestrator and the typed client, refuses a missing or relative engine path without running anything, uses a product-neutral default consumer, maps a progress line to a `media_gen.*` event, and parses the model catalog's label durations.

See the header of `src/main.rs` for what it deliberately does not prove.
