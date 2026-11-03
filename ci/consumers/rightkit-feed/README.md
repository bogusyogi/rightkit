# rightkit-feed 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-feed` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Parses in-memory RSS 2.0, Atom and RSS 1.0 documents: entity decoding, RFC 822 and RFC 3339 dates to
Unix seconds, non-http links dropped, item cap truncation, and the empty, not-a-feed, too-large and
too-deep refusals. No network, file or process.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
