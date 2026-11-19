# rightkit-dictation-engine registry consumer

Standalone consumer pinning `rightkit-dictation-engine` to the exact version in its `Cargo.toml`.
Through the public API, it checks bundled host configuration, product vocabulary,
file transcription errors, and output acceptance before and after host configuration.

State: in-memory values only. No engine process, no model. Run only by the CI consumer lane.
