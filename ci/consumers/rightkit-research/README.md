# rightkit-research registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-research` to the exact version in its `Cargo.toml`
with default features. It is built from the sealed archive through the offline directory registry and
runs a few cheap checks on the public API (see `src/main.rs`), printing one `ok:` line per check and
exiting 1 on failure.

State: in-memory values or a temporary directory only. No network, no process spawn, no user data. Run
only by the CI consumer lane.
