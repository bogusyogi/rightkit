# rightkit-dictation-telemetry 0.2.3 registry consumer

Standalone consumer pinning `rightkit-dictation-telemetry =0.2.3` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: submission ids are fresh UUID-shaped values,
diagnostics upload headers carry the configured prefix, and an empty feedback message is refused
before any network call.

An oversized archive is refused and cleaned up before sending. State is one temporary file. No network. Run only by the CI consumer lane.
