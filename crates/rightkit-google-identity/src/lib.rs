//! `rightkit-google-identity` 0.3.0, DEPRECATED and unpublished: use `rightkit-oauth` with its
//! `google` feature.
//!
//! This crate held a second copy of PKCE, the authorization URL and redirect parsing, the token
//! request bodies, base64url, an in-crate RSA verifier and Google ID-token verification. All of it
//! already lives in `rightkit-oauth`, which `rightkit-google-apis` uses:
//!
//! | Was here | Use |
//! |---|---|
//! | `Pkce`, `AuthRequest`, `parse_redirect`, `token_*_body`, `TokenResponse` | `rightkit_oauth::OAuthClient` with `OAuthConfig::google` (loopback PKCE, exchange, refresh) |
//! | `verify_id_token`, `verify_jws_rs256`, `JwkSet`, `validate_claims` | [`GoogleIdVerifier`] (fetches and caches Google's keys) or [`verify_with_jwks`] (caller-supplied keys and clock), returning [`GoogleIdentity`] |
//! | `RefreshTokenStore` | [`GoogleSession`] over a `rightkit_oauth::TokenStore` (normally the OS credential store) |
//! | `SCOPE_DRIVE_APPDATA`, `ISSUERS` | [`DRIVE_APPDATA_SCOPE`], [`GOOGLE_ISSUERS`] |
//! | `IdTokenProvider` (Android Credential Manager seam, never implemented) | the platform code obtains the ID token and passes it to [`GoogleIdVerifier::verify`] |
//!
//! This crate only re-exports `rightkit_oauth::google`, so the old crate name points at the one
//! implementation. 0.1.0 stays on crates.io; no new version is published.

pub use rightkit_oauth::google::*;
