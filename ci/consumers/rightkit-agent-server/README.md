# rightkit-agent-server 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-server` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Names the public entry `run` as a function item without calling it, because calling it starts the agent server. Zero-size check only.

Proof row: entry item. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
