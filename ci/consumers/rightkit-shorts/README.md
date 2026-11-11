# rightkit-shorts 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-shorts =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- The default profile validates, has an ordered duration window for its primary platform, round-trips through JSON and refuses an unknown platform.
- The LLM request planner splits a long transcript into ordered requests within the per-request limit.

See the header of `src/main.rs` for what it deliberately does not prove.
