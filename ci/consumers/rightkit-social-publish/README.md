# rightkit-social-publish 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-social-publish =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate counts X post length, parses Retry-After, refuses a non-loopback base URL unless allowed, and redacts OAuth values, with no network.

It needs no external tools, network or model assets.
