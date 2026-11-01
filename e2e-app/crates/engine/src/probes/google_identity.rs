//! rightkit-google-identity: build a real authorization request (PKCE S256, state, nonce), check the challenge against RFC 7636's
//! test vector, and parse a redirect: the code comes back for the right state; a wrong state and an error are refused.
use rightkit_google_identity::{b64url_decode, b64url_encode, parse_redirect, AuthRequest, Pkce};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // RFC 7636 appendix B.
    let rfc = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r7wW1gFWFOEjXk".into());
    if rfc.challenge != "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" {
        return Err(format!("S256 challenge {} is not RFC 7636's", rfc.challenge));
    }
    let req = AuthRequest::new("client-id.apps.example", "http://127.0.0.1:53682/callback", &["openid", "email"], None).map_err(|e| format!("auth request: {e}"))?;
    if !req.url.contains("code_challenge_method=S256") || !req.url.contains(&req.pkce.challenge) {
        return Err(format!("the authorization URL lacks its PKCE challenge: {}", req.url));
    }
    let code = parse_redirect(&format!("http://127.0.0.1:53682/callback?code=abc123&state={}", req.state), &req.state).map_err(|e| format!("redirect: {e}"))?;
    let wrong_state = parse_redirect("http://127.0.0.1:53682/callback?code=abc123&state=other", &req.state).is_err();
    let error = parse_redirect(&format!("?error=access_denied&state={}", req.state), &req.state).is_err();
    let round = b64url_decode(&b64url_encode(b"\x00\xffrightkit")).as_deref() == Some(&b"\x00\xffrightkit"[..]);
    if code != "abc123" || !wrong_state || !error || !round {
        return Err(format!("code={code} wrong_state_refused={wrong_state} error_refused={error} b64url_round_trip={round}"));
    }
    Ok(json!({ "code": code, "wrong_state_refused": wrong_state, "error_refused": error }))
}
