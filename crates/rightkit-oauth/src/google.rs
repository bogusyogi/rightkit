//! Sign in with Google: ID-token verification and a stored, refreshing Google session for
//! Drive sync. Feature `google`.
//!
//! Desktop sign-in reuses the loopback PKCE flow of [`OAuthClient`]; this module adds what that
//! flow does not: proof of who signed in ([`GoogleIdVerifier`]) and a session that keeps the
//! refresh token in the supplied [`TokenStore`] (normally
//! [`SecretTokenStore`](crate::credentials::SecretTokenStore), the OS credential store) and hands
//! out a fresh access token on demand ([`GoogleSession`]).
//!
//! Endpoint and claim rules follow Google's OpenID Connect documentation and discovery document
//! (<https://accounts.google.com/.well-known/openid-configuration>, checked 2026-10-16):
//! `iss` is `https://accounts.google.com` or `accounts.google.com`, `aud` is the app's client id,
//! `exp` must not have passed, the signing algorithm is RS256 and the keys are at
//! [`GOOGLE_JWKS_URL`].

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rightkit_http::{Client, Request, Secret};
use serde_json::Value;

use crate::credentials::{Resolution, Resolver};
use crate::{OAuthClient, OAuthError, TokenSet, TokenStore};

/// Google's signing keys (the discovery document's `jwks_uri`).
pub const GOOGLE_JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
/// Accepted `iss` values.
pub const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];
/// The scope Drive sync needs; sign-in fails if the user did not grant it.
pub const DRIVE_APPDATA_SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
/// Clock slack applied to `exp` and `iat`.
pub const DEFAULT_CLOCK_SKEW: Duration = Duration::from_secs(60);
/// How long fetched keys are reused before a refetch.
pub const JWKS_TTL: Duration = Duration::from_secs(3600);
/// An unknown `kid` forces a refetch (key rotation), but never more often than this, so a
/// stream of forged tokens cannot turn into a stream of requests to Google.
pub const JWKS_MIN_REFETCH: Duration = Duration::from_secs(30);
/// Refresh the access token this long before it expires.
pub const ACCESS_REFRESH_SKEW: Duration = crate::credentials::DEFAULT_REFRESH_SKEW;
/// Longest ID token accepted (Google's are about 1 to 2 KiB).
const MAX_ID_TOKEN_BYTES: usize = 16 * 1024;

/// Why an ID token was refused. Never carries token bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdTokenError {
    Malformed,
    /// Not RS256 (including `none`).
    UnsupportedAlgorithm,
    /// No key with the token's `kid`, even after a refetch.
    UnknownKey,
    BadSignature,
    BadIssuer,
    BadAudience,
    Expired,
    /// `iat` or `nbf` is in the future beyond the skew.
    NotYetValid,
    MissingSubject,
    /// A nonce was expected and did not match.
    NonceMismatch,
    /// The key set could not be fetched or parsed.
    Keys(String),
}

impl fmt::Display for IdTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = match self {
            Self::Malformed => "malformed ID token",
            Self::UnsupportedAlgorithm => "ID token algorithm is not RS256",
            Self::UnknownKey => "ID token signing key is unknown",
            Self::BadSignature => "ID token signature is invalid",
            Self::BadIssuer => "ID token issuer is not Google",
            Self::BadAudience => "ID token audience is not this app",
            Self::Expired => "ID token has expired",
            Self::NotYetValid => "ID token is not yet valid",
            Self::MissingSubject => "ID token has no subject",
            Self::NonceMismatch => "ID token nonce mismatch",
            Self::Keys(m) => return write!(f, "Google signing keys unavailable: {m}"),
        };
        f.write_str(m)
    }
}

impl std::error::Error for IdTokenError {}

impl From<IdTokenError> for OAuthError {
    fn from(e: IdTokenError) -> Self {
        OAuthError::Callback(e.to_string())
    }
}

/// The verified identity. `sub` is Google's stable account id; `email` can change, so key
/// accounts on `sub`.
#[derive(Clone, PartialEq, Eq)]
pub struct GoogleIdentity {
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
}

impl fmt::Debug for GoogleIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The address is personal data and never needs to reach a log line.
        f.debug_struct("GoogleIdentity")
            .field("sub", &self.sub)
            .field("email", &self.email.as_ref().map(|_| "[redacted]"))
            .field("email_verified", &self.email_verified)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct Jwk {
    kid: String,
    n: Vec<u8>,
    e: Vec<u8>,
}

/// A parsed JWK set (RSA keys only; other key types are skipped).
#[derive(Debug, Clone, Default)]
pub struct Jwks {
    keys: Vec<Jwk>,
}

impl Jwks {
    pub fn parse(json: &str) -> Result<Self, IdTokenError> {
        let v: Value = serde_json::from_str(json).map_err(|e| IdTokenError::Keys(e.to_string()))?;
        let list = v
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| IdTokenError::Keys("no keys array".into()))?;
        let mut keys = Vec::new();
        for k in list {
            if k.get("kty").and_then(Value::as_str) != Some("RSA") {
                continue;
            }
            if k.get("alg")
                .and_then(Value::as_str)
                .is_some_and(|a| a != "RS256")
            {
                continue;
            }
            let (Some(kid), Some(n), Some(e)) = (
                k.get("kid").and_then(Value::as_str),
                k.get("n").and_then(Value::as_str),
                k.get("e").and_then(Value::as_str),
            ) else {
                continue;
            };
            if let (Some(n), Some(e)) = (b64url_decode(n), b64url_decode(e)) {
                keys.push(Jwk {
                    kid: kid.into(),
                    n,
                    e,
                });
            }
        }
        Ok(Self { keys })
    }

    fn find(&self, kid: &str) -> Option<&Jwk> {
        self.keys.iter().find(|k| k.kid == kid)
    }
}

struct CachedKeys {
    jwks: Jwks,
    fetched: Instant,
}

/// Verifies Google ID tokens. Configure with every client id the app accepts: the desktop
/// client and, for tokens minted for a server or by Android Credential Manager, the web client
/// id the token is requested for (its `aud`).
pub struct GoogleIdVerifier {
    http: Client,
    jwks_url: String,
    client_ids: Vec<String>,
    skew: Duration,
    cache: Mutex<Option<CachedKeys>>,
    /// Always [`verify_rs256`]; unit tests substitute it to exercise the session without a
    /// signing key.
    sig_check: SigCheck,
}

impl fmt::Debug for GoogleIdVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleIdVerifier")
            .field("jwks_url", &self.jwks_url)
            .field("client_ids", &self.client_ids)
            .finish()
    }
}

impl GoogleIdVerifier {
    pub fn new(
        http: Client,
        client_ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, OAuthError> {
        let client_ids: Vec<String> = client_ids.into_iter().map(Into::into).collect();
        if client_ids.is_empty() || client_ids.iter().any(|c| c.trim().is_empty()) {
            return Err(OAuthError::InvalidConfig(
                "at least one non-empty client id is required".into(),
            ));
        }
        Ok(Self {
            http,
            jwks_url: GOOGLE_JWKS_URL.into(),
            client_ids,
            skew: DEFAULT_CLOCK_SKEW,
            cache: Mutex::new(None),
            sig_check: verify_rs256,
        })
    }

    /// Point at another JWKS endpoint (tests use a loopback mock).
    pub fn with_jwks_url(mut self, url: impl Into<String>) -> Self {
        self.jwks_url = url.into();
        self
    }

    pub fn with_skew(mut self, skew: Duration) -> Self {
        self.skew = skew;
        self
    }

    /// Verify `id_token` now. Fetches (and caches) Google's keys; an unknown `kid` forces one
    /// refetch (rate limited by [`JWKS_MIN_REFETCH`]), which covers key rotation.
    ///
    /// Pass the `nonce` sent in the authorization request; `None` skips the nonce check and is
    /// only right for tokens that carry none.
    pub fn verify(
        &self,
        id_token: &str,
        expected_nonce: Option<&str>,
    ) -> Result<GoogleIdentity, IdTokenError> {
        // Cheap structural checks first, so garbage never causes a network request.
        let kid = read_header(id_token)?.1;
        let jwks = self.keys(false)?;
        let jwks = if jwks.find(&kid).is_some() {
            jwks
        } else {
            self.keys(true)?
        };
        verify_token(
            id_token,
            &jwks,
            &self.client_ids,
            expected_nonce,
            SystemTime::now(),
            self.skew,
            self.sig_check,
        )
    }

    /// Read the identity out of an ID token this crate stored after verifying it at sign-in.
    /// Checks issuer, audience and subject but neither signature nor expiry (a stored token is
    /// hours old by design), so it is only for tokens read back from the credential store, never
    /// for a token received from anywhere else.
    pub fn read_trusted(&self, id_token: &str) -> Result<GoogleIdentity, IdTokenError> {
        if id_token.len() > MAX_ID_TOKEN_BYTES {
            return Err(IdTokenError::Malformed);
        }
        let mut parts = id_token.split('.');
        let (Some(_), Some(p), Some(_), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(IdTokenError::Malformed);
        };
        let claims = json_part(p)?;
        identity_from_claims(&claims, &self.client_ids, None, None)
    }

    fn keys(&self, force: bool) -> Result<Jwks, IdTokenError> {
        let mut guard = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = guard.as_ref() {
            let age = c.fetched.elapsed();
            if (!force && age < JWKS_TTL) || (force && age < JWKS_MIN_REFETCH) {
                return Ok(c.jwks.clone());
            }
        }
        let request = Request::get(&self.jwks_url).header("Accept", "application/json");
        let response = self
            .http
            .send(&request)
            .map_err(|e| IdTokenError::Keys(e.to_string()))?;
        let jwks = Jwks::parse(&response.text())?;
        *guard = Some(CachedKeys {
            jwks: jwks.clone(),
            fetched: Instant::now(),
        });
        Ok(jwks)
    }
}

/// Pure verification over a given key set and clock: signature (RS256 only), issuer, audience
/// (and `azp` when several audiences are present), expiry, `iat`/`nbf`, subject, nonce.
pub fn verify_with_jwks(
    id_token: &str,
    jwks: &Jwks,
    client_ids: &[String],
    expected_nonce: Option<&str>,
    now: SystemTime,
    skew: Duration,
) -> Result<GoogleIdentity, IdTokenError> {
    verify_token(
        id_token,
        jwks,
        client_ids,
        expected_nonce,
        now,
        skew,
        verify_rs256,
    )
}

/// Signature check for one key: `signed` is `header.payload` exactly as received.
type SigCheck = fn(&Jwk, &[u8], &[u8]) -> Result<(), IdTokenError>;

fn verify_rs256(key: &Jwk, signed: &[u8], sig: &[u8]) -> Result<(), IdTokenError> {
    ring::rsa::PublicKeyComponents {
        n: key.n.as_slice(),
        e: key.e.as_slice(),
    }
    .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, signed, sig)
    .map_err(|_| IdTokenError::BadSignature)
}

fn verify_token(
    id_token: &str,
    jwks: &Jwks,
    client_ids: &[String],
    expected_nonce: Option<&str>,
    now: SystemTime,
    skew: Duration,
    check: SigCheck,
) -> Result<GoogleIdentity, IdTokenError> {
    let (h, kid, p, s) = read_header(id_token)?;
    let key = jwks.find(&kid).ok_or(IdTokenError::UnknownKey)?;
    let sig = b64url_decode(s).ok_or(IdTokenError::Malformed)?;
    let signed = &id_token[..h.len() + 1 + p.len()];
    check(key, signed.as_bytes(), &sig)?;

    // Signature holds; only now are the claims trusted enough to read.
    let claims = json_part(p)?;
    identity_from_claims(&claims, client_ids, Some((now, skew)), expected_nonce)
}

/// Claim checks shared by [`verify_with_jwks`] and [`GoogleIdVerifier::read_trusted`]. `time`
/// is `None` only for the latter (no `exp`/`iat`/`nonce` checks).
fn identity_from_claims(
    c: &Value,
    client_ids: &[String],
    time: Option<(SystemTime, Duration)>,
    expected_nonce: Option<&str>,
) -> Result<GoogleIdentity, IdTokenError> {
    let iss = c.get("iss").and_then(Value::as_str).unwrap_or_default();
    if !GOOGLE_ISSUERS.contains(&iss) {
        return Err(IdTokenError::BadIssuer);
    }
    let auds: Vec<&str> = match c.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !auds.iter().any(|a| client_ids.iter().any(|id| id == a)) {
        return Err(IdTokenError::BadAudience);
    }
    if auds.len() > 1 {
        let azp = c.get("azp").and_then(Value::as_str).unwrap_or_default();
        if !client_ids.iter().any(|id| id == azp) {
            return Err(IdTokenError::BadAudience);
        }
    }
    if let Some((now, skew)) = time {
        let now_s = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let skew_s = skew.as_secs();
        let exp = claim_secs(c, "exp").ok_or(IdTokenError::Expired)?;
        if exp.saturating_add(skew_s) <= now_s {
            return Err(IdTokenError::Expired);
        }
        for claim in ["iat", "nbf"] {
            if let Some(t) = claim_secs(c, claim) {
                if t > now_s.saturating_add(skew_s) {
                    return Err(IdTokenError::NotYetValid);
                }
            }
        }
        if let Some(want) = expected_nonce {
            let got = c.get("nonce").and_then(Value::as_str).unwrap_or_default();
            if want.is_empty() || !ct_eq(got.as_bytes(), want.as_bytes()) {
                return Err(IdTokenError::NonceMismatch);
            }
        }
    }
    let sub = c
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .ok_or(IdTokenError::MissingSubject)?;
    Ok(GoogleIdentity {
        sub: sub.into(),
        email: c.get("email").and_then(Value::as_str).map(String::from),
        email_verified: c
            .get("email_verified")
            .map(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"))
            .unwrap_or(false),
    })
}

fn claim_secs(c: &Value, name: &str) -> Option<u64> {
    let v = c.get(name)?;
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

/// Split a compact JWT and check its header: returns (header part, kid, payload part,
/// signature part). Only `RS256` with a `kid` passes (never `none` or an HMAC).
fn read_header(id_token: &str) -> Result<(&str, String, &str, &str), IdTokenError> {
    if id_token.len() > MAX_ID_TOKEN_BYTES {
        return Err(IdTokenError::Malformed);
    }
    let mut parts = id_token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(IdTokenError::Malformed);
    };
    let header = json_part(h)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(IdTokenError::UnsupportedAlgorithm);
    }
    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .filter(|k| !k.is_empty())
        .ok_or(IdTokenError::UnknownKey)?
        .to_string();
    Ok((h, kid, p, s))
}

fn json_part(part: &str) -> Result<Value, IdTokenError> {
    let bytes = b64url_decode(part).ok_or(IdTokenError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| IdTokenError::Malformed)
}

/// Unpadded base64url decode (RFC 4648 section 5). Tolerates trailing `=`.
pub(crate) fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// One signed-in Google account on this device, for Drive sync.
///
/// * Sign-in is the desktop loopback flow with PKCE `S256` and an OpenID Connect `nonce`
///   ([`Self::sign_in_loopback`]). The ID token is verified (signature, issuer, audience,
///   expiry, nonce) and the grant must include [`DRIVE_APPDATA_SCOPE`] and a refresh token
///   before anything is stored.
/// * Tokens, the refresh token included, live in the supplied [`TokenStore`]; use
///   `SecretTokenStore` for the OS credential store. Refreshing goes through [`Resolver`], so it
///   runs under a cross-process lock and a revoked grant (`invalid_grant`) deletes the stored
///   sign-in.
/// * [`Self::access_token`], [`Self::rejected`], [`Self::identity`] and [`Self::generation`] are
///   the four things rightkit-sync's `DriveTokenSource` asks for; see the README for the glue.
///
/// Android Credential Manager yields an ID token only (no refresh token, no Drive access
/// token). Verify that token with [`GoogleIdVerifier::verify`] using the web client id and the
/// nonce the host sent; Drive authorization there comes from `AuthorizationClient` and is fed to
/// the sync backend by the host, not through this type.
pub struct GoogleSession {
    resolver: Resolver,
    verifier: GoogleIdVerifier,
    store: Arc<dyn TokenStore>,
    account: String,
    generation: AtomicU64,
}

impl fmt::Debug for GoogleSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GoogleSession")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl GoogleSession {
    /// `oauth` should come from [`crate::OAuthConfig::google_signin`]; `account` is the
    /// credential-store key for this app's Google grant (for example `google`).
    pub fn new(
        oauth: OAuthClient,
        verifier: GoogleIdVerifier,
        store: Arc<dyn TokenStore>,
        account: impl Into<String>,
    ) -> Self {
        let account = account.into();
        Self {
            resolver: Resolver::new(oauth, store.clone(), account.clone())
                .refresh_skew(ACCESS_REFRESH_SKEW),
            verifier,
            store,
            account,
            generation: AtomicU64::new(0),
        }
    }

    /// Directory for the cross-process refresh locks (default: a temp subdirectory).
    pub fn lock_dir(self, dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            resolver: self.resolver.lock_dir(dir),
            ..self
        }
    }

    /// Desktop (macOS, Windows) sign-in: loopback + PKCE S256 + `nonce`. `open_browser`
    /// receives the authorization URL. Prefer [`Self::sign_in_default_browser`] (opens the OS default browser). Nothing is
    /// stored unless the ID token verifies and the grant is complete.
    pub fn sign_in_loopback(
        &self,
        open_browser: impl FnOnce(&str),
        timeout: Duration,
    ) -> Result<GoogleIdentity, OAuthError> {
        let nonce = crate::random_urlsafe(32)?;
        let client = self.resolver.client();
        let flow = client.begin_loopback_with(&[("nonce", nonce.as_str())])?;
        open_browser(&flow.request.authorization_url);
        let tokens = client.finish_loopback(flow, timeout)?;
        self.accept(tokens, &nonce)
    }

    /// [`Self::sign_in_loopback`] through a [`crate::browser::BrowserOpener`]; use
    /// [`crate::browser::SystemBrowser`] so the user's default browser (and its existing Google
    /// session) is used. An embedded-webview opener is refused with
    /// [`OAuthError::EmbeddedWebviewRefused`].
    pub fn sign_in_default_browser(
        &self,
        opener: &dyn crate::browser::BrowserOpener,
        timeout: Duration,
    ) -> Result<GoogleIdentity, OAuthError> {
        let nonce = crate::random_urlsafe(32)?;
        let tokens = self.resolver.client().sign_in_with_browser_extra(
            opener,
            timeout,
            &[("nonce", nonce.as_str())],
        )?;
        self.accept(tokens, &nonce)
    }

    /// Verify a fresh token set, then persist it.
    fn accept(&self, tokens: TokenSet, nonce: &str) -> Result<GoogleIdentity, OAuthError> {
        // The user may untick individual scopes on the consent screen.
        let drive_granted = tokens
            .scope
            .as_deref()
            .is_some_and(|s| s.split_whitespace().any(|g| g == DRIVE_APPDATA_SCOPE));
        if !drive_granted {
            return Err(OAuthError::AccessDenied);
        }
        let id = tokens
            .id_token
            .as_ref()
            .ok_or_else(|| OAuthError::Callback("Google returned no ID token".into()))?;
        let identity = self.verifier.verify(id.expose(), Some(nonce))?;
        if tokens.refresh_token.is_none() {
            // Without a refresh token sync would stop after an hour.
            return Err(OAuthError::Callback(
                "Google returned no refresh token (offline access was not granted)".into(),
            ));
        }
        self.resolver.save(&tokens)?;
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(identity)
    }

    /// A valid access token, refreshed when it is within [`ACCESS_REFRESH_SKEW`] of expiry.
    /// `Ok(None)` means signed out (or the grant was revoked and the stored sign-in was
    /// removed). Network and storage failures are `Err` and leave the stored sign-in alone.
    pub fn access_token(&self) -> Result<Option<Secret>, OAuthError> {
        match self.resolver.resolve()? {
            Resolution::Token { token, .. } => Ok(Some(token)),
            Resolution::NeedsSignIn { .. } => Ok(None),
        }
    }

    /// Google answered 401 for `token`: mark the stored access token spent so the next
    /// [`Self::access_token`] refreshes. A different, newer token is left alone.
    pub fn rejected(&self, token: &str) -> Result<(), OAuthError> {
        if let Some(mut tokens) = self.store.load(&self.account)? {
            if tokens.access_token() == token {
                tokens.expires_in = Some(Duration::ZERO);
                self.store.save(&self.account, &tokens)?;
            }
        }
        Ok(())
    }

    /// The signed-in account, read back from the stored ID token (see
    /// [`GoogleIdVerifier::read_trusted`]); `None` when signed out. Use `sub` for the
    /// sync account scope.
    pub fn identity(&self) -> Result<Option<GoogleIdentity>, OAuthError> {
        let Some(tokens) = self.store.load(&self.account)? else {
            return Ok(None);
        };
        match tokens.id_token {
            Some(id) => Ok(Some(self.verifier.read_trusted(id.expose())?)),
            None => Ok(None),
        }
    }

    /// Bumped on every sign-in and sign-out in this process.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Delete the stored sign-in. This is local only: the grant stays on the Google account
    /// until the user revokes it at <https://myaccount.google.com/permissions>.
    pub fn sign_out(&self) -> Result<(), OAuthError> {
        self.resolver.sign_out()?;
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
