# rightkit-presence 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-presence` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Classifies synthetic window observations (quiet, full-screen meeting, large meeting window, ignored
dock, own, non-zero-layer and boundless windows), applies the preference gates, layers manual and
automatic Do-Not-Disturb, checks the injected-observation hold against live samples and the
camelCase wire forms. The polling loop is never started; the one live call, `observe_os`, is a read-
only probe whose answer is not constrained.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
