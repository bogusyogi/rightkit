# Changelog

## 0.2.3

- **Plain exception entries; signed exceptions retired (Orthic-Labs/rightkit#262).** `scan.rs` reads the same plain entries as `packages/release/ownership.mjs` (`{capability, package or crate, reason, owner, date}`, optional `fingerprint` and `path`), matches them the same way, marks every matching finding `excepted`, and reports an entry that matches no finding as a stale warning (`exception #N (capability/package): stale, matches no current finding (...)`). A missing field or a legacy signed record (`schema`/`payload`/`signature`) is a problem. `verify_exception`, `ExceptionContext` and the exception signature check are removed from `approval.rs`; key-log, trust-registry, floor and provenance code is unchanged.
- **Pending enrollment no longer blocks `check_app` / `check_committed`.** It was only needed for signed approvals; it is now a warning and `ok` ignores it. The report keeps `enrollmentPending: false` from these entry points.
- Tests: `exception_tests` in `scan.rs` (plain match, alias/fingerprint/path narrowing, stale warning, malformed and legacy entries, duplicate entries, enrollment warning). Not built or run here.
