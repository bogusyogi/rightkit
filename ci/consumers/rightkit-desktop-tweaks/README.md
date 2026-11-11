# rightkit-desktop-tweaks 0.2.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-desktop-tweaks` by exact registry pin
(`=0.2.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Windows-only: the shortcut settings start off, the shortcut handler orders after an application handler at 0, and keys::apply and services::install resolve, on Windows (nothing installed). On other hosts the crate exports nothing and the consumer reports a skip.

Proof row: Windows shortcut-layer surface. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
