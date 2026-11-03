# rightkit-power 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-power` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

`Power::keep_awake` and `Activity::begin` change what the system or process holds awake and are
never called. Exercises the unconstrained default conditions, the acknowledgement handle, event
debug text, and a listening-only watcher (spawn, suspend hook registration, conditions read, event
drain, suspended-at take, drop). A host that cannot start a reader gets default conditions and no
events, which is the documented behaviour.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
