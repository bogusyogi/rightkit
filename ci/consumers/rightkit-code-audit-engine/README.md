# rightkit-code-audit-engine 0.2.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-code-audit-engine` by exact registry pin
(`=0.2.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Empty-plan topological order, canonical bytes and sha256 digest of JSON values. No provider, worktree, browser or process is used.

Proof row: ordering and digest helpers. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
