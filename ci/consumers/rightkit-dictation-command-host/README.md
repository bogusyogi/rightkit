# rightkit-dictation-command-host registry consumer

Standalone consumer pinning `rightkit-dictation-command-host` to the exact version in its
`Cargo.toml`. Through the public API only, it proves: no process is the host's own until the host
identity is configured, and after configuring it the host prefix matches case-insensitively while
other processes do not.

State: in-memory values only. No input injection, no screen capture, no app launch. Run only by
the CI consumer lane.
