# rightkit-oauth

Shared OAuth 2.0 sign-in for Right Suite apps. It owns loopback S256 PKCE for desktop apps, the RFC 8628 device flow, provider presets, OS credential storage for desktop tokens, refreshing credential resolution, and an opt-in `server` mode for daemons and web callbacks. Server mode adds sealed (encrypted) file token storage, pending-state stores, and OIDC validation hooks.

**Use it** for sign-in and token refresh in a Right Suite app or daemon, when the provider fits one of the descriptors or can be described by an `OAuthConfig`.

**Do not use it** as an identity provider, to verify who a provider account belongs to (the host must do that), or to hold secrets outside the store it provides. The crate starts no server, opens no browser, and schedules no background work.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `secrets` | on | `rightkit-secrets` | Desktop token storage in the OS credential store. |
| `system-sources` | on | `rightkit-process` | Reads credentials from system sources (`src/system.rs`). |
| `server` | off | `base64`, `ring`, `subtle`, `url`, `zeroize`, `serde`, `libc` | Server mode: `rightkit_oauth::server` (begin/complete/refresh, sealed file store, OIDC hooks). |
| `server-example` | off | `server`, `axum`, `tokio` | Builds `examples/server_axum.rs` (required feature for that example). |

`rightkit-http` is always a dependency. It is used for every token request.

## Platform support

- Desktop token storage uses `rightkit-secrets`. That crate has a macOS and a Windows backend. On Linux it fails closed, so the `secrets` feature cannot store tokens there.
- `SealedFileTokenStore` supports Unix local filesystems with reliable advisory locks and atomic rename. On Windows, construction fails closed; supply an ACL or credential-store adapter there.
- The `server` code uses `cfg(unix)` in `src/server/store.rs` for file permission checks.

Published version: see `INDEX.md` at the repository root. Release notes: see `CHANGELOG.md` in this crate.

## Example

The server-mode flow below is illustrative. The compiled example is `examples/server_axum.rs`, which needs the `server-example` feature.

## Server mode

```toml
[dependencies]
rightkit-oauth = { version = "0.3.0", default-features = false, features = ["server"] }
rightkit-http = "0.2.0"
```

Daemons can omit `secrets` and `system-sources` (`default-features = false`): Linux has no `rightkit-secrets` backend.
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
with `subtle::ConstantTimeEq`, checks expiry & provider binding, then exchanges code.
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
ID tokens are never accepted through decode-only JWT checks. Existing `TokenSet` stays
unchanged; ID token is passed only to validator, not retained in token storage.

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
missing binding rejects callback before exchange. `server-example` enables its optional dependencies; Axum is already used
by RightKit's `agent-room-core`. Host supplies listener, TLS, authenticated begin route,
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
