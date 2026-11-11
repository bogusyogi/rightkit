# rightkit-launcher-win 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-launcher-win` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Windows-only: the host's own launcher rows (an application page, a usage row, a cleanup summary) are built from the public types, the type-ahead holds nothing while no capture is open, and `host::install`, `panel::start` and `type_ahead::install` resolve. Nothing is called that opens a window, installs a hook or presses a key. On other hosts the crate exports nothing and the consumer reports a skip.

Proof row: Windows panel host surface. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
