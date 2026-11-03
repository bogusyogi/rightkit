# rightkit-mail-store 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-mail-store` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Mutation-class vocabulary, the destructive-action flag name and the schema v13 script. guard, clear_account and database opens are not called.

Proof row: safety vocabulary and schema. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
