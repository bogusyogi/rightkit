# Changelog


## 0.5.0

- Republished on the renamed dependencies: `rightkit-child-process` (was `rightkit-process`), `rightkit-credentials` (was `rightkit-secrets`). The 0.4.0 on crates.io was packaged before the renames and still named the old crates.
- **Breaking (0.x minor):** `SecretTokenStore::new` takes a `rightkit_credentials::Store`.
## 0.4.0

- **Breaking (0.x minor):** public dependencies raised: `rightkit-http` 0.3, `rightkit-secrets` 0.2 and `rightkit-process` 0.4. `OAuthError::Http(HttpError)` and `CredentialStore::new(rightkit_secrets::Store)` expose them, so callers must use matching versions.

- **Fixed:** token-endpoint and device-poll provider errors no longer repeat credentials the client submitted. The refresh token, device code, authorization code, PKCE verifier and client secret are registered as secrets on the request (`Request::secret_form`), and the provider's `error` and `error_description` are scrubbed to `[redacted]` before `OAuthError::Provider` is built, on both the non-2xx and the HTTP 200 provider-error paths. Requires the unreleased `rightkit-http` additions (`secret_form`, `scrub`).

## 0.3.1 - 2026-10-10

- Drop the `base64` and `subtle` dependencies from the `server` feature. HTTP Basic credentials use an in-crate padded standard-alphabet encoder next to the existing `base64url` helper (one shared encoder, sized so it never reallocates), and state/provider/account/binding comparisons use `ring::hmac::verify` over fixed-key HMAC-SHA256 tags (`ring` is already a `server` dependency; `ring::constant_time` is deprecated in ring 0.17, so the public HMAC verify is the constant-time comparison). `base64` stays as a dev-dependency, the independent reference for tests.
- Move `libc` to a `cfg(unix)` target dependency (still optional, still enabled by `server`); Windows builds no longer resolve it.
- No public API changes.

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
