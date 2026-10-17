//! Browser bearer-token device auth with a per-IP rate limit (Orthic-Labs/rightkit#379).
//!
//! Tokens are shown once at mint; the registry keeps only a domain-separated SHA-256. Every
//! attempt consumes a rate-limit token for its source IP before any comparison, and the
//! comparison walks every entry in constant time.

use crate::util::{ct_eq, hex, random_bytes, sha256};
use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    RateLimited {
        retry_after_ms: u64,
    },
    /// Unknown, malformed or revoked: deliberately indistinguishable.
    Invalid,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::RateLimited { retry_after_ms } => {
                write!(f, "rate limited; retry in {retry_after_ms} ms")
            }
            AuthError::Invalid => write!(f, "invalid bearer token"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Per-IP token bucket. IPv6 addresses share a bucket per /64; IPv4-mapped IPv6 addresses
/// share the bucket of their IPv4 address. Buckets that are not yet refilled are never evicted:
/// when the table is full of such buckets, new keys share one bounded overflow bucket.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    max_tracked: usize,
    buckets: HashMap<IpAddr, (f64, u64)>,
    overflow: (f64, u64),
}

fn bucket_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let mut o = v6.octets();
            for b in &mut o[8..] {
                *b = 0;
            }
            IpAddr::V6(o.into())
        }
    }
}

/// Refill `(tokens, last)` to `now_ms`, then take one token; the error is the wait in ms.
fn take(
    e: &mut (f64, u64),
    cap: f64,
    refill_per_sec: f64,
    now_ms: u64,
) -> std::result::Result<(), u64> {
    let elapsed = now_ms.saturating_sub(e.1) as f64 / 1000.0;
    e.0 = (e.0 + elapsed * refill_per_sec).min(cap);
    e.1 = e.1.max(now_ms);
    if e.0 >= 1.0 {
        e.0 -= 1.0;
        Ok(())
    } else {
        Err((((1.0 - e.0) / refill_per_sec) * 1000.0).ceil() as u64)
    }
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_per_sec: f64, max_tracked: usize) -> Self {
        let capacity = capacity.max(1) as f64;
        RateLimiter {
            capacity,
            refill_per_sec: refill_per_sec.max(0.001),
            max_tracked: max_tracked.max(1),
            buckets: HashMap::new(),
            overflow: (capacity, 0),
        }
    }

    /// Take one token for `ip`, or return the milliseconds until one is available.
    pub fn check(&mut self, ip: IpAddr, now_ms: u64) -> std::result::Result<(), u64> {
        let key = bucket_key(ip);
        let (cap, rate) = (self.capacity, self.refill_per_sec);
        if !self.buckets.contains_key(&key) && self.buckets.len() >= self.max_tracked {
            // Only a bucket whose refill would restore it to full carries no state: evicting
            // it grants nothing a fresh bucket would not.
            let restored = self
                .buckets
                .iter()
                .filter(|(_, v)| v.0 + now_ms.saturating_sub(v.1) as f64 / 1000.0 * rate >= cap)
                .min_by_key(|(_, v)| v.1)
                .map(|(k, _)| *k);
            match restored {
                Some(k) => {
                    self.buckets.remove(&k);
                }
                None => return take(&mut self.overflow, cap, rate, now_ms),
            }
        }
        let e = self.buckets.entry(key).or_insert((cap, now_ms));
        take(e, cap, rate, now_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    device_id: String,
    token_hash: String,
    created_unix: u64,
    revoked: bool,
}

/// Registry of per-device bearer tokens. Serialisable: it holds hashes only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BearerRegistry {
    entries: Vec<Entry>,
}

fn hash_token(token: &str) -> [u8; 32] {
    let mut d = b"rightkit-identity/bearer/v1\0".to_vec();
    d.extend_from_slice(token.as_bytes());
    sha256(&d)
}

impl BearerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a token for `device_id`, revoking any earlier active token for it. The returned
    /// string is the only copy.
    pub fn mint(&mut self, device_id: &str, now_unix: u64) -> Result<String> {
        self.revoke(device_id);
        let token = format!("rkb_{}", hex(&random_bytes::<32>()?));
        self.entries.push(Entry {
            device_id: device_id.to_string(),
            token_hash: hex(&hash_token(&token)),
            created_unix: now_unix,
            revoked: false,
        });
        Ok(token)
    }

    /// Revoke every active token of `device_id`. Returns whether one was active.
    pub fn revoke(&mut self, device_id: &str) -> bool {
        let mut any = false;
        for e in self
            .entries
            .iter_mut()
            .filter(|e| e.device_id == device_id && !e.revoked)
        {
            e.revoked = true;
            any = true;
        }
        any
    }

    pub fn active_devices(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|e| !e.revoked)
            .map(|e| e.device_id.as_str())
            .collect()
    }

    /// Rate-limit by `ip`, then match `token`. Returns the device id.
    pub fn authenticate(
        &self,
        token: &str,
        ip: IpAddr,
        limiter: &mut RateLimiter,
        now_ms: u64,
    ) -> std::result::Result<&str, AuthError> {
        limiter
            .check(ip, now_ms)
            .map_err(|retry_after_ms| AuthError::RateLimited { retry_after_ms })?;
        let presented = hex(&hash_token(token));
        let mut found: Option<&Entry> = None;
        for e in &self.entries {
            let hit = ct_eq(e.token_hash.as_bytes(), presented.as_bytes());
            if hit && !e.revoked {
                found = Some(e);
            }
        }
        found
            .map(|e| e.device_id.as_str())
            .ok_or(AuthError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn mint_authenticate_revoke() {
        let mut reg = BearerRegistry::new();
        let mut lim = RateLimiter::new(10, 1.0, 100);
        let t = reg.mint("phone", 1).unwrap();
        assert_eq!(
            reg.authenticate(&t, ip("10.0.0.1"), &mut lim, 0).unwrap(),
            "phone"
        );
        assert_eq!(
            reg.authenticate("rkb_nope", ip("10.0.0.1"), &mut lim, 0),
            Err(AuthError::Invalid)
        );
        let t2 = reg.mint("phone", 2).unwrap();
        assert_eq!(
            reg.authenticate(&t, ip("10.0.0.1"), &mut lim, 0),
            Err(AuthError::Invalid)
        );
        assert!(reg.authenticate(&t2, ip("10.0.0.1"), &mut lim, 0).is_ok());
        assert!(reg.revoke("phone"));
        assert_eq!(
            reg.authenticate(&t2, ip("10.0.0.1"), &mut lim, 0),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn rate_limit_per_ip_and_refill() {
        let reg = BearerRegistry::new();
        let mut lim = RateLimiter::new(2, 1.0, 100);
        let a = ip("192.0.2.1");
        assert_eq!(
            reg.authenticate("x", a, &mut lim, 0),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            reg.authenticate("x", a, &mut lim, 0),
            Err(AuthError::Invalid)
        );
        assert!(matches!(
            reg.authenticate("x", a, &mut lim, 0),
            Err(AuthError::RateLimited {
                retry_after_ms: 1000
            })
        ));
        assert_eq!(
            reg.authenticate("x", ip("192.0.2.2"), &mut lim, 0),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            reg.authenticate("x", a, &mut lim, 1000),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn exhausted_buckets_survive_capacity_churn() {
        let mut lim = RateLimiter::new(1, 0.001, 1);
        assert!(lim.check(ip("192.0.2.1"), 0).is_ok());
        // A second key cannot evict the exhausted bucket; it shares the overflow bucket.
        assert!(lim.check(ip("192.0.2.2"), 0).is_ok());
        assert!(lim.check(ip("192.0.2.3"), 0).is_err());
        // Returning to the first key does not yield a fresh burst.
        assert!(lim.check(ip("192.0.2.1"), 0).is_err());
        // After a full refill the idle bucket may be evicted for a new key.
        assert!(lim.check(ip("192.0.2.4"), 2_000_000).is_ok());
    }

    #[test]
    fn mapped_ipv4_uses_the_v4_bucket() {
        let mut lim = RateLimiter::new(1, 0.001, 10);
        assert!(lim.check(ip("::ffff:192.0.2.1"), 0).is_ok());
        assert!(lim.check(ip("192.0.2.1"), 0).is_err());
        assert!(lim.check(ip("::ffff:192.0.2.2"), 0).is_ok());
    }

    #[test]
    fn ipv6_shares_a_64() {
        let mut lim = RateLimiter::new(1, 0.001, 10);
        assert!(lim.check(ip("2001:db8::1"), 0).is_ok());
        assert!(lim.check(ip("2001:db8::2"), 0).is_err());
    }
}
