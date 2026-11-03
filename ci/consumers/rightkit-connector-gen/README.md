# rightkit-connector-gen 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-connector-gen` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Renders n8n, Zapier and Make connector source for an in-memory contract and checks file sets, the
HTTP bridge path convention, determinism and the four contract refusals (engine name, method name,
duplicate method, repeated parameter). The crate returns file contents only; nothing is written and
no network or process is used.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
