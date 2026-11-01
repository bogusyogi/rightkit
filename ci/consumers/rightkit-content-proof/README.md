# rightkit-content-proof 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-content-proof =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate reads claim numbers as exact decimals and finds every number token in a sentence.

It needs no external tools, network or model assets.
