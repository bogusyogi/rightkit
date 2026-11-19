# rightkit-dictation-app-runtime registry consumer

Standalone consumer pinning `rightkit-dictation-app-runtime` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: the engine binary name defaults to the
RightKit engine, follows the configured stem with the platform extension, and is configured once.
It also checks legacy session resets and resets carrying the typed previous-delivery command.

State: in-memory values only. No engine process is spawned. Run only by the CI consumer lane.
