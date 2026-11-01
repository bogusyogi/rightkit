# rightkit-chat-bridge 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-chat-bridge =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate keeps a neutral default brand with the wire identifiers existing installs use, reads the old and new link keys, and strips markup from chat labels.
