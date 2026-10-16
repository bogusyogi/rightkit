# rightkit-oauth

Shared OAuth 2.0 sign-in for Right Suite apps. It owns loopback S256 PKCE for desktop apps, the RFC 8628 device flow, provider presets, OS credential storage for desktop tokens, refreshing credential resolution, and an opt-in `server` mode for daemons and web callbacks. Server mode adds sealed (encrypted) file token storage, pending-state stores, and OIDC validation hooks.

**Use it** for sign-in and token refresh in a Right Suite app or daemon, when the provider fits one of the descriptors or can be described by an `OAuthConfig`.

**Do not use it** as an identity provider, to verify who a provider account belongs to (the host must do that), or to hold secrets outside the store it provides. Desktop sign-in binds a loopback callback listener on `127.0.0.1` (`src/lib.rs:370`); the crate starts no other server, opens no browser, and schedules no background work.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `secrets` | on | `rightkit-credentials` | Desktop token storage in the OS credential store. |
| `system-sources` | on | `rightkit-child-process` | Reads credentials from system sources (`src/system.rs`). |
| `server` | off | `ring`, `url`, `zeroize`, `serde`, `libc` (`Cargo.toml:22`) | Server mode: `rightkit_oauth::server` (begin/complete/refresh, sealed file store, OIDC hooks). |
| `google` | off | `secrets`, `ring` | Sign in with Google: `rightkit_oauth::google` (`GoogleIdVerifier`, `GoogleSession`). |
| `server-example` | off | `server`, `axum`, `tokio` | Builds `examples/server_axum.rs` (required feature for that example). |

`rightkit-http` is always a dependency. It is used for every token request.

## Durable server-mode pieces (PostRight engine)

All under `server`; `KeychainTokenStore` and `FilePendingStateStore::with_keychain_key` also need `secrets` (default on): do not set `default-features = false` without adding `secrets` if you use them. Both return `ServerAuthError::StoreNotDurable` when the credential store is guarded into memory or deny mode (`Store::is_durable()` is false), rather than falling back to memory.

- `KeychainTokenStore::new(credentials_store, lock_dir)`: `TokenStore` in the OS credential store, macOS and Windows. `lock_dir` holds only empty lock files (per-user: 0700 on Unix, under `%LOCALAPPDATA%` on Windows). The Windows credential blob limit is 2560 bytes.
- `FilePendingStateStore::new(dir, key)` or `::with_keychain_key(dir, &credentials_store, "pending-key")`: pending PKCE state shared across processes; single use, expiry enforced by the flow, sealed at rest.
- `flow.refresh_token("x", &Secret::new(refresh))` returns a `TokenSet` whose `refresh_token` is the one to keep (rotated or unchanged). `flow.refresh_token_and_store(&store, account, "x", &refresh)` also persists it atomically; `flow.refresh(&store, ..)` does the same from the stored set.
- Presets: `ServerProvider::mastodon("https://instance", id, secret, redirect)`, `x_confidential(id, secret, redirect)`, `x_public(id, redirect)`; `register_mastodon_app(&http, instance, name, &[redirect], &scopes, website)` for per-instance credentials.

```rust,ignore
let creds = rightkit_credentials::Store::open(rightkit_credentials::Config::new(ns));
let tokens = KeychainTokenStore::new(creds.clone(), data_dir.join("locks"))?;
let pending = Arc::new(FilePendingStateStore::with_keychain_key(data_dir.join("pending"), &creds, "pending-key")?);
let flow = ServerAuthFlow::new(http, [ServerProvider::x_public(client_id, redirect)], pending, Duration::from_secs(600))?;
let start = flow.begin("x", "me", &[])?;            // process 1
let done = flow.complete(callback)?;                // process 2, same directories
tokens.put("me", "x", &done.tokens)?;
let fresh = flow.refresh(&tokens, "me", "x", Duration::from_secs(120))?;
```

Redirect URIs must still be `https` (`ServerProvider::validate`); loopback `http` and Mastodon's `urn:ietf:wg:oauth:2.0:oob` are not accepted.

## PKCE, loopback listener and gateway exchange

- `pkce`: `PkceVerifier::generate`, `challenge_s256`, `generate_state`, `constant_time_eq`. `S256` only.
- `callback::CallbackListener::bind(state, CallbackOptions)` then `.wait(timeout, cancel)`: serves `127.0.0.1` and/or `[::1]`, ignores favicon, preflight and wrong-state requests, returns the code once.
- `gateway::GatewayExchange::new(https_url)?.with_field("resource", ..).exchange(&http, code, verifier, cancel)` posts JSON `{code, code_verifier, ..}` and returns a `TokenSet`.

## Platform support

- Desktop token storage uses `rightkit-credentials`. That crate has a macOS and a Windows backend. On Linux it fails closed, so the `secrets` feature cannot store tokens there.
- `SealedFileTokenStore` supports Unix local filesystems with reliable advisory locks and atomic rename. On Windows, construction fails closed; supply an ACL or credential-store adapter there.
- The `server` code uses `cfg(unix)` in `src/server/store.rs` for file permission checks.

Published version: see `INDEX.md` at the repository root. Release notes: see `CHANGELOG.md` in this crate.

## Example

The server-mode flow below is illustrative. The compiled example is `examples/server_axum.rs`, which needs the `server-example` feature.

## Sign in with Google (feature `google`)

`OAuthConfig::google_signin(client_id)` asks for `openid`, `email` and
`https://www.googleapis.com/auth/drive.appdata` with offline access and consent. Client id (and
the client secret, if Google requires one at the token endpoint for the client type) come from
app config; nothing is embedded.

```rust,ignore
use std::{sync::Arc, time::Duration};
use rightkit_oauth::{google::{GoogleIdVerifier, GoogleSession}, credentials::SecretTokenStore,
    OAuthClient, OAuthConfig};

let http = rightkit_http::Client::new(rightkit_http::ClientConfig::default());
let oauth = OAuthClient::new(OAuthConfig::google_signin(client_id).with_client_secret(secret), http.clone())?;
let verifier = GoogleIdVerifier::new(http, [client_id])?;
let store = Arc::new(SecretTokenStore::new(credentials_store)); // OS credential store
let session = GoogleSession::new(oauth, verifier, store, "google");
let who = session.sign_in_loopback(|url| open_in_browser(url), Duration::from_secs(300))?;
// who.sub is Google's stable account id: key accounts on it, never on email.
```

What `sign_in_loopback` enforces before it stores anything:

- PKCE `S256`, a random `state`, and a random OpenID Connect `nonce`; the redirect is
  `http://127.0.0.1:<ephemeral port>/oauth/callback` (Google's loopback rule for desktop clients).
- The granted `scope` includes `drive.appdata` (users can untick scopes on the consent screen),
  there is an ID token and a refresh token.
- The ID token is RS256-verified against Google's JWKS (`GOOGLE_JWKS_URL`, from the discovery
  document; cached for an hour, one rate-limited refetch on an unknown `kid`), then `iss` is
  `https://accounts.google.com` or `accounts.google.com`, `aud` is one of the configured client
  ids (and `azp` too when `aud` has several), `exp` has not passed and `iat`/`nbf` are not in the
  future (60 s skew), `sub` is present, and `nonce` equals the one sent. `alg` must be `RS256`.
- Tokens, the refresh token included, are saved in the `TokenStore`; with `SecretTokenStore` that
  is the OS credential store. Refreshes go through `credentials::Resolver`, so they take the
  cross-process lock and a revoked grant (`invalid_grant`) removes the stored sign-in. No token,
  ID token or email is written to `Debug` or error text.

`sign_out` deletes the local sign-in only; the grant stays on the Google account until the user
revokes it at <https://myaccount.google.com/permissions> (token revocation is not implemented).
The stored ID token is read back by `identity()` with issuer, audience and subject checks only
(no signature or expiry, because it was verified at sign-in and is hours old by design).

### Feeding rightkit-sync's Drive backend

`rightkit-sync` takes a `DriveTokenSource` and this crate does not depend on it; the host app
writes the few lines that join them:

```rust,ignore
use std::sync::Arc;
use rightkit_oauth::{google::GoogleSession, OAuthError};
use rightkit_sync::{drive::{account_scope, DriveTokenSource}, SyncError, SyncResult};

struct GoogleDriveTokens(Arc<GoogleSession>);

impl DriveTokenSource for GoogleDriveTokens {
    fn access_token(&self) -> SyncResult<Option<String>> {
        match self.0.access_token() {
            Ok(token) => Ok(token.map(|t| t.expose().to_string())), // None = signed out
            Err(OAuthError::Http(e)) => Err(SyncError::Transient(e.to_string())),
            Err(e) => Err(SyncError::Unavailable(e.to_string())),
        }
    }
    fn rejected(&self, token: &str) {
        let _ = self.0.rejected(token); // 401: the next access_token() refreshes
    }
    fn account_scope(&self) -> Option<String> {
        let id = self.0.identity().ok()??;
        Some(account_scope("heardright", "main", &id.sub))
    }
    fn generation(&self) -> u64 {
        self.0.generation()
    }
}
```

Android: Credential Manager returns an ID token only (no refresh token, no Drive access token).
Request it for the app's web client id with a nonce you generate, add that web client id to
`GoogleIdVerifier::new`, and check it with `verifier.verify(id_token, Some(nonce))`. Drive
authorization on Android comes from Google's `AuthorizationClient`; the host hands that access
token to its own `DriveTokenSource`. The JNI bridge for both is not in this crate or in
`rightkit-android` yet.

## Server mode

```toml
[dependencies]
rightkit-oauth = { version = "0.3.1", default-features = false, features = ["server"] }
rightkit-http = "0.3.0"
```

Daemons can omit `secrets` and `system-sources` (`default-features = false`): Linux has no `rightkit-credentials` backend.
All methods are synchronous; call on a blocking executor in an async HTTP route.

Illustrative; not compiled as a doctest. The example at `examples/server_axum.rs` is the compiled reference (`cargo run --example server_axum --features server-example`, through `rightkit cargo`).

```rust,ignore
use std::{sync::Arc, time::Duration};
use rightkit_http::{Client, ClientConfig};
use rightkit_oauth::server::{CallbackQuery, MemoryPendingStateStore, SealedFileTokenStore,
    ServerAuthFlow, ServerProvider, TokenStore};

# fn demo(client_secret: String, encryption_key: [u8; 32], query: CallbackQuery)
# -> Result<(), Box<dyn std::error::Error>> {
let flow = ServerAuthFlow::new(
    Client::new(ClientConfig::default()),
    [ServerProvider::linkedin("registered-client-id", client_secret,
        "https://app.example.invalid/oauth/callback")],
    Arc::new(MemoryPendingStateStore::default()),
    Duration::from_secs(300),
)?;
// Run only for an authenticated user; account_hint identifies their internal account.
let begin = flow.begin("linkedin", "tenant:account", &["w_member_social".into()])?;
// Host redirects user's browser to begin.authorize_url.
// Host decodes callback query once, rejects duplicate fields & calls complete.
let complete = flow.complete_bound(query, "linkedin", "tenant:account")?;
let store = SealedFileTokenStore::new("/var/lib/my-app/oauth", encryption_key)?;
store.put(&complete.account_hint, &complete.provider, &complete.tokens)?;
// Scheduled jobs call before expiry. Fresh tokens return without an exchange.
let usable = flow.refresh(&store, "tenant:account", "linkedin", Duration::from_secs(120))?;
# let _ = (begin, usable);
# Ok(()) }
```

`server::TokenStore` keys by account **plus provider**; desktop's root `TokenStore`
keeps its original API. `CompletedAuthorization` contains provider, account_hint &
existing `TokenSet`; account_hint is host-selected context, not a verified provider identity.
Resolve platform identity separately before attaching tokens to a publishing account.

### State & OIDC

`begin(provider, account_hint, scopes)` returns `{ authorize_url, state }` & stores
256-bit random state server-side. Scopes replace descriptor defaults; empty uses defaults.
PKCE uses CSPRNG/base64url primitives, 64-character verifiers & S256 challenges
where descriptor enables it. Public clients must enable PKCE. Nonce is independent random state for `openid` scopes.

`complete(CallbackQuery { code, state, error })` atomically takes pending state, compares
with a constant-time check (`ct_eq`, HMAC tags compared by `ring::hmac::verify`, `src/server/mod.rs:22-25`), checks expiry & provider binding, then exchanges code.
State is single-use even on denial, malformed callback or exchange failure. Errors include
`StateMismatch`, `StateExpired`, `ProviderError`, `ExchangeFailure`, `OidcValidation` &
`ReauthRequired`; arbitrary provider text/HTTP bodies never enter errors or Debug output.
Token requests disable retries, including rotating refresh grants. Flow rejects HTTP clients
with redirects enabled, zero request deadlines or absent/zero connect deadlines
(`connect_timeout` must be `Some(non-zero Duration)`). Redirect responses fail before parsing tokens.

Host must bind callbacks to authenticated browser context established by `begin`, as required
by [RFC 9700](https://www.rfc-editor.org/rfc/rfc9700.html#section-4.7.1). Prefer
`complete_bound(query, expected_provider, expected_account)`: mismatched account/route consumes
state & fails before exchange. Expectations come from authenticated session middleware &
provider-specific callback routes, never callback parameters. For multiple providers, use
distinct registered callback URLs with route-derived provider context (or independently verify
issuer according to RFC 9700). Raw `complete(query)` requires equivalent host checks first.

`PendingStateStore` is pluggable. Durable implementations must atomically insert-if-absent
& take across instances, encrypt state/verifiers/nonces, bound capacity & purge expired
records. `PendingAuthorization` supports serde for adapters. In-memory reference defaults
to 1024 entries; it purges expiry on insertion. Already-purged state reports mismatch;
retained expired state reports expiry. Flow enforces TTL in `(0, 3600s]`.
In-memory state is lost on restart; durable pending storage is required for restart continuity.

OIDC fails closed unless `with_oidc_validator` installs an `OidcValidator`. Host validator
must check signature, issuer, audience/azp, expiry & expected nonce against trusted JWKS.
Compare nonce in constant time.
ID tokens are never accepted through decode-only JWT checks. Server mode clears
`TokenSet::id_token` (added in 0.6.0); the ID token is passed only to the validator, not retained in token storage.

### Persistence & refresh

`SealedFileTokenStore` supports Unix local filesystems with reliable advisory locks & atomic
rename. AES-256-GCM seals each full token set using a fresh 96-bit nonce & authenticated
account/provider binding. Root is 0700; lock/ciphertext files are 0600 & must belong to
current effective UID. Privacy is rechecked before operations. Descriptor metadata, no-follow
opens & rejection of non-regular/multiply-linked files prevent file-open substitution; nonblocking
opens prevent FIFO hangs. Reads refuse symlinks; atomic writes replace destination entries
without following them. Writes use private temporary files, fsync, rename & directory fsync.
Windows construction fails closed; supply an ACL/credential-store adapter there.

Nonce uniqueness uses OS CSPRNG sampling, including across restarts/processes; it is probabilistic.
Host must rotate keys well before 2^32 total encryptions across every store sharing one key,
following [NIST SP 800-38D §8.3](https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38d.pdf).

Provision root beneath a trusted host-controlled parent. Supply a random 32-byte key from
a secret manager or systemd credential; retain it across restarts & separate it from
ciphertext/backups. Store never generates or saves keys. Secure backup, key rotation &
distributed database integration belong to host. Debug redacts key & tokens; never log
serialized pending records, callback queries, authorization URLs or token HTTP requests.
Disable callback query logging in reverse proxy as well.

OAuth-owned random buffers, pending state/verifiers/nonces, callback fields, authorization URLs,
Basic-auth intermediates, request/response buffers, parsed JSON strings & persistence plaintext
are wiped when released in server builds. `PendingAuthorization`, `CallbackQuery` &
`ServerAuthorization` implement `Drop`; borrow fields or use `std::mem::take` to extract ownership.
Wiping covers owned live allocations; allocator history, serde parser scratch/failed partial
parses, `rightkit-http` transport copies & opaque `Secret` token/client-secret allocations,
validator's ID-token `Secret`, and ring's expanded key are outside this guarantee. Caller retains
its own copy of by-value `[u8; 32]` key input & must wipe it. Disable core dumps & transport
trace logging; protect daemon memory. Full process-memory erasure is not claimed.

Every store exposes `lock` returning `TokenStoreGuard`. `get/put/delete` use that guard;
refresh holds it through read, exchange & atomic replacement. Separate instances/processes
on same local filesystem coordinate; remote/NFS & multi-host deployment need a fenced
distributed adapter. Reference lock wait defaults to 65s & can be bounded differently.
Provider request deadline remains configured by `rightkit-http`.

Refresh retains previous refresh token when provider omits one, replaces rotated grants,
& returns `ReauthRequired` for absent refresh grants or provider `invalid_grant` variants.
Rejected grants are deleted; transient exchange failures retain stored tokens.
`RefreshPersistence { tokens: Box<TokenSet> }` returns rotated tokens when saving fails
without inflating every error result. Pattern matching is unchanged; dereference `*tokens` to
recover owned `TokenSet`, or borrow `&tokens` for persistence: **stop refresh jobs
for this account & securely persist returned tokens before resuming**. Never retry old grant.
Crash between provider rotation & persistence cannot be made transactional with a remote
provider; host should require reauthorization if no recovery token remains.
Missing access-token expiry follows existing `TokenSet` semantics: no proactive refresh
deadline is inferred. Host must handle downstream authorization rejection.

## Provider descriptors

| Descriptor | Client auth | Web PKCE | Notes |
|---|---|---|---|
| `ServerProvider::new` | secret_post default; configurable | S256 default | Wraps existing `OAuthConfig`; can enable OIDC or public-client auth |
| `meta` | secret_post | disabled | Facebook Login / Instagram Graph through Facebook; explicit Graph version required |
| `linkedin` | secret_post | disabled | Web confidential flow; OIDC opt-in via `openid` scopes & validator |
| `tiktok` | secret_post | disabled | `client_key`, comma-separated scopes |
| `pinterest` | secret_basic | disabled | Comma-separated scopes |

Registered public HTTPS redirect must match exactly. Descriptor stores it independently
of desktop loopback redirect. Protected generated authorization parameters cannot be overridden.

Official references: [LinkedIn web flow](https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow),
[LinkedIn native PKCE](https://learn.microsoft.com/en-us/linkedin/shared/authentication/authorization-code-flow-native),
[TikTok web](https://developers.tiktok.com/docs/en/login-kit-web),
[TikTok token management](https://developers.tiktok.com/docs/en/oauth-user-access-token-management),
[Pinterest authorization](https://developers.pinterest.com/docs/getting-started/set-up-authentication-and-authorization/).
These web guides do not establish server PKCE support; native-only PKCE is not substituted.
Meta's [manual flow](https://developers.facebook.com/docs/facebook-login/guides/advanced/manual-flow/)
returned HTTP 429 during verification: endpoint templates, POST client auth, scope delimiter
& absent web PKCE are explicitly marked **unverified** in source. Confirm before deployment.
Meta long-lived user/page-token exchanges & Instagram Login are separate platform operations,
not standard refresh-token grants. LinkedIn programmatic refresh requires partner eligibility.

## HTTP integration & validation

`examples/server_axum.rs` builds an Axum callback router using `spawn_blocking` & persists
completed tokens. Host middleware must supply authenticated `CallbackBinding` extension;
missing binding rejects callback before exchange. `server-example` enables its optional dependencies; Host supplies listener, TLS, authenticated begin route,
tenant binding, callback rate limits & safe token-persistence recovery.

From RightKit root, with managed broker available:

```sh
rightkit cargo test -p rightkit-oauth
rightkit cargo test -p rightkit-oauth --no-default-features
rightkit cargo test -p rightkit-oauth --all-features
rightkit cargo clippy -p rightkit-oauth --all-targets --all-features -- -D warnings
rightkit cargo clippy -p rightkit-oauth --all-targets -- -D warnings
rightkit cargo fmt -p rightkit-oauth --check
rightkit cargo package -p rightkit-oauth --no-verify --allow-dirty --list
```

Server tests cover state lifecycle, PKCE exchange, nonce validation, auth variants, encrypted
store round-trip/tamper/isolation, proactive refresh rotation, concurrent refresh & redaction.
