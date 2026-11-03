# rightkit-agent-skills 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-skills` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Path routing, trigger equality, positional argument substitution and quoted argument splitting. No I/O.

Proof row: routing, triggers, arguments. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
