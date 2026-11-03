# rightkit-now-playing 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-now-playing` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Never registers a native session, because that would attach to the real OS media controls. Exercises
the file URL helper (percent-encoding, Windows drive paths), config, track, artwork, state and
command types, and the registration refusals that happen before any OS call (web view source on
every host, missing window handle on Windows).

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
