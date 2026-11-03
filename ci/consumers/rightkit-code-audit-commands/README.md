# rightkit-code-audit-commands 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-code-audit-commands` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Install share-directory declaration (first call wins) and CommandError exit codes. Audit, coverage and rules handlers read repositories and are not run.

Proof row: declaration and error codes. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
