//! The entitlement-service client.
//!
//! `GET  {base}/v1/vault/wrapping-key`  fetch the server half.
//! `POST {base}/v1/vault/rotate`        re-initialise the vault (used only by `Vault::reset`).
//!
//! Both send `Authorization: Bearer <google id token>` and `X-Right-App: <app>`. The response
//! shape is ASSUMED pending bogusyogi/rightsites#81:
//! `{"key_id": "...", "key": "<base64 32 bytes>", "previous": [{"key_id": "...", "key": "..."}]}`.
//!
//! Status mapping: 401 `Unauthorized`, 403 `Denied` (not entitled), 410 `Revoked` (vault
//! erased), transport failure or timeout `Offline`, anything else `Service`.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rightkit_http::{Client, HttpError, Request};
use serde::{Deserialize, Deserializer};
use zeroize::{Zeroize, Zeroizing};

use crate::error::{Error, Result};
use crate::keys::{valid_key_id, ServerKey, ServerKeySource, ServerKeys, KEY_LEN, MAX_PREVIOUS};

/// A response body larger than this is refused (a bundle is a few hundred bytes).
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// A string that is zeroed on drop, so the base64 key text in a decoded response does not
/// linger as a plain `String`.
pub(crate) struct ZStr(pub(crate) Zeroizing<String>);

impl<'de> Deserialize<'de> for ZStr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = ZStr;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> std::result::Result<ZStr, E> {
                Ok(ZStr(Zeroizing::new(s.to_owned())))
            }
        }
        d.deserialize_str(V)
    }
}

#[derive(Deserialize)]
struct WireKey {
    key_id: String,
    key: ZStr,
}

#[derive(Deserialize)]
struct WireBundle {
    key_id: String,
    key: ZStr,
    #[serde(default)]
    previous: Vec<WireKey>,
}

fn decode_key(id: &str, b64: &ZStr) -> Result<ServerKey> {
    valid_key_id(id)?;
    let raw = Zeroizing::new(
        B64.decode(b64.0.as_bytes())
            .map_err(|_| Error::Service("wrapping key is not base64".into()))?,
    );
    let arr: [u8; KEY_LEN] = raw
        .as_slice()
        .try_into()
        .map_err(|_| Error::Service("wrapping key is not 32 bytes".into()))?;
    Ok(ServerKey::new(id, arr))
}

/// Parse a service response body into a bundle and zero the body either way. The error text
/// never quotes the body.
pub(crate) fn parse_bundle(body: &mut Vec<u8>) -> Result<ServerKeys> {
    let parsed = serde_json::from_slice::<WireBundle>(body);
    body.zeroize();
    let wire = parsed.map_err(|_| Error::Service("unreadable response".into()))?;
    if wire.previous.len() > MAX_PREVIOUS {
        return Err(Error::Service("too many previous keys".into()));
    }
    let current = decode_key(&wire.key_id, &wire.key)?;
    let mut previous = Vec::with_capacity(wire.previous.len());
    for p in &wire.previous {
        previous.push(decode_key(&p.key_id, &p.key)?);
    }
    Ok(ServerKeys { current, previous })
}

pub(crate) fn status_error(status: u16) -> Error {
    match status {
        401 => Error::Unauthorized,
        403 => Error::Denied,
        410 => Error::Revoked,
        s => Error::Service(format!("status {s}")),
    }
}

fn transport_error(e: &HttpError) -> Error {
    match e {
        HttpError::Transport { .. } | HttpError::Timeout { .. } => Error::Offline,
        // Policy refusals (address policy, https required, invalid request) are configuration
        // faults, not connectivity: do not let them look like "offline".
        _ => Error::Service("request refused by the http client".into()),
    }
}

/// [`ServerKeySource`] over `rightkit-http`. Build the client with
/// `ClientConfig::first_party(vec![host])` and `https_only(true)`: the ID token and the key
/// travel in these requests.
pub struct HttpServerKeySource {
    client: Arc<Client>,
    base_url: String,
    app: String,
}

impl HttpServerKeySource {
    pub fn new(client: Arc<Client>, base_url: impl Into<String>, app: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            app: app.into(),
        }
    }

    fn call(&self, req: Request) -> Result<ServerKeys> {
        let mut resp = self
            .client
            .send_any(&req)
            .map_err(|e| transport_error(&e))?;
        if !(200..300).contains(&resp.status) {
            resp.body.zeroize();
            return Err(status_error(resp.status));
        }
        parse_bundle(&mut resp.body)
    }
}

impl ServerKeySource for HttpServerKeySource {
    fn fetch(&self, id_token: &str) -> Result<ServerKeys> {
        self.call(
            Request::get(format!("{}/v1/vault/wrapping-key", self.base_url))
                .bearer(id_token)
                .header("X-Right-App", self.app.clone())
                .max_response_bytes(MAX_RESPONSE_BYTES),
        )
    }

    fn reset(&self, id_token: &str) -> Result<ServerKeys> {
        self.call(
            Request::post(format!("{}/v1/vault/rotate", self.base_url))
                .bearer(id_token)
                .header("X-Right-App", self.app.clone())
                .max_response_bytes(MAX_RESPONSE_BYTES),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(b: u8) -> String {
        B64.encode([b; 32])
    }

    #[test]
    fn parses_bundle_and_zeroes_body() {
        let mut body = format!(
            r#"{{"key_id":"k2","key":"{}","previous":[{{"key_id":"k1","key":"{}"}}]}}"#,
            b64(2),
            b64(1)
        )
        .into_bytes();
        let keys = parse_bundle(&mut body).unwrap();
        assert!(body.is_empty() || body.iter().all(|b| *b == 0));
        assert_eq!(keys.current.id(), "k2");
        assert_eq!(keys.current.bytes(), &[2; 32]);
        assert_eq!(keys.previous[0].id(), "k1");
    }

    #[test]
    fn previous_is_optional() {
        let mut body = format!(r#"{{"key_id":"k1","key":"{}"}}"#, b64(1)).into_bytes();
        assert!(parse_bundle(&mut body).unwrap().previous.is_empty());
    }

    #[test]
    fn bad_responses_are_service_errors_that_do_not_echo_the_body() {
        let mut short = format!(r#"{{"key_id":"k1","key":"{}"}}"#, B64.encode([1u8; 16])).into_bytes();
        assert!(matches!(parse_bundle(&mut short), Err(Error::Service(_))));
        let mut not_b64 = br#"{"key_id":"k1","key":"!!not-base64!!"}"#.to_vec();
        assert!(matches!(parse_bundle(&mut not_b64), Err(Error::Service(_))));
        let mut garbage = br#"{"key_id": 7, "SENTINEL-SECRET": 1}"#.to_vec();
        match parse_bundle(&mut garbage) {
            Err(e) => assert!(!e.to_string().contains("SENTINEL-SECRET")),
            Ok(_) => panic!("accepted garbage"),
        }
        let mut empty_id = format!(r#"{{"key_id":"","key":"{}"}}"#, b64(1)).into_bytes();
        assert!(matches!(parse_bundle(&mut empty_id), Err(Error::Service(_))));
    }

    #[test]
    fn status_mapping() {
        assert!(matches!(status_error(401), Error::Unauthorized));
        assert!(matches!(status_error(403), Error::Denied));
        assert!(matches!(status_error(410), Error::Revoked));
        assert!(matches!(status_error(500), Error::Service(_)));
        assert!(matches!(status_error(404), Error::Service(_)));
    }
}
