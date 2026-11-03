# rightkit-app-lock 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-app-lock` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Cross-platform lock preference and state rules. The OS verification (Hello, LocalAuthentication) is never called.

Proof row: lock state rules. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
