# Changelog

## 0.3.1 — unpublished

- Replace `rand` with `getrandom` 0.3 (the workspace standard) for random identifier bytes in `auth` and the `room` bin. Output format is unchanged (lowercase hex).

## 0.3.0 — unpublished

- Add the `http` Cargo feature (default on). It gates the `server` module and the `room` bin, which
  own the axum, tokio and tokio-stream dependencies. `default-features = false` gives a library with
  no HTTP stack; the default build is unchanged.

- Breaking: unauthenticated joins are observer-only. A seat whose role is in `auth::AUTHORITY_ROLES`
  (`human`, `moderator`, `implementer`) requires a host-minted, seat-bound bootstrap token. Without one,
  the join fails with `seat_unavailable`. Observer-class roles still claim without a token.
- Add `auth::AUTHORITY_ROLES` and `ServerState::mint_bootstrap_token(seat_id)`. The room host mints a
  token for each authority seat and threads it into that seat's join, so agent moderators and
  implementers still receive their seat credentials at spawn (`bin/room`).
- Update `JoinRequest::bootstrap_token` docs: any seat accepts a token; authority seats require one.

## 0.2.0 — unpublished

- Unauthenticated joins can no longer claim human seats. Human seats require a one-time,
  seat-bound bootstrap token minted with `AuthRegistry::mint_bootstrap_token`.
- Add `JoinRequest::bootstrap_token` (optional). A token is consumed only after its seat and
  identity check pass, and claims exactly its seat once. Minting again replaces the outstanding token.
- Agent seats still claim unauthenticated, as before.
- Note: `JoinRequest` gained a public field; struct-literal callers must add `bootstrap_token: None`.
