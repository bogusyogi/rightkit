# rightkit-code-audit-qualification 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-code-audit-qualification` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Digest form and safe relative path rules of the assurance record. No record, key or attestation is read.

Proof row: digest and path rules. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
