# rightkit-desktop-tweaks 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-desktop-tweaks` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Windows-only: names the keys and services modules and services::install on Windows. On other hosts the crate exports nothing and the consumer reports a skip.

Proof row: Windows module surface. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
