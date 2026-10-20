# rightkit-sidecar 0.3.0 registry consumer (PK06)

Standalone consumer, outside the workspace, pinning `rightkit-sidecar =0.3.0` exactly. It exercises
the bounded JSON-line framing: `encode_json_line`, `write_json_line`, `read_json_line`, and the
`FrameError::TooLarge` refusal.

State: in-memory buffers only. No network, no files, no process spawn, no user data. Run only by the
CI consumer lane.

Not covered: deadline-bounded transfers, child-process pipes, `ReadOptions`, and oversized-line
resynchronisation.
