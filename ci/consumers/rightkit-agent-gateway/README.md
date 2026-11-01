# rightkit-agent-gateway 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-agent-gateway =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate compiles against its public API only: profiles parse, sender text is wrapped as untrusted, errors serialise with a stable code and the redaction check finds a planted token, with no database or host installed.

It needs no external tools, network or model assets.
