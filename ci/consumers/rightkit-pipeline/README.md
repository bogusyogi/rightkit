# rightkit-pipeline 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-pipeline =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the deprecated crate still exposes the orchestrator API under its old paths (re-exported from `rightkit-media-gen-client`), refuses a missing engine without running it, and uses a product-neutral default consumer.

See the header of `src/main.rs` for what it deliberately does not prove.
