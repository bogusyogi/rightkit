//! The join token: one line the host owner copies to the computer that should join
//! (a `device join <token>` command), instead of typing a code across two screens.
//!
//! ```text
//! pulse-join:1:<host endpoint, 64 hex>:<pairing id, 32 hex>:<secret, 64 hex>[:<relay, % escaped>][:<addrs, % escaped>]
//! ```
//!
//! The endpoint is the host's iroh endpoint id (its identity key), the pairing id names the one
//! pairing session the host opened for this token, and the secret is 32 fresh random bytes. The
//! host keeps only [`super::pairing::join_verifier`] of the secret, in memory, for the life of
//! that session (ten minutes, [`super::PAIRING_TTL_MS`]); the joiner proves it knows the secret
//! with an HMAC over the confirmed pairing transcript (`pairing.rs`). The relay and the direct
//! addresses are only hints for the dial: they are never trusted, and the host key that signs
//! everything is the endpoint named here.
//!
//! The token is a bearer secret until it is used or expires: never log it, never put it in a URL.
//! `Debug` hides the secret.

use std::fmt;
use std::net::SocketAddr;

use rightkit_fs::digest::hex;

use super::LinkError;
use super::wire::{random, unhex};

/// The text every token starts with; the `1` is the token format version.
pub const JOIN_PREFIX: &str = "pulse-join:1:";
/// Longest token text read.
const MAX_TOKEN_CHARS: usize = 2048;
const MAX_RELAY_CHARS: usize = 256;
const MAX_ADDRS: usize = 8;

/// A parsed or freshly made join token.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinToken {
    pub endpoint: [u8; 32],
    pub pairing_id: [u8; 16],
    pub secret: [u8; 32],
    /// The host's relay, when it has one (`https` only).
    pub relay: Option<String>,
    /// The host's private-network addresses, to dial first.
    pub addrs: Vec<SocketAddr>,
}

impl fmt::Debug for JoinToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinToken")
            .field("endpoint", &hex(&self.endpoint))
            .field("pairing_id", &hex(&self.pairing_id))
            .field("secret", &"<redacted>")
            .field("relay", &self.relay)
            .field("addrs", &self.addrs)
            .finish()
    }
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let pair = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            at += 3;
        } else {
            out.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl JoinToken {
    /// A token for the pairing session `pairing_id` (hex, from the offer) of the host whose
    /// endpoint id is `endpoint`, with a fresh random secret.
    pub fn generate(
        endpoint: [u8; 32],
        pairing_id: &str,
        relay: Option<String>,
        addrs: Vec<SocketAddr>,
    ) -> Result<Self, LinkError> {
        let relay = relay.filter(|r| !r.is_empty()).filter(|r| {
            r.len() <= MAX_RELAY_CHARS
                && r.get(..8)
                    .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        });
        Ok(Self {
            endpoint,
            pairing_id: unhex::<16>(pairing_id)?,
            secret: random::<32>()?,
            relay,
            addrs: addrs.into_iter().take(MAX_ADDRS).collect(),
        })
    }

    /// [`Self::generate`] with a secret made earlier: the one whose hash the owner check signed in
    /// the token's grant (`circle::TokenGrant`).
    pub fn with_secret(
        endpoint: [u8; 32],
        pairing_id: &str,
        secret: [u8; 32],
        relay: Option<String>,
        addrs: Vec<SocketAddr>,
    ) -> Result<Self, LinkError> {
        let mut token = Self::generate(endpoint, pairing_id, relay, addrs)?;
        token.secret = secret;
        Ok(token)
    }

    /// The token as the one line the owner copies. A bearer secret: show it to the owner only.
    pub fn encode(&self) -> String {
        let mut out = format!(
            "{JOIN_PREFIX}{}:{}:{}",
            hex(&self.endpoint),
            hex(&self.pairing_id),
            hex(&self.secret)
        );
        let relay = self.relay.as_deref().map(percent_encode);
        let addrs = (!self.addrs.is_empty()).then(|| {
            percent_encode(
                &self
                    .addrs
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
        });
        match (relay, addrs) {
            (None, None) => {}
            (Some(relay), None) => {
                out.push(':');
                out.push_str(&relay);
            }
            (relay, Some(addrs)) => {
                out.push(':');
                out.push_str(&relay.unwrap_or_default());
                out.push(':');
                out.push_str(&addrs);
            }
        }
        out
    }

    /// Reads a token. Anything that is not exactly the format is `Malformed("join token")`; the
    /// refusal never echoes the text.
    pub fn parse(text: &str) -> Result<Self, LinkError> {
        let bad = || LinkError::Malformed("join token");
        let text = text.trim();
        if text.len() > MAX_TOKEN_CHARS {
            return Err(bad());
        }
        let rest = text.strip_prefix(JOIN_PREFIX).ok_or_else(bad)?;
        let parts: Vec<&str> = rest.split(':').collect();
        if !(3..=5).contains(&parts.len()) {
            return Err(bad());
        }
        let endpoint = unhex::<32>(parts[0]).map_err(|_| bad())?;
        let pairing_id = unhex::<16>(parts[1]).map_err(|_| bad())?;
        let secret = unhex::<32>(parts[2]).map_err(|_| bad())?;
        let relay = match parts.get(3).copied().filter(|p| !p.is_empty()) {
            Some(text) => {
                let relay = percent_decode(text).ok_or_else(bad)?;
                let https = relay
                    .get(..8)
                    .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
                if !https || relay.len() > MAX_RELAY_CHARS {
                    return Err(bad());
                }
                Some(relay)
            }
            None => None,
        };
        // Unparseable addresses are skipped: they only help the dial and are never trusted.
        let addrs = match parts.get(4).copied().filter(|p| !p.is_empty()) {
            Some(text) => percent_decode(text)
                .ok_or_else(bad)?
                .split(',')
                .filter_map(|item| item.trim().parse().ok())
                .take(MAX_ADDRS)
                .collect(),
            None => Vec::new(),
        };
        Ok(Self {
            endpoint,
            pairing_id,
            secret,
            relay,
            addrs,
        })
    }
}
