# rightkit-ownership 0.4.0 (registry consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-ownership =0.4.0` exactly, plus
`serde_json = "1"` because the public API exchanges `serde_json::Value`. It uses public API only:

- The embedded catalog: `catalog`, `catalog_value`, and the schema and capability list it carries.
- Pure contract functions: `ownership_verdict` refusing a malformed report with
  `codes::MALFORMED_REPORT`, `ownership_cache_key` accepting well-formed digests and refusing a
  malformed set, `is_blocking_finding`, `canonical_repository_identity`, `scanner_policy_digest`.
- `Scanner::with_embedded_catalog` over a temp directory with an injected `ScanContext` (manual clock,
  unavailable checkpoint authority, in-memory trust loader): `inputs` and `scan` on a trust location that
  was not verified return the typed `RK_OWNERSHIP_INVALID_INPUTS` error JSON, and `inputs` with an absent
  external trust file (pending enrolment) returns well-formed content, catalog and policy digests for the
  temp tree.

State: one temp directory holding one small file, removed at the end. No network, no git remote, no
signing key, no protected trust file, no real user data.

Not covered: evidence creation on a committed git checkout, approval signature verification, the
admission verdict cache, the external-worker runner, and Windows ACL evaluation.
