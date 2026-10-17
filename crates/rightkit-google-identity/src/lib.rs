//! `rightkit-google-identity` - Sign in with Google, protocol layer.
//!
//! Wire shapes from the issue (#162): OAuth PKCE, the Google ID token, scopes `openid` / `email`
//! plus `drive.appdata`, refresh tokens in `rightkit-credentials`, an Android Credential Manager
//! path. UNVERIFIED (not in the issue text, no fetched source): endpoint URLs, the full
//! `drive.appdata` scope URL, accepted issuers. ID tokens are authenticated only by
//! [`verify_id_token`] (RS256 against a caller-supplied key set); everything else here grants nothing.

mod rsa;

use rightkit_credentials::{Secret, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCOPE_OPENID: &str = "openid";
pub const SCOPE_EMAIL: &str = "email";
/// UNVERIFIED full URL form of `drive.appdata`.
pub const SCOPE_DRIVE_APPDATA: &str = "https://www.googleapis.com/auth/drive.appdata";
/// UNVERIFIED.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// UNVERIFIED.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// UNVERIFIED.
pub const ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

#[derive(Debug)]
pub enum Error {
    Random,
    Redirect(&'static str),
    Token(&'static str),
    Store(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Random => write!(f, "operating system random source failed"),
            Error::Redirect(m) => write!(f, "redirect: {m}"),
            Error::Token(m) => write!(f, "token: {m}"),
            Error::Store(m) => write!(f, "credential store: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Unpadded base64url.
pub fn b64url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64URL[(n >> 18) as usize & 63] as char);
        out.push(B64URL[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(B64URL[n as usize & 63] as char);
        }
    }
    out
}

/// Unpadded base64url decode; `None` on any invalid character or length.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for ch in s.bytes() {
        let v = B64URL.iter().position(|&b| b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn random_b64url<const N: usize>() -> Result<String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| Error::Random)?;
    Ok(b64url_encode(&b))
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", pct_encode(k), pct_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// 32 random bytes as the verifier; challenge = base64url(SHA-256(verifier)) (method S256).
    pub fn generate() -> Result<Pkce> {
        let verifier = random_b64url::<32>()?;
        Ok(Pkce::from_verifier(verifier))
    }

    pub fn from_verifier(verifier: String) -> Pkce {
        let challenge = b64url_encode(&Sha256::digest(verifier.as_bytes())[..]);
        Pkce {
            verifier,
            challenge,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthRequest {
    pub url: String,
    pub state: String,
    pub nonce: String,
    pub pkce: Pkce,
}

impl AuthRequest {
    /// Build the authorization URL for the default browser. `scopes` is typically
    /// `[SCOPE_OPENID, SCOPE_EMAIL, SCOPE_DRIVE_APPDATA]`. `access_type=offline` requests a
    /// refresh token.
    pub fn new(
        client_id: &str,
        redirect_uri: &str,
        scopes: &[&str],
        login_hint: Option<&str>,
    ) -> Result<AuthRequest> {
        let pkce = Pkce::generate()?;
        let state = random_b64url::<16>()?;
        let nonce = random_b64url::<16>()?;
        let scope = scopes.join(" ");
        let mut pairs = vec![
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope.as_str()),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge", pkce.challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ];
        if let Some(h) = login_hint {
            pairs.push(("login_hint", h));
        }
        Ok(AuthRequest {
            url: format!("{AUTH_ENDPOINT}?{}", form(&pairs)),
            state,
            nonce,
            pkce,
        })
    }
}

/// Extract the authorization code from the redirect URL (or bare query string). The state must
/// match and no `error` may be present.
pub fn parse_redirect(redirect: &str, expected_state: &str) -> Result<String> {
    let query = redirect.split_once('?').map(|(_, q)| q).unwrap_or(redirect);
    let query = query.split('#').next().unwrap_or("");
    let (mut code, mut state, mut error) = (None, None, false);
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        match k {
            "code" => code = pct_decode(v),
            "state" => state = pct_decode(v),
            "error" => error = true,
            _ => {}
        }
    }
    if error {
        return Err(Error::Redirect("authorization error"));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(Error::Redirect("state mismatch"));
    }
    code.filter(|c| !c.is_empty())
        .ok_or(Error::Redirect("missing code"))
}

/// Form body for the code exchange (POST to [`TOKEN_ENDPOINT`]). A native-app client has no
/// secret; pass `client_secret` only if the client type requires one.
pub fn token_exchange_body(
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> String {
    let mut p = vec![
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("code", code),
        ("code_verifier", verifier),
    ];
    if let Some(s) = client_secret {
        p.push(("client_secret", s));
    }
    form(&p)
}

pub fn token_refresh_body(
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> String {
    let mut p = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    if let Some(s) = client_secret {
        p.push(("client_secret", s));
    }
    form(&p)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
}

impl TokenResponse {
    pub fn from_json(body: &str) -> Result<TokenResponse> {
        serde_json::from_str(body).map_err(|_| Error::Token("unparseable token response"))
    }

    /// Whether the granted scope string includes `scope` (absent scope field: unknown, false).
    pub fn granted(&self, scope: &str) -> bool {
        self.scope
            .as_deref()
            .is_some_and(|s| s.split(' ').any(|g| g == scope))
    }
}

/// ID-token claims decoded WITHOUT authentication. Holding one grants nothing: anyone can forge
/// these fields. Only [`VerifiedClaims`] (from [`verify_id_token`]) may drive a decision.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnverifiedClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: u64,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub nonce: Option<String>,
}

/// Claims whose RS256 signature verified against a supplied key and whose issuer, audience,
/// expiry and nonce checks passed. Constructible only inside [`verify_id_token`].
#[derive(Debug, Clone)]
pub struct VerifiedClaims {
    claims: UnverifiedClaims,
}

impl VerifiedClaims {
    pub fn claims(&self) -> &UnverifiedClaims {
        &self.claims
    }

    pub fn sub(&self) -> &str {
        &self.claims.sub
    }
}

/// One RSA public key of a JWK set (RFC 7517): key id, base64url modulus and exponent.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kid: String,
    pub n: String,
    pub e: String,
    #[serde(default)]
    pub kty: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<Jwk>,
}

impl JwkSet {
    pub fn from_json(body: &str) -> Result<JwkSet> {
        serde_json::from_str(body).map_err(|_| Error::Token("unparseable key set"))
    }
}

#[derive(Deserialize)]
struct JwsHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

/// Verify a compact JWS signed with RS256 against `keys` and return its payload bytes.
/// Only `alg` = `RS256` is accepted. The key is chosen by `kid`; a header without `kid` is
/// accepted only when the set holds exactly one usable key.
pub fn verify_jws_rs256(token: &str, keys: &JwkSet) -> Result<Vec<u8>> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::Token("not a JWS"));
    };
    let header: JwsHeader =
        serde_json::from_slice(&b64url_decode(h).ok_or(Error::Token("bad base64url"))?)
            .map_err(|_| Error::Token("bad header"))?;
    if header.alg != "RS256" {
        return Err(Error::Token("algorithm not allowed"));
    }
    let usable: Vec<&Jwk> = keys
        .keys
        .iter()
        .filter(|k| k.kty.as_deref().is_none_or(|t| t == "RSA"))
        .filter(|k| k.alg.as_deref().is_none_or(|a| a == "RS256"))
        .collect();
    let key = match header.kid.as_deref() {
        Some(kid) => usable.iter().find(|k| k.kid == kid).copied(),
        None if usable.len() == 1 => Some(usable[0]),
        None => None,
    }
    .ok_or(Error::Token("no matching key"))?;
    let n = b64url_decode(&key.n).ok_or(Error::Token("bad key"))?;
    let e = b64url_decode(&key.e).ok_or(Error::Token("bad key"))?;
    let sig = b64url_decode(s).ok_or(Error::Token("bad base64url"))?;
    let input = format!("{h}.{p}");
    if !rsa::verify_rs256(&n, &e, input.as_bytes(), &sig) {
        return Err(Error::Token("bad signature"));
    }
    b64url_decode(p).ok_or(Error::Token("bad base64url"))
}

/// Authenticate an ID token: RS256 signature against `keys` first, then issuer, audience,
/// expiry and (when given) nonce. The only way to obtain [`VerifiedClaims`].
pub fn verify_id_token(
    id_token: &str,
    keys: &JwkSet,
    client_id: &str,
    expected_nonce: Option<&str>,
    now_unix: u64,
) -> Result<VerifiedClaims> {
    let payload = verify_jws_rs256(id_token, keys)?;
    let claims: UnverifiedClaims =
        serde_json::from_slice(&payload).map_err(|_| Error::Token("bad claims"))?;
    validate_claims(&claims, client_id, expected_nonce, now_unix)?;
    Ok(VerifiedClaims { claims })
}

/// Decode the payload segment of an ID token. The signature is NOT verified; the result grants
/// nothing.
pub fn decode_id_token_claims(id_token: &str) -> Result<UnverifiedClaims> {
    let mut parts = id_token.split('.');
    let (_, payload, _) = (parts.next(), parts.next(), parts.next());
    let payload = payload.ok_or(Error::Token("not a JWT"))?;
    let bytes = b64url_decode(payload).ok_or(Error::Token("bad base64url"))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Token("bad claims"))
}

/// Check issuer, audience, expiry and (when given) nonce of UNAUTHENTICATED claims. Passing
/// this proves nothing about who issued the token; use [`verify_id_token`].
pub fn validate_claims(
    c: &UnverifiedClaims,
    client_id: &str,
    expected_nonce: Option<&str>,
    now_unix: u64,
) -> Result<()> {
    if !ISSUERS.contains(&c.iss.as_str()) {
        return Err(Error::Token("unexpected issuer"));
    }
    if c.aud != client_id {
        return Err(Error::Token("audience mismatch"));
    }
    if now_unix >= c.exp {
        return Err(Error::Token("expired"));
    }
    if let Some(n) = expected_nonce {
        if c.nonce.as_deref() != Some(n) {
            return Err(Error::Token("nonce mismatch"));
        }
    }
    Ok(())
}

/// Source of an ID token from a native path (Android Credential Manager through
/// rightkit-android). The implementation lives with the platform code.
pub trait IdTokenProvider {
    fn id_token(&self, client_id: &str, nonce: &str) -> Result<String>;
}

/// Refresh token per Google subject in a `rightkit_credentials::Store`.
pub struct RefreshTokenStore {
    store: Store,
}

impl RefreshTokenStore {
    pub fn new(store: Store) -> Self {
        RefreshTokenStore { store }
    }

    fn account(sub: &str) -> String {
        format!("google-refresh-token:{sub}")
    }

    pub fn save(&self, sub: &str, refresh_token: &str) -> Result<()> {
        self.store
            .set(&Self::account(sub), &Secret::from_string(refresh_token))
            .map_err(|e| Error::Store(e.to_string()))
    }

    pub fn load(&self, sub: &str) -> Result<Option<String>> {
        match self
            .store
            .get(&Self::account(sub))
            .map_err(|e| Error::Store(e.to_string()))?
        {
            None => Ok(None),
            Some(s) => s
                .expose_str()
                .map(|v| Some(v.to_string()))
                .map_err(|e| Error::Store(e.to_string())),
        }
    }

    /// Returns whether a token was stored.
    pub fn delete(&self, sub: &str) -> Result<bool> {
        self.store
            .delete(&Self::account(sub))
            .map_err(|e| Error::Store(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn b64url_round_trip() {
        for n in 0..8 {
            let v: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            assert_eq!(b64url_decode(&b64url_encode(&v)), Some(v));
        }
        assert_eq!(b64url_decode("a"), None);
        assert_eq!(b64url_decode("a+b="), None);
    }

    #[test]
    fn request_and_redirect() {
        let r = AuthRequest::new(
            "cid",
            "http://127.0.0.1:1234/cb",
            &[SCOPE_OPENID, SCOPE_EMAIL, SCOPE_DRIVE_APPDATA],
            None,
        )
        .unwrap();
        assert!(r.url.contains("code_challenge_method=S256"));
        assert!(r
            .url
            .contains("scope=openid%20email%20https%3A%2F%2Fwww.googleapis.com"));
        let ok = format!("http://127.0.0.1:1234/cb?code=4%2Fabc&state={}", r.state);
        assert_eq!(parse_redirect(&ok, &r.state).unwrap(), "4/abc");
        assert!(parse_redirect("cb?code=x&state=other", &r.state).is_err());
        assert!(parse_redirect(
            &format!("cb?error=access_denied&state={}", r.state),
            &r.state
        )
        .is_err());
        assert!(parse_redirect(&format!("cb?state={}", r.state), &r.state).is_err());
    }

    #[test]
    fn claims_checks() {
        let payload = b64url_encode(
            br#"{"iss":"https://accounts.google.com","sub":"123","aud":"cid","exp":2000,"email":"a@b.c","nonce":"n1"}"#,
        );
        let jwt = format!("h.{payload}.s");
        let c = decode_id_token_claims(&jwt).unwrap();
        assert_eq!(c.sub, "123");
        validate_claims(&c, "cid", Some("n1"), 1000).unwrap();
        assert!(validate_claims(&c, "other", None, 1000).is_err());
        assert!(validate_claims(&c, "cid", None, 2000).is_err());
        assert!(validate_claims(&c, "cid", Some("n2"), 1000).is_err());
        assert!(decode_id_token_claims("nodots").is_err());
    }

    #[test]
    fn token_response_and_bodies() {
        let t = TokenResponse::from_json(
            r#"{"access_token":"a","expires_in":3599,"refresh_token":"r","scope":"openid email"}"#,
        )
        .unwrap();
        assert!(t.granted("email") && !t.granted(SCOPE_DRIVE_APPDATA));
        let b = token_exchange_body("cid", None, "http://x/cb", "4/abc", "ver");
        assert!(b.starts_with("grant_type=authorization_code&client_id=cid"));
        assert!(b.contains("code=4%2Fabc"));
        assert!(token_refresh_body("cid", Some("s"), "r").ends_with("client_secret=s"));
    }

    const RFC_JWS: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.cC4hiUPoj9Eetdgtv3hF80EGrhuB__dzERat0XF9g2VtQgr9PJbu3XOiZj5RZmh7AAuHIm4Bh-0Qc_lF5YKt_O8W2Fp5jujGbds9uJdbF9CUAr7t1dnZcAcQjbKBYNX4BAynRFdiuB--f_nZLgrnbyTyWzO75vRK5h6xBArLIARNPvkSjtQBMHlb1L07Qe7K0GarZRmB_eSN9383LcOLn6_dO--xi12jzDwusC-eOkHWEsqtFZESc6BfI7noOPqvhJ1phCnvWh6IeYI2w9QOYEUipUTI8np6LbgGY9Fs98rqVt5AXLIhWkWywlVmtVrBp0igcN_IoypGlUPQGe77Rw";
    const RFC_N: &str = "ofgWCuLjybRlzo0tZWJjNiuSfb4p4fAkd_wWJcyQoTbji9k0l8W26mPddxHmfHQp-Vaw-4qPCJrcS2mJPMEzP1Pt0Bm4d4QlL-yRT-SFd2lZS-pCgNMsD1W_YpRPEwOWvG6b32690r2jZ47soMZo9wGzjb_7OMg0LOL-bSf63kpaSHSXndS5z5rexMdbBYUsLA9e-KXBdQOS-UTo7WTBEMa2R2CapHg665xsmtdVMTBQY4uDZlxvb3qCo5ZwKh9kG4LT6_I5IhlJH7aGhyxXFvUK-DWNmoudF8NAco9_h9iaGNj8q2ethFkMLs91kzk2PAcDTW9gb54h4FRWyuXpoQ";

    fn rfc_keys(kid: &str) -> JwkSet {
        JwkSet::from_json(&format!(
            r#"{{"keys":[{{"kty":"RSA","alg":"RS256","kid":"{kid}","n":"{RFC_N}","e":"AQAB"}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn rfc7515_a2_signature_verifies() {
        let payload = verify_jws_rs256(RFC_JWS, &rfc_keys("k1")).unwrap();
        assert!(payload.starts_with(b"{\"iss\":\"joe\""));
        // The RFC payload has no `sub`/`aud`, so it is not ID-token claims: the full path
        // rejects it after the signature verified (a bad signature reports differently).
        assert!(matches!(
            verify_id_token(RFC_JWS, &rfc_keys("k1"), "cid", None, 1000),
            Err(Error::Token("bad claims"))
        ));
        // Claims validation on its own rejects the foreign issuer.
        let claims = UnverifiedClaims {
            iss: "joe".into(),
            sub: "1".into(),
            aud: "cid".into(),
            exp: 1_300_819_380,
            email: None,
            email_verified: None,
            nonce: None,
        };
        assert!(matches!(
            validate_claims(&claims, "cid", None, 1000),
            Err(Error::Token("unexpected issuer"))
        ));
    }

    #[test]
    fn forged_or_downgraded_tokens_fail() {
        let keys = rfc_keys("k1");
        // Tampered payload.
        let mut parts: Vec<&str> = RFC_JWS.split('.').collect();
        let forged = b64url_encode(
            br#"{"iss":"accounts.google.com","sub":"1","aud":"cid","exp":9999999999}"#,
        );
        parts[1] = &forged;
        assert!(matches!(
            verify_id_token(&parts.join("."), &keys, "cid", None, 1),
            Err(Error::Token("bad signature"))
        ));
        // alg none / HS256 / unsigned shape.
        for alg in ["none", "HS256"] {
            let h = b64url_encode(format!(r#"{{"alg":"{alg}"}}"#).as_bytes());
            let t = format!("{h}.{}.{}", parts[1], parts[2]);
            assert!(matches!(
                verify_jws_rs256(&t, &keys),
                Err(Error::Token("algorithm not allowed"))
            ));
        }
        assert!(verify_jws_rs256("a.b", &keys).is_err());
        assert!(verify_jws_rs256(&format!("{RFC_JWS}.x"), &keys).is_err());
        // Unknown kid.
        let h = b64url_encode(br#"{"alg":"RS256","kid":"other"}"#);
        let t = format!("{h}.{}.{}", parts[1], parts[2]);
        assert!(matches!(
            verify_jws_rs256(&t, &keys),
            Err(Error::Token("no matching key"))
        ));
    }

    #[test]
    fn weak_or_malformed_keys_are_refused() {
        let sig = b64url_decode(RFC_JWS.rsplit('.').next().unwrap()).unwrap();
        let n = b64url_decode(RFC_N).unwrap();
        assert!(!rsa::verify_rs256(&n[..128], &[1, 0, 1], b"x", &sig[..128]));
        assert!(!rsa::verify_rs256(&n, &[1, 0, 0], b"x", &sig));
        assert!(!rsa::verify_rs256(&n, &[1, 0, 1], b"x", &sig[1..]));
    }
}
