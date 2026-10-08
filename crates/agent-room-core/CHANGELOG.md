# Changelog

## 0.2.0 — unpublished

- Unauthenticated joins can no longer claim human seats. Human seats require a one-time,
  seat-bound bootstrap token minted with `AuthRegistry::mint_bootstrap_token`.
- Add `JoinRequest::bootstrap_token` (optional). A token is consumed only after its seat and
  identity check pass, and claims exactly its seat once. Minting again replaces the outstanding token.
- Agent seats still claim unauthenticated, as before.
- Note: `JoinRequest` gained a public field; struct-literal callers must add `bootstrap_token: None`.
