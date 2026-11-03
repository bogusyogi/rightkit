# rightkit-prompt-recipes 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-prompt-recipes` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Studio command table, catalogue lookups and an unknown-intent refusal. No recipe is dispatched.

Proof row: command table and catalogue. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
