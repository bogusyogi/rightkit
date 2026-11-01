# rightkit-text-polish registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-text-polish` to the exact version in
its `Cargo.toml`. Through the public API only, it proves: deterministic polish capitalizes and
normalises a spoken number, a host-supplied brand homophone table repairs a guarded phrase and
spares its literal reading, and plain polish applies no brand repair.

State: in-memory values only. No network, no files, no process spawn. Run only by the CI consumer
lane.
