# rightkit-dictation-runtime registry consumer

Standalone consumer pinning `rightkit-dictation-runtime` to the exact version in its `Cargo.toml`.
Through the public API only, it proves: the speech-service host role differs between the headless
and shell engines, an unknown role name is refused, and the crash identity can be configured
once.

State: in-memory values only. No engine process, no model, no socket. Run only by the CI consumer
lane.
