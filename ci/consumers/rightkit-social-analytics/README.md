# rightkit-social-analytics 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-social-analytics =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate derives a well-formed link id, tags a destination with default UTM values and reduces a referrer to its host.

It needs no external tools, network or model assets.
