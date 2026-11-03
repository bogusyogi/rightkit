# rightkit-theme 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-theme` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Resolves in-memory brand files: parsing and refusals, suite defaults per mode, accent and dark-brand
resolution including lifting a low-contrast brand to readable contrast, token overrides, flat mode
tokens, the W3C design-token document (shape, determinism, motion scale, wordmark font) and the
colour helpers. No file, network or process.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
