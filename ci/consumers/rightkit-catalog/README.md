# rightkit-catalog 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-catalog` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Parses in-memory JSON catalogs, flattens nested keys, extracts placeholders, runs the completeness
check (missing key, placeholder drift, extra key) and generates the TypeScript, Rust and Swift
tables, checking determinism. The one directory read targets a path that cannot exist. Nothing is
written.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
