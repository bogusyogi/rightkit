# Changelog

## 0.5.0 — staged, unpublished

- Breaking (0.x minor): new `McpError::RandomUnavailable` variant; exhaustive matches need one arm.
- Security: a caller-supplied empty loopback token is refused instead of accepting empty bearers.

- Security: the loopback HTTP server's default bearer token no longer ignores RNG failures.
  A failed or all-zero random fill now fails closed: `ServerHost::serve_http` returns the new
  `McpError::RandomUnavailable` and binds no listener. Previously the discarded error left an
  all-zero token that anyone on loopback could guess.
- HTTP session ids use the same fail-closed generator. If none can be generated, `initialize`
  returns HTTP 500 and issues no session.
- Generated tokens are checked to be 48 hex characters and not all zeros.
- Adds `McpError::RandomUnavailable(String)`. Exhaustive `match` on `McpError` needs one new arm.

## 0.4.0 — staged, unpublished

- Bind the stdio launch configuration by value into the approval fingerprint: environment
  variable values now count, not only names. A changed `PATH` or `NODE_OPTIONS` is `Pending`.
- Fingerprint canonical form is version 2. All 0.3.x approvals are invalid; re-approve each
  workspace server once. Only the SHA-256 digest is stored; env values never reach the file.
- `PendingApproval` still exposes env variable names only.
