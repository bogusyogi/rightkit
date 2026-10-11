# rightkit-ownership

The single Rust implementation of RightKit's ownership scanner, verdict and admission. It replaces the Node scanner in `packages/release` (`ownership-scan-worker.mjs` and the modules it imports) and is linked as a library by the RustRight broker.

Catalog capability: `ownership-scanner`. Version 0.2.1, published to crates.io and matching the source (see [INDEX.md](../../INDEX.md)).

## What it owns

- `Scanner` and `NativeScanRunner`: the in-process scan worker. `Scanner::inputs` is the inputs probe (`--inputs`); `Scanner::scan` is the protected scan. Both return the worker's JSON line as a `serde_json::Value`, either the result object or `{error, code}`.
- `OwnershipAdmission`, `AdmissionConfig`, `Mode`, `Target`, `Verdict`: report and enforce modes (`off` returns an `off` verdict without scanning, `src/admission.rs:33-37`, `:340-341`), the verdict cache (128 entries, FIFO, keyed by the six bindings plus the UTC day), single-flight scans, and `assert_admitted`. `AdmissionConfig::new` wires the external-worker runner; assemble the struct directly to use `NativeScanRunner`.
- `ownership_verdict`, `ownership_cache_key`, `classify_ownership_report`: pure functions of the contract.
- `canonical_repository_identity`: WHATWG parsing through the `url` crate. Failures return `Err(String)` with the parse message, for example `repository identity is required` or `repository identity must be an absolute HTTPS/SSH URL or SSH scp remote` (`src/approval.rs:24-40`). `verify_repository_registration` reports any such failure as `invalid repository identity` (`src/approval.rs:278`).
- `WorkerLimits`, `Kind`: a global cap plus one probe permit and one scan permit per repository, so a follower's probe never queues behind the leader's scan.
- `ScanContext`: the host's clock, host string, trust loader, checkpoint environment and scanner policy digest. `trust::load_trusted_external`, `trust::production_trust_loader` and `trust::production_authority_env` give the production values.

## How scanning works

Git is read by shelling out to the `git` executable, as the Node scanner does. Protected reads run with `GIT_*` variables dropped, `core.hooksPath` nulled, `-c safe.directory=<repo>`, bounded timeouts and output caps, and chunked `cat-file` reads. Windows children are launched with `CREATE_NO_WINDOW`.

Evidence is built from the committed tree (`HEAD`) of a clean checkout. A pending-enrolment advisory scan with no pinned key log (no `rootFingerprint` in trust) reads the working tree instead, as Node does (`src/evidence.rs:81`).

Approval signatures are verified with RustCrypto: P-256 ECDSA/SHA-256 (`p256`) and RSA-2048 PSS or PKCS#1 v1.5 (`rsa`). Nothing hand-rolls cryptography.

## Known differences from the Node scanner

- The cache key carries the UTC day (CONTRACT DEFECT-1). A cached verdict never outlives the day it was computed.
- Catalog source patterns run on `fancy-regex`. `\w`, `\d` and `\b` are Unicode-aware, where V8 is ASCII-only.
- Path ordering follows ICU root collation exactly for ASCII and approximately beyond it.
- JSON and TOML parse errors come from `serde_json` and the `toml` crate, so their text differs from Node's; the fixture harness normalises TOML error text (`tests/ownership_fixtures.rs`, `normalize_parser_text`).
- `scannerPolicyDigest` hashes this crate's own source files, not the Node module closure.

## Tests

`cargo test` runs the unit tests and `tests/ownership_fixtures.rs`, which reads every case in `packages/release/fixtures/ownership/cases` in place (no vendored copy) and reports a pass count per group.

## Windows

Windows-specific code: the ACL evaluation (`evaluate_windows_acl`, `src/trust.rs:59`, a pure function over JSON that its unit test runs on any host), `CREATE_NO_WINDOW` spawning (`src/gitio.rs:97-100`, `src/runner.rs:105-109`, both `cfg(windows)`) and verbatim-path prefix stripping (`src/paths.rs:38-47`, `cfg(windows)`). This repository records no run of this code on Windows.

## Changelog

- 0.2.1: refresh the embedded catalog from packages/release/rightkit-ownership.json (adds rightkit-fsindex and rightkit-ownership rows).
