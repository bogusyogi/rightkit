# rightkit-text-polish 0.1.2 registry consumer (`store` feature)

Standalone consumer, outside the workspace, pinning `rightkit-text-polish` to the exact version in
its `Cargo.toml`. Through the public API only, it proves: deterministic polish capitalizes and
normalises a spoken number, a host-supplied brand homophone table repairs a guarded phrase and
spares its literal reading, and plain polish applies no brand repair.

The lazy persisted-store host is constructed and its public operations are type-checked; it is
never loaded, and host path/write callbacks are never called. No network, files or process spawn.
Run only by the CI consumer lane.
