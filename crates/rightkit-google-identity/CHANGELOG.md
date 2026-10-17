# Changelog

## Unreleased

- New crate 0.1.0 (Orthic-Labs/rightkit#162). OAuth PKCE (`Pkce`, `AuthRequest`, `parse_redirect`), token request bodies and `TokenResponse`, ID-token claim decoding and `validate_claims`, `RefreshTokenStore` over rightkit-credentials, `IdTokenProvider` seam for the Android Credential Manager path.
- Unverified: the Google endpoint URLs, the `drive.appdata` scope URL and the accepted issuers come from Google's public documentation as remembered, not from the issue text or a fetched source. Key-set fetching and caching stay with the caller.
- ID-token authentication: `verify_id_token` / `verify_jws_rs256` check RS256 (only) against a caller-supplied `JwkSet` with an in-crate RSA-PKCS1-v1_5 verifier (RFC 7515 A.2 vector tested) and return `VerifiedClaims`; `IdClaims` is renamed `UnverifiedClaims` and grants nothing.
- Not implemented: the Android Credential Manager call itself (needs rightkit-android), browser launch, HTTP, Drive access.
- Not compiled or run when written; verification is the CI run.
