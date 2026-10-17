//! HMAC approval tokens: mint and verify a time-bound approval for a purpose and subject.
//!
//! The MAC is HMAC-SHA256 over a length-prefixed encoding of the version, purpose, subject,
//! issued-at and expiry (Unix seconds). Verification compares in constant time. Errors never
//! carry the key or MAC material, and nothing here formats the key.

use crate::{base64url_decode, base64url_encode, ct_eq, hmac_sha256};

const VERSION: &str = "v1";

/// Why a token did not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalError {
    Malformed,
    BadSignature,
    Expired,
    NotYetValid,
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed approval token",
            Self::BadSignature => "approval token signature mismatch",
            Self::Expired => "approval token expired",
            Self::NotYetValid => "approval token not yet valid",
        })
    }
}

impl std::error::Error for ApprovalError {}

fn encode(purpose: &str, subject: &str, issued: u64, expires: u64) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"rightkit-approval-");
    out.extend_from_slice(VERSION.as_bytes());
    out.push(0);
    for field in [purpose.as_bytes(), subject.as_bytes()] {
        out.extend_from_slice(&(field.len() as u64).to_be_bytes());
        out.extend_from_slice(field);
    }
    out.extend_from_slice(&issued.to_be_bytes());
    out.extend_from_slice(&expires.to_be_bytes());
    out
}

/// Mint a token valid from `now` for `ttl_secs` seconds (Unix seconds).
pub fn mint(key: &[u8], purpose: &str, subject: &str, now: u64, ttl_secs: u64) -> String {
    let expires = now.saturating_add(ttl_secs);
    let mac = hmac_sha256(key, &encode(purpose, subject, now, expires));
    format!("{VERSION}.{now}.{expires}.{}", base64url_encode(&mac))
}

/// Parse a timestamp segment, accepting only its canonical decimal encoding: no sign,
/// leading zeros, or whitespace.
fn parse_canonical_u64(seg: &str) -> Result<u64, ApprovalError> {
    let v: u64 = seg.parse().map_err(|_| ApprovalError::Malformed)?;
    if v.to_string() != seg {
        return Err(ApprovalError::Malformed);
    }
    Ok(v)
}

/// Verify a token for `purpose` and `subject` at `now` (Unix seconds).
pub fn verify(
    key: &[u8],
    purpose: &str,
    subject: &str,
    token: &str,
    now: u64,
) -> Result<(), ApprovalError> {
    let mut parts = token.split('.');
    let (v, issued, expires, mac) = match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some(v), Some(i), Some(e), Some(m), None) => (v, i, e, m),
        _ => return Err(ApprovalError::Malformed),
    };
    if v != VERSION {
        return Err(ApprovalError::Malformed);
    }
    let issued = parse_canonical_u64(issued)?;
    let expires = parse_canonical_u64(expires)?;
    let mac = base64url_decode(mac).map_err(|_| ApprovalError::Malformed)?;
    let expected = hmac_sha256(key, &encode(purpose, subject, issued, expires));
    if !ct_eq(&expected, &mac) {
        return Err(ApprovalError::BadSignature);
    }
    if now < issued {
        return Err(ApprovalError::NotYetValid);
    }
    if now >= expires {
        return Err(ApprovalError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"app-held-secret";

    #[test]
    fn round_trip() {
        let t = mint(KEY, "publish", "post-1", 1000, 60);
        assert_eq!(verify(KEY, "publish", "post-1", &t, 1000), Ok(()));
        assert_eq!(verify(KEY, "publish", "post-1", &t, 1059), Ok(()));
    }

    #[test]
    fn rejects_expiry_and_early() {
        let t = mint(KEY, "publish", "post-1", 1000, 60);
        assert_eq!(
            verify(KEY, "publish", "post-1", &t, 1060),
            Err(ApprovalError::Expired)
        );
        assert_eq!(
            verify(KEY, "publish", "post-1", &t, 999),
            Err(ApprovalError::NotYetValid)
        );
    }

    #[test]
    fn rejects_wrong_binding_and_key() {
        let t = mint(KEY, "publish", "post-1", 1000, 60);
        for (k, p, s) in [
            (KEY, "delete", "post-1"),
            (KEY, "publish", "post-2"),
            (&b"other"[..], "publish", "post-1"),
        ] {
            assert_eq!(verify(k, p, s, &t, 1001), Err(ApprovalError::BadSignature));
        }
    }

    #[test]
    fn rejects_non_canonical_timestamp_segments() {
        let t = mint(KEY, "publish", "post-1", 1000, 60);
        for alt in [
            "v1.01000.1060.",
            "v1.+1000.1060.",
            "v1.1000.+1060.",
            "v1.1000.01060.",
            "v1.-5.1060.",
            "v1.1000.-5.",
            "v1..1060.",
            "v1.1000..",
            "v1. 1000.1060.",
        ] {
            let mac = t.rsplit('.').next().unwrap();
            let tok = format!("{alt}{mac}");
            assert_eq!(
                verify(KEY, "publish", "post-1", &tok, 1001),
                Err(ApprovalError::Malformed),
                "{tok}"
            );
        }
        assert_eq!(verify(KEY, "publish", "post-1", &t, 1001), Ok(()));
    }

    #[test]
    fn rejects_tampered_times_and_malformed() {
        let t = mint(KEY, "publish", "post-1", 1000, 60);
        let tampered = t.replacen(".1060.", ".9999.", 1);
        assert_eq!(
            verify(KEY, "publish", "post-1", &tampered, 1001),
            Err(ApprovalError::BadSignature)
        );
        for bad in [
            "",
            "v1.1.2",
            "v2.1.2.AA",
            "v1.x.2.AA",
            "v1.1.2.AA.AA",
            "v1.1.2.!!",
        ] {
            assert_eq!(
                verify(KEY, "publish", "post-1", bad, 1),
                Err(ApprovalError::Malformed)
            );
        }
    }

    #[test]
    fn field_boundaries_are_unambiguous() {
        let t = mint(KEY, "ab", "c", 1, 10);
        assert_eq!(
            verify(KEY, "a", "bc", &t, 2),
            Err(ApprovalError::BadSignature)
        );
    }
}
