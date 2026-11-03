# rightkit-notifications 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-notifications` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Never sends, replaces, closes, badges or reads back notifications, never raises a permission prompt
and never registers an activation handler. Exercises the notification builder, sound and action
types, the trimmed app id, the per-platform capability table, the permission read (which never
prompts and may answer an error on a host without a bundle) and the error text.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
