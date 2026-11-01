# rightkit-mail 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-mail =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate compiles a Gmail-style query, fuses two rankings, parses Authentication-Results and recognises a tracker host, with no database or network.

It needs no external tools, network or model assets.
