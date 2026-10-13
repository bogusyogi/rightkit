
## Opus BLOCK fixes (lane B fixer, 2026-10-13)
- remote_conformance.rs: DAEMON_SRC now includes src/remote/core.rs and src/remote/http.rs; schema read from crate-embedded copy.
- Cargo.toml: dev-dependency rightkit-contracts with `schema` feature; `schemas/**` in include; crates/rightkit-service/schemas/daemon-api.v1.schema.json added with drift test embedded_schema_equals_repository_contract.
- Cargo.lock: rightkit-service deps in cargo order (rightkit-framed-sidecar, rightkit-fs, ..., ring).
- rightkit-daemon -> rightkit-service (`remote`) in ownership catalog (3 copies), RUNTIME-BUDGETS.md, PLATFORM-MATRIX.md.
- docs/ADOPTION.md left untouched: its rightkit-daemon rows are scans of consumer apps' vendored copies (heardright/postright/viewright), not stale references; file has another chat's uncommitted edits.
- Not run: cargo (CI-only); rustfmt --check passed on the test file.
