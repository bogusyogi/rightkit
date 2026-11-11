# rightkit-launcher 0.2.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-launcher` by exact registry pin
(`=0.2.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Calculator evaluation and formatting, the bounded JSON reader and template filling with no host. The Windows panel is `rightkit-launcher-win` since 0.2.0 and the answer layout `rightkit-overlay-win`.

Proof row: calculator, JSON and template filling. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
