# rightkit-instance 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-instance` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Nothing is hosted and no suite root, socket, named pipe or window is created: `claim`, `claim_in`
and a valid-id `arbitrate` would make this process a real primary and are never called. Exercises
service naming and normalisation, the empty and over-long id refusals (including `arbitrate`
refusing an empty id without starting as primary), the forwarded message type and its wire form,
legacy tauri-plugin-single-instance name derivation, claim options and failure text, and the in-
process destroy hook running exactly once.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
