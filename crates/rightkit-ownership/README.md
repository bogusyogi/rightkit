# rightkit-ownership

The single Rust implementation of RightKit's ownership scanner, verdict and admission. It replaces the Node scanner in `packages/release` (`ownership-scan-worker.mjs` and the modules it imports) and is linked as a library by the RustRight broker.

Catalog capability: `ownership-scanner`. Version 0.4.0 (see [CHANGELOG.md](CHANGELOG.md) and [INDEX.md](../../INDEX.md)).

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
- A Cargo Git dependency pinned by a full 40-hex `rev` with a third-party URL is reported on `DepsResult.covered` as `pinned-git-dependency` (`scanned: false`), not as a coverage gap. First-party orgs (github.com/Orthic-Labs, github.com/bogusyogi; the Node scanner also reads `ownership-repositories.json`, whose orgs are the same two) and unparseable URLs stay unscanned gaps.
- `scannerPolicyDigest` hashes this crate's own source files, not the Node module closure.

## Source rules

Catalog source fingerprints are single-pattern regular expressions evaluated per line or per file (`src/source.rs`). One rule is evaluated natively because it must relate a thread closure, its loop and the absence of a pool inside that loop (`src/autorelease.rs`); it runs only when the scanned catalog declares its capability.

| Rule id | Capability / owner | Tier | Fails when | Fix |
|---|---|---|---|---|
| `appkit.autorelease.thread-loop-no-pool` | `appkit-autorelease-pool` / `rightkit-ownership` | block | A macOS-scoped `.rs` file (path contains `/macos/` or `macos_`, or a non-negated `target_os = "macos"` cfg) passes a closure (or same-file fn) to `thread::spawn`, `.spawn(..)` or `.spawn_scoped(..)` whose `loop` or `while` calls AppKit, `AX*`, `CF*`, `msg_send!` or an `objc2_app_kit`, `core_foundation`, `accessibility_sys`, `objc2_foundation::NS*` path with no `autoreleasepool(..)` inside the loop body. A pool around the whole loop does not count. | Drain `objc2::rc::autoreleasepool` once per loop iteration. |

The finding names the file, the line of the spawn, the first uncovered API and the fix. Record a reviewed exception as a plain entry in `.rightkit/ownership-exceptions.json` (`capability: "appkit-autorelease-pool"`, `package: "rightkit-ownership"`, optional `fingerprint` and `path`, `reason`, `owner`, `date`). Limits: helpers in other files, trait objects, stored closures and `for` loops are not followed. Origin: Orthic-Labs/rightkit#893, heardright#292.

## Tests

`cargo test` runs the unit tests (including `src/autorelease.rs` over `tests/fixtures/appkit-autorelease/`) and `tests/ownership_fixtures.rs`, which reads every case in `packages/release/fixtures/ownership/cases` in place (no vendored copy) and reports a pass count per group.

## Windows

Windows-specific code: the ACL evaluation (`evaluate_windows_acl`, `src/trust.rs:59`, a pure function over JSON that its unit test runs on any host), `CREATE_NO_WINDOW` spawning (`src/gitio.rs:97-100`, `src/runner.rs:105-109`, both `cfg(windows)`) and verbatim-path prefix stripping (`src/paths.rs:38-47`, `cfg(windows)`). This repository records no run of this code on Windows.

## Changelog

- 0.2.1: refresh the embedded catalog from packages/release/rightkit-ownership.json (adds rightkit-fsindex and rightkit-ownership rows).
