# rightkit-social-inbox 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-social-inbox =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate verifies a webhook signature it produced, refuses a tampered one, parses a delivery into an event and matches a keyword, with no network or store.

It needs no external tools, network or model assets.
