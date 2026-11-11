# rightkit-google-identity

**Deprecated, not published.** Use [`rightkit-oauth`](../rightkit-oauth/README.md) with its `google` feature. This crate held a second copy of PKCE, redirect parsing, token request bodies, an RSA verifier and Google ID-token verification that `rightkit-oauth` already provides (and `rightkit-google-apis` already uses). No app or crate depended on it.

From 0.3.0 it is `publish = false` and only re-exports `rightkit_oauth::google`. 0.1.0 stays on crates.io; no new version is published.

| Was here | Use |
|---|---|
| `Pkce`, `AuthRequest`, `parse_redirect`, `token_exchange_body`, `token_refresh_body`, `TokenResponse` | `rightkit_oauth::OAuthClient` with `OAuthConfig::google` (loopback PKCE, code exchange, refresh) |
| `verify_id_token`, `verify_jws_rs256`, `JwkSet`, `decode_id_token_claims`, `validate_claims` | `rightkit_oauth::google::GoogleIdVerifier` (fetches and caches Google's keys) or `verify_with_jwks` (caller-supplied keys and clock) |
| `RefreshTokenStore` | `rightkit_oauth::google::GoogleSession` over a `TokenStore` (normally the OS credential store) |
| `SCOPE_DRIVE_APPDATA`, `ISSUERS` | `rightkit_oauth::google::DRIVE_APPDATA_SCOPE`, `GOOGLE_ISSUERS` |
| `IdTokenProvider` (Android Credential Manager seam, never implemented) | platform code obtains the ID token and passes it to `GoogleIdVerifier::verify` |

Changes are listed in [CHANGELOG.md](CHANGELOG.md).
