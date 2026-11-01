//! rightkit-oauth: PKCE verifiers per RFC 7636 (length and alphabet enforced, S256 matches the RFC vector), state generation, and a
//! loopback callback socket bound on 127.0.0.1.
use rightkit_oauth::pkce::{challenge_s256, generate_state, validate_verifier, PkceVerifier};
use serde_json::{json, Value};

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let s = challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
        .map_err(|e| format!("challenge: {e}"))?;
    if s != "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" {
        return Err(format!("S256 challenge {s} is not RFC 7636's"));
    }
    let v = PkceVerifier::generate().map_err(|e| format!("generate: {e}"))?;
    if v.expose().len() < 43 || validate_verifier(v.expose()).is_err() {
        return Err("a generated verifier is not a valid RFC 7636 verifier".into());
    }
    let short_refused = validate_verifier("too-short").is_err();
    let bad_char_refused = validate_verifier(&format!("{}!", "a".repeat(42))).is_err();
    let (a, b) = (
        generate_state().map_err(|e| e.to_string())?,
        generate_state().map_err(|e| e.to_string())?,
    );
    if !short_refused || !bad_char_refused || a == b {
        return Err(format!(
            "short refused {short_refused}, bad char refused {bad_char_refused}, states differ {}",
            a != b
        ));
    }
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("loopback bind: {e}"))?;
    Ok(
        json!({ "s256": s, "verifier_len": v.expose().len(), "loopback": listener.local_addr().map(|a| a.to_string()).unwrap_or_default() }),
    )
}
