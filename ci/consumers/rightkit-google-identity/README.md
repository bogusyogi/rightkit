# rightkit-google-identity 0.2.0 registry consumer (draft, PK10)

Standalone consumer, outside the workspace, pinning `rightkit-google-identity =0.2.0` exactly. It
exercises the protocol layer a sign-in path calls: `Pkce::from_verifier` against the RFC 7636
vector, `AuthRequest::new` and its URL, `parse_redirect` for a matching state and for a refused
state or error, `token_exchange_body`, `TokenResponse::from_json` and `granted`, `validate_claims`,
the base64url codec, and `verify_id_token` refusing input that is not a JWS.

State: no network, browser, credential store or files. The state and nonce are random, so the
check covers only their shape. No real user data. Run only by the CI consumer lane.

Not covered: ID-token signature verification against a real key set, the endpoint URLs and the
`drive.appdata` scope URL (both UNVERIFIED in the crate), `RefreshTokenStore` (it needs a
`rightkit-credentials` store, and this recipe avoids the OS credential store), the Android
Credential Manager path, and any token exchange over the network. PK10 must be revisited if the
protocol API changes.
