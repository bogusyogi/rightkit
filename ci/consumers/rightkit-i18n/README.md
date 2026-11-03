# rightkit-i18n 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-i18n` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Loads in-memory JSON catalogs, resolves locale tags (exact id, alias, longest prefix, registered
tag), refuses invalid JSON and non-objects, checks the lookup order (locale, English, caller
fallback, key) including dotted paths and literal dotted keys, and merges catalogs recursively.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
