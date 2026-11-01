# rightkit-dictation-models registry consumer

Standalone consumer pinning `rightkit-dictation-models` to the exact version in its `Cargo.toml`.
Through the public API only, it proves: the store owner id and the Windows TDT model directory
constants are the published values, and the bundle identifier can be configured once.

State: in-memory values only. No network, no files, no model download. Run only by the CI
consumer lane.
