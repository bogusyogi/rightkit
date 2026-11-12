# rightkit-app-update 0.2.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-app-update` by exact registry pin
(`=0.2.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`. Extra pinned-by-range test dependencies: `serde_json`.

Boot probation runs its real sentinel state machine (begin, evaluate_boot, report_stage,
acknowledge_healthy, failed boot, superseded sentinel) against one JSON file in a private temporary
directory that is removed at the end. The fetch bridge is driven only into refusals that happen
before any connection (plain http, userinfo, unknown method, forbidden or injected headers, body on
GET, malformed or oversized base64), plus the fixed error wire form. No DNS lookup, network, process
or user data.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
