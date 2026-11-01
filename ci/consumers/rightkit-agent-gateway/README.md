# rightkit-agent-gateway 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-agent-gateway =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate refuses an unknown tool before touching settings, reads its bundled tool manifest, keeps neutral default engine names and re-exports redaction.
