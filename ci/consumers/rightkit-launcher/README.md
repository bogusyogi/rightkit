# rightkit-launcher 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-launcher` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Calculator evaluation and formatting and the response excerpt. The Windows panel, chat, store and spot-takeover modules are not used.

Proof row: calculator and excerpt. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
