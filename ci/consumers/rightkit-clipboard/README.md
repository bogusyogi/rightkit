# rightkit-clipboard 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-clipboard` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Never calls `read`, `read_with`, `write`, `formats` or `platform_snapshot`: they touch the real
system clipboard. Exercises the pure helpers (named-board refusal, DIB to BMP conversion with its
refusals), the content and option types, the temp-directory name and a stale-file purge against a
directory that does not exist.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
