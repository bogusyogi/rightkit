# rightkit-dictation-text-pipeline registry consumer

Standalone consumer pinning `rightkit-dictation-text-pipeline` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: the product vocabulary is empty until the
host configures it, then both term lists read back.

State: in-memory values only. No network, no files, no model. Run only by the CI consumer lane.
