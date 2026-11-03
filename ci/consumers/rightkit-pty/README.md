# rightkit-pty 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-pty` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

No pseudo-terminal is opened and no program is started (the crate documents terminal behaviour as
tested end to end against an installed app). Exercises sizes, the command builder chain, the empty-
argv refusal, and `spawn` only into the two refusals that happen before any terminal or process
exists: an invalid size and no tokio runtime.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
