# rightkit-clipboard-history 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-clipboard-history` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Platform-neutral kind names, text detection, colour parsing and the bounded JSON reader. The store, capture and paste paths are not touched.

Proof row: detection and JSON helpers. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
