# rightkit-crash 0.3.4 registry consumer (PK11)

Standalone consumer, outside the workspace, pinning `rightkit-crash =0.3.4` exactly with default
features off. It exercises `list_pending`: only `crash_*.json` files are counted, and unrelated files
in the same directory are ignored.

State: a per-process directory under the OS temp directory, removed at the end. No network, no
process spawn, no real user data. Run only by the CI consumer lane.

Not covered: `capture_native`, the panic and process hooks, `enqueue_pending` (needs an outbox),
`prune` (it sweeps raw samples in the OS temp directory), and support bundles.
