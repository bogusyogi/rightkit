# rightkit-evidence-commands 0.2.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-evidence-commands` by exact registry pin
(`=0.2.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Extended-length path display rule and CommandError exit codes. Handlers needing an installed host are not called.

Proof row: path display and error codes. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
