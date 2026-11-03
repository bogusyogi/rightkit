# rightkit-signal 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-signal` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Only validation paths run, so no Darwin notification, named event or watcher thread is created and
nothing is posted: name validation (accepted forms, empty, space, slash, non-ASCII, over-long),
`post`, `Watcher::start` and `LeaseMonitor::start` refusing bad input before touching the platform,
the watched-name limit and error text.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
