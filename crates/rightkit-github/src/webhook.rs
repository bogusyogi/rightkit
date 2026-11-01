use super::{GitHubAppError, GitHubDenial};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// GitHub webhook payload hard cap from GitHub's delivery contract.
pub const MAX_WEBHOOK_BODY_BYTES: usize = 25 * 1024 * 1024;
const MAX_DELIVERY_ID_CHARS: usize = 256;
const MAX_EVENT_CHARS: usize = 128;

/// Header values needed before parsing an untrusted webhook payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubWebhookHeaders {
    pub delivery_id: String,
    pub event: String,
    pub hook_id: Option<u64>,
    pub signature_256: String,
}

impl GitHubWebhookHeaders {
    pub fn new(
        delivery_id: impl Into<String>,
        event: impl Into<String>,
        hook_id: Option<u64>,
        signature_256: impl Into<String>,
    ) -> Result<Self, GitHubAppError> {
        let headers = Self {
            delivery_id: delivery_id.into(),
            event: event.into(),
            hook_id,
            signature_256: signature_256.into(),
        };
        headers.validate()?;
        Ok(headers)
    }

    fn validate(&self) -> Result<(), GitHubAppError> {
        let id_valid = !self.delivery_id.is_empty()
            && self.delivery_id.chars().count() <= MAX_DELIVERY_ID_CHARS
            && self.delivery_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if !id_valid {
            return Err(GitHubAppError::denied(if self.delivery_id.is_empty() {
                GitHubDenial::MissingDeliveryId
            } else {
                GitHubDenial::InvalidDeliveryId
            }));
        }
        let event_valid = !self.event.is_empty()
            && self.event.chars().count() <= MAX_EVENT_CHARS
            && self
                .event
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !byte.is_ascii_whitespace());
        if !event_valid {
            return Err(GitHubAppError::denied(if self.event.is_empty() {
                GitHubDenial::MissingEvent
            } else {
                GitHubDenial::InvalidEvent
            }));
        }
        Ok(())
    }
}

/// Identity extracted from a verified payload and bound to an installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookIdentity {
    pub app_id: u64,
    pub installation_id: Option<u64>,
    pub account_id: Option<u64>,
    pub repository_id: Option<u64>,
    pub actor_id: Option<u64>,
    pub actor_login: Option<String>,
}

impl WebhookIdentity {
    fn validate(&self, expected_app_id: u64) -> Result<(), GitHubAppError> {
        if self.app_id == 0 || self.app_id != expected_app_id {
            return Err(GitHubAppError::denied(GitHubDenial::InvalidWebhookIdentity));
        }
        let Some(installation_id) = self.installation_id else {
            return Err(GitHubAppError::denied(
                GitHubDenial::MissingInstallationIdentity,
            ));
        };
        if installation_id == 0
            || self.account_id.is_some_and(|id| id == 0)
            || self.repository_id.is_some_and(|id| id == 0)
            || self.actor_id.is_some_and(|id| id == 0)
            || self
                .actor_login
                .as_deref()
                .is_some_and(|login| login.trim().is_empty())
        {
            return Err(GitHubAppError::denied(GitHubDenial::InvalidWebhookIdentity));
        }
        Ok(())
    }
}

/// Verified, replay-admitted webhook metadata. Raw payload is intentionally
/// omitted; downstream code must retain its own bounded copy if needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedWebhook {
    pub delivery_id: String,
    pub event: String,
    pub hook_id: Option<u64>,
    pub payload_sha256: String,
    pub identity: WebhookIdentity,
}

/// Bounded replay window. Live entries are never evicted to admit new input.
#[derive(Debug, Clone)]
pub struct ReplayGuard {
    capacity: usize,
    ttl_seconds: u64,
    seen: BTreeMap<String, u64>,
}

impl ReplayGuard {
    pub fn new(capacity: usize, ttl_seconds: u64) -> Result<Self, GitHubAppError> {
        if capacity == 0 || ttl_seconds == 0 {
            return Err(GitHubAppError::InvalidBinding(
                "replay capacity and TTL must be non-zero".to_string(),
            ));
        }
        Ok(Self {
            capacity,
            ttl_seconds,
            seen: BTreeMap::new(),
        })
    }

    pub fn admit(&mut self, delivery_id: &str, now_unix: u64) -> Result<(), GitHubAppError> {
        let id_valid = !delivery_id.is_empty()
            && delivery_id.chars().count() <= MAX_DELIVERY_ID_CHARS
            && delivery_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if !id_valid {
            return Err(GitHubAppError::denied(if delivery_id.is_empty() {
                GitHubDenial::MissingDeliveryId
            } else {
                GitHubDenial::InvalidDeliveryId
            }));
        }
        let ttl_seconds = self.ttl_seconds;
        self.seen
            .retain(|_, received_at| now_unix.saturating_sub(*received_at) <= ttl_seconds);
        if self.seen.contains_key(delivery_id) {
            return Err(GitHubAppError::denied(GitHubDenial::Replay {
                delivery_id: delivery_id.to_string(),
            }));
        }
        if self.seen.len() >= self.capacity {
            return Err(GitHubAppError::denied(GitHubDenial::ReplayWindowFull));
        }
        self.seen.insert(delivery_id.to_string(), now_unix);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Signature and identity verifier. Secret is not serializable or printable.
pub struct WebhookVerifier {
    app_id: u64,
    secret: Vec<u8>,
}

impl std::fmt::Debug for WebhookVerifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebhookVerifier")
            .field("app_id", &self.app_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl WebhookVerifier {
    pub fn new(app_id: u64, secret: impl Into<Vec<u8>>) -> Result<Self, GitHubAppError> {
        let secret = secret.into();
        if app_id == 0 || secret.len() < 16 {
            return Err(GitHubAppError::InvalidBinding(
                "app_id must be non-zero and webhook secret must be at least 16 bytes".to_string(),
            ));
        }
        Ok(Self { app_id, secret })
    }

    pub fn verify(
        &self,
        headers: &GitHubWebhookHeaders,
        body: &[u8],
        identity: WebhookIdentity,
        replay: &mut ReplayGuard,
        now_unix: u64,
    ) -> Result<VerifiedWebhook, GitHubAppError> {
        if body.len() > MAX_WEBHOOK_BODY_BYTES {
            return Err(GitHubAppError::denied(GitHubDenial::PayloadTooLarge));
        }
        headers.validate()?;
        let Some(provided) = decode_signature(&headers.signature_256) else {
            return Err(GitHubAppError::denied(
                if headers.signature_256.trim().is_empty() {
                    GitHubDenial::MissingSignature
                } else {
                    GitHubDenial::InvalidSignature
                },
            ));
        };
        let expected = rightkit_crypto::hmac_sha256(&self.secret, body);
        if !rightkit_crypto::ct_eq(&provided, &expected) {
            return Err(GitHubAppError::denied(GitHubDenial::InvalidSignature));
        }
        identity.validate(self.app_id)?;
        if identity.installation_id.is_none() {
            return Err(GitHubAppError::denied(
                GitHubDenial::MissingInstallationIdentity,
            ));
        }
        replay.admit(&headers.delivery_id, now_unix)?;
        Ok(VerifiedWebhook {
            delivery_id: headers.delivery_id.clone(),
            event: headers.event.clone(),
            hook_id: headers.hook_id,
            payload_sha256: rightkit_fs::digest::sha256_bytes(body),
            identity,
        })
    }
}

/// Parses a `sha256=<64 hex>` header into the 32 MAC bytes (hex of either case; the hex decode is
/// `rightkit-fs`'s `digest::unhex`).
fn decode_signature(raw: &str) -> Option<[u8; 32]> {
    let raw = raw.trim().strip_prefix("sha256=")?;
    rightkit_fs::digest::unhex(raw)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_crypto::{hex_encode, hmac_sha256};

    fn identity() -> WebhookIdentity {
        WebhookIdentity {
            app_id: 7,
            installation_id: Some(9),
            account_id: Some(11),
            repository_id: Some(13),
            actor_id: Some(17),
            actor_login: Some("alice".to_string()),
        }
    }

    #[test]
    fn verifies_github_hmac_vector_and_binds_identity() {
        let secret = b"It's a Secret to Everybody";
        let body = b"Hello, World!";
        let signature = hex_encode(&hmac_sha256(secret, body));
        assert_eq!(
            signature,
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
        let headers = GitHubWebhookHeaders::new(
            "delivery-1",
            "issues",
            Some(3),
            format!("sha256={signature}"),
        )
        .unwrap();
        let verifier = WebhookVerifier::new(7, secret.to_vec()).unwrap();
        let mut replay = ReplayGuard::new(8, 300).unwrap();
        let verified = verifier
            .verify(&headers, body, identity(), &mut replay, 100)
            .unwrap();
        assert_eq!(verified.identity.installation_id, Some(9));
        assert_eq!(replay.len(), 1);
    }

    #[test]
    fn replay_window_rejects_duplicates_without_live_eviction() {
        let mut replay = ReplayGuard::new(1, 300).unwrap();
        replay.admit("one", 100).unwrap();
        assert!(matches!(
            replay.admit("one", 101),
            Err(GitHubAppError::Denied(GitHubDenial::Replay { .. }))
        ));
        assert!(matches!(
            replay.admit("two", 101),
            Err(GitHubAppError::Denied(GitHubDenial::ReplayWindowFull))
        ));
        replay.admit("two", 401).unwrap();
    }

    #[test]
    fn signature_and_installation_identity_fail_closed() {
        let verifier = WebhookVerifier::new(7, b"sixteen-byte-key".to_vec()).unwrap();
        let headers =
            GitHubWebhookHeaders::new("delivery-2", "issues", None, "sha256=bad").unwrap();
        let mut replay = ReplayGuard::new(8, 300).unwrap();
        assert!(matches!(
            verifier.verify(&headers, b"body", identity(), &mut replay, 100),
            Err(GitHubAppError::Denied(GitHubDenial::InvalidSignature))
        ));
        let signature = hex_encode(&hmac_sha256(b"sixteen-byte-key", b"body"));
        let headers =
            GitHubWebhookHeaders::new("delivery-3", "issues", None, format!("sha256={signature}"))
                .unwrap();
        let missing_installation = WebhookIdentity {
            installation_id: None,
            ..identity()
        };
        assert!(matches!(
            verifier.verify(&headers, b"body", missing_installation, &mut replay, 100),
            Err(GitHubAppError::Denied(
                GitHubDenial::MissingInstallationIdentity
            ))
        ));
    }

    #[test]
    fn hmac_and_hex_helpers_produce_the_published_vectors() {
        // RFC 4231 test case 2.
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex_encode(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            decode_signature(
                "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
            ),
            Some(mac)
        );
        // Either hex case decodes to the same bytes; surrounding whitespace is trimmed.
        assert_eq!(
            decode_signature(
                " sha256=5BDCC146BF60754E6A042426089575C75A003F089D2739839DEC58B964EC3843 "
            ),
            Some(mac)
        );
    }

    #[test]
    fn signature_decoder_rejects_malformed_headers() {
        let good = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        assert_eq!(decode_signature(good), None, "missing sha256= prefix");
        assert_eq!(decode_signature(&format!("sha1={good}")), None);
        assert_eq!(decode_signature("sha256="), None);
        assert_eq!(decode_signature("sha256=abcd"), None, "short");
        assert_eq!(decode_signature(&format!("sha256={good}00")), None, "long");
        assert_eq!(
            decode_signature(&format!("sha256={}", &good[..63])),
            None,
            "odd length"
        );
        assert_eq!(
            decode_signature(&format!("sha256=zz{}", &good[2..])),
            None,
            "non-hex"
        );
    }

    #[test]
    fn verified_webhook_reports_the_exact_payload_digest() {
        let secret = b"It's a Secret to Everybody";
        let body = b"Hello, World!";
        let signature = hex_encode(&hmac_sha256(secret, body));
        let headers = GitHubWebhookHeaders::new(
            "delivery-digest",
            "issues",
            Some(3),
            format!("sha256={signature}"),
        )
        .unwrap();
        let verifier = WebhookVerifier::new(7, secret.to_vec()).unwrap();
        let mut replay = ReplayGuard::new(8, 300).unwrap();
        let verified = verifier
            .verify(&headers, body, identity(), &mut replay, 100)
            .unwrap();
        assert_eq!(
            verified.payload_sha256,
            "dffd6021bb2bd5b0af676290809ec3a53191dd81c7f70a4b28688a362182986f"
        );
    }

    #[test]
    fn a_signature_that_differs_in_one_bit_is_refused() {
        let secret = b"sixteen-byte-key";
        let mut mac = hmac_sha256(secret, b"body");
        mac[31] ^= 1;
        let headers = GitHubWebhookHeaders::new(
            "delivery-flip",
            "issues",
            None,
            format!("sha256={}", hex_encode(&mac)),
        )
        .unwrap();
        let verifier = WebhookVerifier::new(7, secret.to_vec()).unwrap();
        let mut replay = ReplayGuard::new(8, 300).unwrap();
        assert!(matches!(
            verifier.verify(&headers, b"body", identity(), &mut replay, 100),
            Err(GitHubAppError::Denied(GitHubDenial::InvalidSignature))
        ));
    }
}
