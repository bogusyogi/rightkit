# rightkit-evidence-application 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-evidence-application` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Configuration refusal for non-versioned JSON and inline-source refusal, plus the catalog value types. No application is built.

Proof row: configuration refusal and catalog types. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
