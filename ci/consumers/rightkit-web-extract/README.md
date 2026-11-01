# rightkit-web-extract 0.1.1 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-web-extract =0.1.1` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate parses readable text from inline HTML, applies the fit filters, and classifies ladder errors, with no network.

It needs no external tools, network or model assets.
