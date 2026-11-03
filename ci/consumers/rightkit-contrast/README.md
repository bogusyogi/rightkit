# rightkit-contrast 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-contrast` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Pure colour arithmetic: hex parsing and refusal, WCAG relative luminance endpoints, the 21:1 maximum
ratio, symmetry, and readable ink selection including the documented yellow example.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
