# rightkit-decimal 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-decimal` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Pure string arithmetic: canonical form, negative zero, exact comparison beyond float precision,
operator names, ordering, and every refusal (empty, bad character, malformed, too long, left and
right operand errors).

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
