# Changelog

## 0.3.0 — staged, unpublished

- Add opt-in `server` feature for framework-agnostic daemon OAuth, public HTTPS callbacks,
  S256 PKCE, bounded single-use state, OIDC validator integration & coordinated refresh.
- Add configurable provider descriptors, account/provider token-store adapters & Unix
  AES-256-GCM file persistence with authenticated binding, private files & atomic replacement.
- Add optional `server-example` Axum adapter; callback requires authenticated account/route context.
- Add `complete_bound` to reject callback account/provider mismatches before spending codes.
  Raw `complete` requires equivalent host browser-session/provider checks.
- Reject public clients without PKCE, redirect-following HTTP clients & absent/zero connect deadlines.
- Wipe OAuth-owned temporary credentials, JSON, callback & pending buffers; redact provider Debug.
  README documents opaque dependency allocations & host key/memory responsibilities.
- Reject symlink/non-regular/multiply-linked files, foreign ownership & exposed permissions;
  use no-follow/nonblocking opens & recheck root privacy.
- `ServerAuthError::RefreshPersistence.tokens` is `Box<TokenSet>` to keep errors small.
  Borrow for persistence or dereference to recover ownership; no Clippy suppression.
- Pending/callback/authorization records implement `Drop` for wiping; borrow fields or use
  `std::mem::take` when extracting ownership. Desktop API remains unchanged.
- Include server sources, Axum example, tests, README & changelog in crate package.
  Gate desktop integration tests by their features so disabled-feature builds validate cleanly.
