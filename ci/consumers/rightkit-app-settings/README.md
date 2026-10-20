# rightkit-app-settings 0.3.0 registry consumer (PK04)

Standalone consumer, outside the workspace, pinning `rightkit-app-settings =0.3.0` exactly. It
exercises `atomic_write`: a first publication, then a replacement, each read back byte for byte.

State: a per-process directory under the OS temp directory, removed at the end. No network, no
process spawn, no real user data. Run only by the CI consumer lane.

Not covered: `SettingsBackend`, `FileBackend`, `Document`, `SqlSettings`, secret slots, and
`atomic_write_with`.
