# Changelog

## 0.8.0 (unpublished)

Orthic-Labs/rightkit#279: sign-in always opens the user's default browser, never an in-app webview. Not built or run locally.

- Add `browser` module: `BrowserOpener`, `SystemBrowser` (OS default handler, argv only), `SignInSurface` (default browser, Android Custom Tab, iOS `ASWebAuthenticationSession`, embedded webview), `check_authorization_url` (https, or loopback http; no whitespace/control/quote characters). Docs cover Sign in with Apple and the mobile surfaces.
- Add `OAuthClient::sign_in_with_browser` and `GoogleSession::sign_in_default_browser`.
- Add `OAuthError::EmbeddedWebviewRefused` and `OAuthError::UnsafeAuthorizationUrl` (the enum is `#[non_exhaustive]`).
- `finish_loopback` now accepts only a request on the exact redirect path (other paths get 404 and are ignored until the deadline), and compares `state` in constant time.

## 0.7.0 (unpublished)

Orthic-Labs/rightkit#194: what the PostRight engine needs for Mastodon and X OAuth with refresh. All additions are under the `server` feature; nothing existing changed except two new `ServerAuthError` variants (`RegistrationFailure`, `StoreNotDurable`) (the enum is `#[non_exhaustive]`). `rightkit-daemon` moves its pin to `0.7.0`.

- Add `server::KeychainTokenStore` (needs `server` + `secrets`, on by default): a cross-platform durable `TokenStore` over the OS credential store (macOS Keychain, Windows Credential Manager) via `rightkit-credentials`, keyed by `(account, provider)`. A file lock in a caller-chosen per-user directory serializes get/refresh/put across processes. `SealedFileTokenStore` is unchanged.
- Add `server::FilePendingStateStore`: a durable `PendingStateStore` shared by processes, so `begin` and `complete` can run in separate CLI invocations. AES-256-GCM sealed record per state, bound to its file name and expiry, under a directory lock; `take` deletes before decrypting (single use, even when corrupt); expired records are purged on insert; capacity bounded. `with_keychain_key` keeps the sealing key in the OS credential store.
- Add `ServerAuthFlow::refresh_token(provider, &Secret)`: refresh from a bare refresh token, independent of any stored `TokenSet`. The returned set carries the rotated refresh token when the provider sends one, else the one passed in.
- Add `ServerAuthFlow::refresh_token_and_store(store, account, provider, &Secret)`: the same under the store lock with an atomic replacement of the stored set (rotated token included); `RefreshPersistence` still returns the new tokens when the write fails; `invalid_grant` deletes the stored set only if it still holds the rejected token.
- Add presets `ServerProvider::mastodon(instance, ..)`, `x_confidential(..)`, `x_public(..)`, `with_default_scopes`, and `register_mastodon_app` (`POST {instance}/api/v1/apps`, JSON body) returning `RegisteredApp { client_id, client_secret, .. }` (redacted `Debug`). New `ServerAuthError::RegistrationFailure { status }`.
- `refresh_token_and_store` re-reads the stored set under the lock: if its refresh token differs from the one passed in (another process already rotated it) the stored set is returned and the stale token is never spent, so a rotated grant is not double-spent. When the provider rejects the grant, a failed read or delete no longer replaces `ReauthRequired` with `Storage`.
- Add `ServerAuthError::StoreNotDurable`. `KeychainTokenStore::new` and `FilePendingStateStore::with_keychain_key` return it when the credential store is not durable (`Store::is_durable()` is false: a tripped guard put it in memory or deny mode) instead of silently losing rotated tokens or the cross-process pending key.
- `KeychainTokenStore` and `FilePendingStateStore::with_keychain_key` need the `secrets` feature (on by default). Consumers that set `default-features = false` must add `features = ["server", "secrets"]` to use them; without `secrets` they do not exist.
- Unverified, see the preset docs: Mastodon PKCE enforcement per version, and the default scope lists.

## 0.6.0 (unpublished)

Includes everything listed as 0.5.1 below (never published) plus the Google sign-in work of Orthic-Labs/rightkit#162.

- **Breaking (0.x minor):** `TokenSet` gains `id_token: Option<Secret>`, filled from the token response (a refresh that omits it keeps the previous one), redacted in `Debug`, and written to the stored form (`"id"`; older stored values still load). Struct literals must add the field. Server mode (`server` feature) clears it, so its stored tokens are unchanged. `rightkit-daemon` moves its pin to `0.6.0`.
- Add `OAuthConfig::google_signin` and `GOOGLE_SIGNIN_SCOPES` (openid, email, drive.appdata).
- Add opt-in `google` feature (implies `secrets`): `google::GoogleIdVerifier` (RS256 ID-token check against Google's JWKS: signature, issuer, audience/azp, expiry, iat/nbf, subject, nonce) and `google::GoogleSession` (loopback sign-in with PKCE S256 and a nonce, drive.appdata and refresh-token checks before storing, refreshing `access_token`, `rejected`, `identity`, `generation`, `sign_out`). Glue to rightkit-sync's `DriveTokenSource` is in the README; no dependency between the crates.

## 0.5.1 (never published; folded into 0.6.0)

Additive: nothing existing changed; `begin_loopback` / `finish_loopback` behave as in 0.5.0.

- New `pkce` module: RFC 7636 `S256`-only helpers. `PkceVerifier::generate` (32 CSPRNG octets, 43 base64url chars), `PkceVerifier::parse` / `validate_verifier` (43 to 128 chars of `[A-Za-z0-9-._~]`), `challenge_s256`, `Pkce::generate`, `generate_state`, `base64url_encode` and `constant_time_eq`. There is no `plain` method. Verifiers redact in `Debug`.
- New `callback` module: `CallbackListener`, a tolerant single-use loopback redirect listener per RFC 8252. Binds `127.0.0.1`, `[::1]` or both on an ephemeral (or fixed) port; answers and ignores favicon, CORS preflight, non-GET, other-path, malformed, stalled and wrong-or-missing-`state` requests; checks `state` in constant time; `wait` takes a bounded timeout and an optional `CancelToken` (so a manual paste can win the race) and consumes the listener, which closes on return.
- New `gateway` module: `GatewayExchange` posts `{"code", "code_verifier", ...extra fields}` as JSON to an HTTPS gateway through `rightkit-http` and parses a strictly typed JSON token response into a `TokenSet`. Extra fields carry values such as `redirect_uri` or an RFC 8707 `resource`. The code and verifier are registered as request secrets and scrubbed from errors; tokens never appear in errors or `Debug`.


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
