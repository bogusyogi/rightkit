# rightkit-dictation-engine registry consumer

Standalone consumer pinning `rightkit-dictation-engine` to the exact version in its `Cargo.toml`.
Through the public API only, it proves the facade re-exports the file transcription errors and
the product vocabulary of the crates it fronts.

State: in-memory values only. No engine process, no model. Run only by the CI consumer lane.
