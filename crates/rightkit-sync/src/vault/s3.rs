//! [`ConditionalStore`] over the S3 REST API with Signature V4 (feature `vault-s3`).
//!
//! Works with Cloudflare R2 (`https://<account id>.r2.cloudflarestorage.com`, region `auto`) and with
//! any S3-compatible store that documents conditional `PutObject`: R2 documents `If-Match` and
//! `If-None-Match` on `PutObject` and strong consistency; AWS S3 documents both as well. The request
//! shapes are path-style `GET`/`PUT` on `{endpoint}/{bucket}/{key}`; the conditions go in the
//! `If-None-Match: *` and `If-Match: <etag>` headers, and `412 Precondition Failed` (or `409
//! Conflict`, which S3 answers for a conditional write racing another one) is
//! [`PutResult::PreconditionFailed`].
//!
//! The HTTP client is the caller's ([`rightkit_http::Client`]: HTTPS only, exact host). It is used
//! without retries on writes: a replayed conditional write whose first attempt landed would be
//! refused and look like a lost race, so a transport failure is surfaced as a transient error and
//! the vault reads the head back instead.
//!
//! A `Credentials` value holds an access key id and its secret; the secret is zeroed on drop and
//! never printed. Use a key scoped to the one bucket.

use rightkit_http::{Client, ErrorClass, HttpError, Request, Response, RetryPolicy};
use ring::{digest, hmac};
use zeroize::Zeroizing;

use super::{hex, ConditionalStore, PutResult, StoredObject};
use crate::error::{SyncError, SyncResult};

const NETWORK_RETRY_SECS: f64 = 15.0;
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// An S3 access key. The secret never appears in `Debug` output.
#[derive(Clone)]
pub struct Credentials {
    access_key_id: String,
    secret_access_key: Zeroizing<String>,
}

impl Credentials {
    pub fn new(access_key_id: impl Into<String>, secret_access_key: impl Into<String>) -> Self {
        Self {
            access_key_id: access_key_id.into(),
            secret_access_key: Zeroizing::new(secret_access_key.into()),
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .finish()
    }
}

/// Where the bucket is.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// `https://host` with no path (R2: `https://<account id>.r2.cloudflarestorage.com`).
    pub endpoint: String,
    pub bucket: String,
    /// The signing region: `auto` for R2, the bucket's region for S3.
    pub region: String,
}

/// A [`ConditionalStore`] on an S3-compatible bucket.
pub struct S3ConditionalStore {
    client: Client,
    endpoint: String,
    host: String,
    bucket: String,
    region: String,
    credentials: Credentials,
}

impl std::fmt::Debug for S3ConditionalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3ConditionalStore")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .finish()
    }
}

fn bad_config(why: &str) -> SyncError {
    SyncError::Unavailable(format!("S3 store configuration: {why}"))
}

impl S3ConditionalStore {
    pub fn new(client: Client, config: S3Config, credentials: Credentials) -> SyncResult<Self> {
        let endpoint = config.endpoint.trim_end_matches('/').to_string();
        let Some(host) = endpoint.strip_prefix("https://") else {
            return Err(bad_config("the endpoint must be https://host"));
        };
        if host.is_empty() || host.contains(['/', '?', '#', '@', ' ']) {
            return Err(bad_config(
                "the endpoint must be a bare host, without a path",
            ));
        }
        let bucket_ok = (3..=63).contains(&config.bucket.len())
            && config
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'));
        if !bucket_ok {
            return Err(bad_config("the bucket name is not a valid S3 bucket name"));
        }
        if config.region.is_empty() || credentials.access_key_id.is_empty() {
            return Err(bad_config("region and access key id are required"));
        }
        Ok(Self {
            client,
            host: host.to_string(),
            endpoint,
            bucket: config.bucket,
            region: config.region,
            credentials,
        })
    }

    /// A signed request for `method` on `key` with `payload`, plus the unsigned-by-default
    /// conditional headers in `conditions` (signed too, so a proxy cannot drop them).
    fn signed(
        &self,
        method: rightkit_http::Method,
        key: &str,
        payload: &[u8],
        conditions: &[(&str, &str)],
        now: i64,
    ) -> Request {
        let path = format!("/{}/{}", self.bucket, encode_path(key));
        let (date, stamp) = amz_times(now);
        let payload_hash = hex(digest::digest(&digest::SHA256, payload).as_ref());
        let mut headers: Vec<(String, String)> = vec![
            ("host".to_string(), self.host.clone()),
            ("x-amz-content-sha256".to_string(), payload_hash.clone()),
            ("x-amz-date".to_string(), stamp.clone()),
        ];
        for (name, value) in conditions {
            headers.push((name.to_ascii_lowercase(), (*value).to_string()));
        }
        headers.sort_by(|a, b| a.0.cmp(&b.0));
        let canonical_headers: String = headers
            .iter()
            .map(|(name, value)| format!("{name}:{}\n", value.trim()))
            .collect();
        let signed_headers = headers
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let canonical_request = format!(
            "{}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
            method.as_str()
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let to_sign = format!(
            "{ALGORITHM}\n{stamp}\n{scope}\n{}",
            hex(digest::digest(&digest::SHA256, canonical_request.as_bytes()).as_ref())
        );
        let signature =
            hex(hmac_sha256(self.signing_key(&date).as_ref(), to_sign.as_bytes()).as_ref());
        let authorization = format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.credentials.access_key_id
        );
        let mut request = Request::new(method, format!("{}{path}", self.endpoint))
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", stamp)
            .secret_header("authorization", authorization)
            .redact_value(signature);
        for (name, value) in conditions {
            request = request.header(*name, *value);
        }
        if !payload.is_empty() || matches!(method, rightkit_http::Method::Put) {
            request = request.body(payload.to_vec());
        }
        // A replayed conditional write is indistinguishable from a lost race; the vault reads back.
        request.retry = Some(RetryPolicy::none());
        request
    }

    fn signing_key(&self, date: &str) -> hmac::Tag {
        let seed = Zeroizing::new(format!(
            "AWS4{}",
            self.credentials.secret_access_key.as_str()
        ));
        let k_date = hmac_sha256(seed.as_bytes(), date.as_bytes());
        let k_region = hmac_sha256(k_date.as_ref(), self.region.as_bytes());
        let k_service = hmac_sha256(k_region.as_ref(), b"s3");
        hmac_sha256(k_service.as_ref(), b"aws4_request")
    }

    fn send(&self, request: &Request) -> SyncResult<Response> {
        self.client
            .send_any(request)
            .map_err(|error| map_http_error(&error))
    }

    fn put(&self, key: &str, body: &[u8], conditions: &[(&str, &str)]) -> SyncResult<PutResult> {
        let request = self.signed(
            rightkit_http::Method::Put,
            key,
            body,
            conditions,
            now_unix()?,
        );
        let response = self.send(&request)?;
        match response.status {
            200..=299 => match response.header("etag") {
                Some(etag) if !etag.is_empty() => Ok(PutResult::Stored {
                    etag: etag.to_string(),
                }),
                // A write that succeeded without telling us its version: the vault cannot trust
                // it, so it is an unknown outcome and the head is read back.
                _ => Err(SyncError::Transient(
                    "the store accepted the write but returned no ETag".into(),
                )),
            },
            409 | 412 => Ok(PutResult::PreconditionFailed),
            _ => Err(error_from_status(&response)),
        }
    }
}

impl ConditionalStore for S3ConditionalStore {
    fn get(&self, key: &str) -> SyncResult<Option<StoredObject>> {
        let request = self.signed(rightkit_http::Method::Get, key, &[], &[], now_unix()?);
        let response = self.send(&request)?;
        match response.status {
            200 => match response.header("etag") {
                Some(etag) if !etag.is_empty() => Ok(Some(StoredObject {
                    etag: etag.to_string(),
                    body: response.body,
                })),
                _ => Err(SyncError::Transient("the store returned no ETag".into())),
            },
            404 => Ok(None),
            _ => Err(error_from_status(&response)),
        }
    }

    fn put_if_absent(&self, key: &str, body: &[u8]) -> SyncResult<PutResult> {
        self.put(key, body, &[("if-none-match", "*")])
    }

    fn put_if_match(&self, key: &str, body: &[u8], etag: &str) -> SyncResult<PutResult> {
        self.put(key, body, &[("if-match", etag)])
    }
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> hmac::Tag {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
}

/// Percent-encode a key for the canonical URI: every byte but the unreserved set, `/` kept.
fn encode_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn now_unix() -> SyncResult<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .ok_or_else(|| SyncError::Unavailable("the device clock is before the Unix epoch".into()))
}

/// `(YYYYMMDD, YYYYMMDD'T'HHMMSS'Z')` of a Unix time in UTC.
fn amz_times(unix: i64) -> (String, String) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    let date = format!("{year:04}{month:02}{day:02}");
    let stamp = format!(
        "{date}T{:02}{:02}{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    );
    (date, stamp)
}

fn map_http_error(error: &HttpError) -> SyncError {
    match error.class() {
        ErrorClass::Timeout | ErrorClass::Transport => SyncError::RetryAfter {
            secs: NETWORK_RETRY_SECS,
            detail: error.to_string(),
        },
        ErrorClass::AddressDenied | ErrorClass::Invalid => {
            SyncError::Unavailable(error.to_string())
        }
        _ => SyncError::Transient(error.to_string()),
    }
}

fn error_from_status(response: &Response) -> SyncError {
    let detail = format!("HTTP {}", response.status);
    match response.status {
        429 | 503 => SyncError::RetryAfter {
            secs: response
                .header("retry-after")
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|secs| secs.is_finite() && *secs > 0.0)
                .unwrap_or(NETWORK_RETRY_SECS),
            detail,
        },
        401 | 403 => SyncError::Unavailable(format!("{detail}: the store refused the credentials")),
        400..=499 => SyncError::Unavailable(detail),
        _ => SyncError::Transient(detail),
    }
}
