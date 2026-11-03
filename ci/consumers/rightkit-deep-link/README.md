# rightkit-deep-link 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-deep-link` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Parses and queues link strings: scheme folding, percent and plus decoding including UTF-8 bytes,
fragment drop, the camelCase wire form, traversal, empty-host, control-character, unregistered-
scheme and over-long refusals, and ordered single-drain queue behaviour. `register_scheme` writes OS
state (Windows registry) and is never called.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
