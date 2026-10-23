// rightkit-google-identity 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the protocol-layer family a sign-in path calls (rightkit-google-identity lib.rs): the
// RFC 7636 PKCE challenge for a fixed verifier; the authorization URL with its scopes, S256 method,
// offline access, login hint and state; parse_redirect accepting the matching state and refusing a
// mismatch or an error; the token exchange body with no client secret; TokenResponse parsing and
// granted(); validate_claims for issuer, audience, expiry and nonce; the base64url codec; and
// verify_id_token refusing input that is not a JWS.
// Does not prove: ID-token signature verification against a real key set, the endpoint URLs and
// the drive.appdata scope URL (UNVERIFIED in the crate), RefreshTokenStore and the credential
// store, the Android Credential Manager path, or any network exchange.
// State: no network, browser, credential store or files. The state and nonce are random, so only
// their shape is checked. No real user data.
use rightkit_google_identity::{
    b64url_decode, b64url_encode, parse_redirect, token_exchange_body, validate_claims,
    verify_id_token, AuthRequest, Error, JwkSet, Pkce, TokenResponse, UnverifiedClaims,
    AUTH_ENDPOINT, SCOPE_DRIVE_APPDATA, SCOPE_EMAIL, SCOPE_OPENID,
};

const CLIENT_ID: &str = "consumer-test-client";
const REDIRECT: &str = "com.example.consumer:/oauth2redirect";
// RFC 7636 appendix B vector.
const RFC7636_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const RFC7636_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // PKCE: the S256 challenge of the RFC 7636 verifier.
    let pkce = Pkce::from_verifier(RFC7636_VERIFIER.to_string());
    assert_eq!(pkce.challenge, RFC7636_CHALLENGE);

    // Authorization URL: endpoint, scopes, PKCE, offline access and login hint. State and nonce
    // are random, so only their shape is checked (16 random bytes, 22 unpadded base64url chars).
    let scopes = [SCOPE_OPENID, SCOPE_EMAIL, SCOPE_DRIVE_APPDATA];
    let req = AuthRequest::new(CLIENT_ID, REDIRECT, &scopes, Some("user@example.test"))?;
    assert!(req.url.starts_with(&format!("{AUTH_ENDPOINT}?")));
    assert!(req.url.contains("scope=openid%20email%20"));
    assert!(req.url.contains("code_challenge_method=S256"));
    assert!(req
        .url
        .contains(&format!("code_challenge={}", req.pkce.challenge)));
    assert!(req.url.contains("access_type=offline"));
    assert!(req.url.contains("login_hint=user%40example.test"));
    assert!(req.url.contains(&format!("state={}", req.state)));
    assert_eq!(req.state.len(), 22);

    // Redirect: the matching state yields the code; a different state and an error are refused.
    let redirect = format!("{REDIRECT}?code=fixture-code&state={}", req.state);
    assert_eq!(parse_redirect(&redirect, &req.state)?, "fixture-code");
    let forged = parse_redirect(&redirect, "other-state").unwrap_err();
    assert!(matches!(forged, Error::Redirect("state mismatch")));
    let denied = format!("{REDIRECT}?error=access_denied&state={}", req.state);
    assert!(matches!(
        parse_redirect(&denied, &req.state),
        Err(Error::Redirect("authorization error"))
    ));

    // Token exchange body: the verifier is sent, and no client secret is added for a native client.
    let body = token_exchange_body(CLIENT_ID, None, REDIRECT, "fixture-code", RFC7636_VERIFIER);
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains(&format!("code_verifier={RFC7636_VERIFIER}")));
    assert!(!body.contains("client_secret"));

    // Token response: parsing, the granted-scope check on whole words, and a malformed body.
    let tok = TokenResponse::from_json(
        r#"{"access_token":"fixture-access","expires_in":3599,"refresh_token":"fixture-refresh","scope":"openid email https://www.googleapis.com/auth/drive.appdata","token_type":"Bearer"}"#,
    )?;
    assert_eq!(tok.expires_in, 3599);
    assert!(tok.granted(SCOPE_DRIVE_APPDATA));
    assert!(!tok.granted("https://www.googleapis.com/auth/drive"));
    assert!(tok.id_token.is_none());
    assert!(matches!(
        TokenResponse::from_json("not json"),
        Err(Error::Token("unparseable token response"))
    ));

    // Claims: issuer, audience, expiry and nonce are checked on unauthenticated claims.
    let claims = UnverifiedClaims {
        iss: "https://accounts.google.com".to_string(),
        sub: "fixture-subject".to_string(),
        aud: CLIENT_ID.to_string(),
        exp: 2_000,
        email: None,
        email_verified: None,
        nonce: Some("n-1".to_string()),
    };
    validate_claims(&claims, CLIENT_ID, Some("n-1"), 1_000)?;
    assert!(matches!(
        validate_claims(&claims, CLIENT_ID, Some("n-1"), 2_000),
        Err(Error::Token("expired"))
    ));
    assert!(matches!(
        validate_claims(&claims, "other-client", None, 1_000),
        Err(Error::Token("audience mismatch"))
    ));
    let wrong_issuer = UnverifiedClaims {
        iss: "https://example.test".to_string(),
        ..claims.clone()
    };
    assert!(matches!(
        validate_claims(&wrong_issuer, CLIENT_ID, None, 1_000),
        Err(Error::Token("unexpected issuer"))
    ));
    assert!(matches!(
        validate_claims(&claims, CLIENT_ID, Some("n-2"), 1_000),
        Err(Error::Token("nonce mismatch"))
    ));

    // Signed-token entry point: input that is not a JWS is refused before any key is consulted.
    let empty_keys = JwkSet { keys: Vec::new() };
    assert!(matches!(
        verify_id_token("not-a-jws", &empty_keys, CLIENT_ID, None, 1_000),
        Err(Error::Token("not a JWS"))
    ));

    // base64url: round trip of arbitrary bytes, RFC 4648 vector without padding, and a refused '+'.
    let bytes = [0u8, 1, 2, 250, 251, 252, 253, 254, 255];
    let encoded = b64url_encode(&bytes);
    assert_eq!(b64url_decode(&encoded).as_deref(), Some(&bytes[..]));
    assert_eq!(b64url_encode(b"f"), "Zg");
    assert!(b64url_decode("Zg+").is_none());

    println!("OK rightkit-google-identity 0.2.0 (registry consumer)");
    Ok(())
}
