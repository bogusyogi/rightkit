# rightkit-google-identity

Sign in with Google, protocol layer only. Not yet published (0.1.0). Design: Adrian's 2026-10-16 own-cloud identity decision (Orthic-Labs/rightkit#162): Google subject as identity, scopes `openid`, `email` and the Drive appDataFolder scope, refresh token in `rightkit-credentials`.

- `Pkce::generate()`: verifier and S256 challenge.
- `AuthRequest::new(client_id, redirect_uri, scopes, login_hint)`: authorization URL, state and PKCE. `parse_redirect(url, expected_state)` returns the code.
- `token_exchange_body` / `token_refresh_body`: form bodies; the app posts them.
- `TokenResponse`, `decode_id_token_claims`, `validate_claims`.
- `RefreshTokenStore`: refresh token per Google subject in a `rightkit_credentials::Store`.
- `IdTokenProvider`: the seam the Android Credential Manager path (through rightkit-android) implements.

## Unverified

Endpoint URLs, the scope URL and the issuer list are not from a fetched source. `validate_claims` does not verify the ID-token signature; fetch Google's keys and verify before trusting a token that did not arrive over the token endpoint's TLS channel. No network, browser or Android code is in this crate.
