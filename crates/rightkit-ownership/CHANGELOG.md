# Changelog

## 0.2.4

- **Pending approval enrollment is a reported warning everywhere (Orthic-Labs/rightkit#262).** `ownership_verdict` no longer refuses on `enrollmentPending` (`PENDING_ENROLLMENT` is never produced); with pending enrollment and no trust it skips the registry and floor checks, and `Decision` gains `warnings`, which `decision_json` emits when non-empty. `create_evidence`: `ok` ignores pending enrollment, an absent trust registry is no longer a floor failure, and registration diagnostics and stale exception warnings go to `result["warnings"]` (only when non-empty) instead of `problems`. `Scanner::scan_protected` no longer short-circuits on a pending preflight. Real finding, coverage, floor and registry gates are unchanged. Tests and the shared Node fixtures were updated (not built or run here).

## 0.2.3

- **Plain exception entries; signed exceptions retired (Orthic-Labs/rightkit#262).** `scan.rs` reads the same plain entries as `packages/release/ownership.mjs` (`{capability, package or crate, reason, owner, date}`, optional `fingerprint` and `path`), matches them the same way, marks every matching finding `excepted`, and reports an entry that matches no finding as a stale warning (`exception #N (capability/package): stale, matches no current finding (...)`). A missing field or a legacy signed record (`schema`/`payload`/`signature`) is a problem. `verify_exception`, `ExceptionContext` and the exception signature check are removed from `approval.rs`; key-log, trust-registry, floor and provenance code is unchanged.
- **Pending enrollment no longer blocks `check_app` / `check_committed`.** It was only needed for signed approvals; it is now a warning and `ok` ignores it. The report keeps `enrollmentPending: false` from these entry points.
- Tests: `exception_tests` in `scan.rs` (plain match, alias/fingerprint/path narrowing, stale warning, malformed and legacy entries, duplicate entries, enrollment warning). Not built or run here.
