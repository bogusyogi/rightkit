# rightkit-agent-gateway 0.3.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-agent-gateway =0.3.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published gateway core compiles against its public API only: a host-defined level and method table go through the gate's checks with stable codes, a call whose audit row fails gets no result unless the row is best effort, doors are named, third-party text is wrapped and stamped, a secret is masked and the leak check finds a planted token, with no database or host installed.

It needs no external tools, network or model assets.
