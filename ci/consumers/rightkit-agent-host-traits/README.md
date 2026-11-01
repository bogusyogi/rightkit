# rightkit-agent-host-traits 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-agent-host-traits =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate exposes the `SkillCatalog` trait and its installation point, and that with no catalog installed the runtime sees none.
