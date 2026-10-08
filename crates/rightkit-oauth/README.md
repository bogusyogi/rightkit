# rightkit-oauth

OAuth 2.0 for desktop apps & daemons. Desktop API remains unchanged: loopback S256 PKCE,
RFC 8628 device flow, existing provider presets, OS credential storage & refreshing resolution.
Version `0.3.0` adds opt-in `server` mode; nothing is committed or published by enabling it.

## Server mode

```toml
[dependencies]
rightkit-oauth = { version = "0.3.0", default-features = false, features = ["server"] }
rightkit-http = "0.2.0"
```

`0.3.0` is staged source, not a publication claim. Daemons can omit `secrets` &
`system-sources`: Linux's current `rightkit-secrets` backend is unavailable.
All methods are synchronous; call on a blocking executor in an async HTTP route.
Crate starts no server, opens no browser & schedules no background work.

```rust,no_run
use std::{sync::Arc, time::Duration};
use rightkit_http::{Client, ClientConfig};
use rightkit_oauth::server::{CallbackQuery, MemoryPendingStateStore, SealedFileTokenStore,
    ServerAuthFlow, ServerProvider, TokenStore};

# fn demo(client_secret: String, encryption_key: [u8; 32], query: CallbackQuery)
# -> Result<(), Box<dyn std::error::Error>> {
let flow = ServerAuthFlow::new(
    Client::new(ClientConfig::default()),
    [ServerProvider::linkedin("registered-client-id", client_secret,
        "https://postright.orthiclabs.com/oauth/callback")],
    Arc::new(MemoryPendingStateStore::default()),
    Duration::from_secs(300),
)?;
// Run only for an authenticated user; account_hint identifies their internal account.
let begin = flow.begin("linkedin", "tenant:account", &["w_member_social".into()])?;
// Host redirects user's browser to begin.authorize_url.
// Host decodes callback query once, rejects duplicate fields & calls complete.
let complete = flow.complete_bound(query, "linkedin", "tenant:account")?;
let store = SealedFileTokenStore::new("/var/lib/postright/oauth", encryption_key)?;
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

## PostRight integration decisions

- Exact per-provider registered redirect paths, client registrations, approved scopes & Graph version.
- Facebook Login vs Instagram Login; user/page/Instagram identity resolution & long-lived token policy.
- LinkedIn partner refresh eligibility & whether OIDC identity scopes are needed.
- Pinterest registration age & continuous-refresh grant requirements.
- Durable pending-state adapter for restarts/multiple engines; database/fenced locking for multi-host tokens.
- Key provisioning, backup/key rotation, refresh scheduler cadence & reauth/recovery UX.
