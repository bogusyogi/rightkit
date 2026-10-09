# rightkit-ownership

The single Rust implementation of RightKit's ownership scanner, verdict and admission. It replaces the Node scanner in `packages/release` (`ownership-scan-worker.mjs` and the modules it imports) and is linked as a library by the RustRight broker.

Catalog capability: `ownership-scanner`. Version 0.1.0, not yet staged for publication (see [INDEX.md](../../INDEX.md)).

## What it owns

- `Scanner` and `NativeScanRunner`: the in-process scan worker. `Scanner::inputs` is the inputs probe (`--inputs`); `Scanner::scan` is the protected scan. Both return the worker's JSON line as a `serde_json::Value`, either the result object or `{error, code}`.
- `OwnershipAdmission`, `AdmissionConfig`, `Mode`, `Target`, `Verdict`: report and enforce modes, the verdict cache (128 entries, FIFO, keyed by the six bindings plus the UTC day), single-flight scans, and `assert_admitted`. `AdmissionConfig::new` wires the external-worker runner; assemble the struct directly to use `NativeScanRunner`.
- `ownership_verdict`, `ownership_cache_key`, `classify_ownership_report`: pure functions of the contract.
- `canonical_repository_identity`: WHATWG parsing through the `url` crate, with the message `invalid repository identity` for failures.
- `WorkerLimits`, `Kind`: a global cap plus one probe permit and one scan permit per repository, so a follower's probe never queues behind the leader's scan.
- `ScanContext`: the host's clock, host string, trust loader, checkpoint environment and scanner policy digest. `trust::load_trusted_external`, `trust::production_trust_loader` and `trust::production_authority_env` give the production values.

## How scanning works

Git is read by shelling out to the `git` executable, as the Node scanner does. Protected reads run with `GIT_*` variables dropped, `core.hooksPath` nulled, `-c safe.directory=<repo>`, bounded timeouts and output caps, and chunked `cat-file` reads. Windows children are launched with `CREATE_NO_WINDOW`.

Evidence is built from the committed tree (`HEAD`) of a clean checkout. A pending-enrolment advisory scan with no pinned key log reads the working tree instead, as Node does.

Approval signatures are verified with RustCrypto: P-256 ECDSA/SHA-256 (`p256`) and RSA-2048 PSS or PKCS#1 v1.5 (`rsa`). Nothing hand-rolls cryptography.

## Known differences from the Node scanner

- The cache key carries the UTC day (CONTRACT DEFECT-1). A cached verdict never outlives the day it was computed.
- Catalog source patterns run on `fancy-regex`. `\w`, `\d` and `\b` are Unicode-aware, where V8 is ASCII-only.
- Path ordering follows ICU root collation exactly for ASCII and approximately beyond it.
- JSON and TOML parser error text differs from V8 and smol-toml.
- `scannerPolicyDigest` hashes this crate's own source files, not the Node module closure.

## Tests

`cargo test` runs the unit tests and `tests/ownership_fixtures.rs`, which reads every case in `packages/release/fixtures/ownership/cases` in place (no vendored copy) and reports a pass count per group.

## Windows

The Windows ACL audit, `CREATE_NO_WINDOW` spawning and Git for Windows path handling are written but have only been exercised on macOS.
