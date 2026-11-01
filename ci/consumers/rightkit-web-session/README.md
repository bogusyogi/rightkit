# rightkit-web-session 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-web-session =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate checks approved sites the way the network policy does (subdomains only) and reports no browser in a build without the browser feature.
