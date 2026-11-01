# rightkit-dictation-telemetry registry consumer

Standalone consumer pinning `rightkit-dictation-telemetry` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: submission ids are fresh UUID-shaped values,
diagnostics upload headers carry the configured prefix, and an empty feedback message is refused
before any network call.

State: in-memory values only. No network, no files. Run only by the CI consumer lane.
