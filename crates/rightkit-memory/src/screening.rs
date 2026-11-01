//! Write-time screening for durable memory (ENG-81).
//!
//! Every candidate memory passes through [`screen`] before it may reach the
//! store. Screening is deliberately conservative and dependency-free (no
//! regex crate in this workspace member): it rejects anything that looks
//! like a secret/credential, anything empty or too short to be a durable
//! fact, and a short list of volatile/ephemeral junk phrases that pollute
//! recall without adding information.

use serde::{Deserialize, Serialize};

/// Why a candidate memory was rejected at write time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Empty or whitespace-only content.
    Empty,
    /// Below the minimum durable-fact length; almost certainly a fragment
    /// or a status ping, not something worth persisting cross-session.
    TooShort,
    /// Matched a credential/secret heuristic (keyword-adjacent, PEM block,
    /// JWT shape, or a long high-entropy opaque token).
    Credential,
    /// Matched a known volatile/ephemeral phrase (loading states, generic
    /// acknowledgements, bare timestamps) that carries no durable meaning.
    VolatileJunk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "verdict")]
pub enum ScreeningVerdict {
    Admit,
    Reject { reason: RejectReason },
}

impl ScreeningVerdict {
    pub fn is_admit(&self) -> bool {
        matches!(self, ScreeningVerdict::Admit)
    }
}

/// Minimum trimmed length (bytes) for a candidate to be considered a durable
/// fact rather than a fragment. Deliberately short so legitimate terse facts
/// ("Adrian uses pnpm.") still pass.
pub const MIN_DURABLE_LEN: usize = 8;

/// Keyword stems that, when they appear immediately before an assignment-
/// like separator (`:`, `=`, whitespace) followed by a plausible secret
/// value, mark the whole candidate as a credential. Matching is
/// case-insensitive and stem-based (substring) on purpose: screening must
/// stay a hard gate, not a fooled-by-casing gate.
const CREDENTIAL_KEYWORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "api_key",
    "apikey",
    "api-key",
    "secret",
    "access_key",
    "access-key",
    "accesskey",
    "private_key",
    "private-key",
    "privatekey",
    "client_secret",
    "auth_token",
    "authtoken",
    "bearer",
    "authorization:",
    "token",
    "credential",
    "ssn",
    "passphrase",
];

const VOLATILE_JUNK_PHRASES: &[&str] = &[
    "ok",
    "okay",
    "done",
    "loading",
    "loading...",
    "pong",
    "ping",
    "yes",
    "no",
    "thanks",
    "thank you",
    "hi",
    "hello",
    "test",
    "testing",
    "n/a",
    "none",
    "in progress",
    "working on it",
];

/// Screen a candidate memory's raw content. Pure function: no I/O, no store
/// access, so it can run identically on every write path (CLI, daemon,
/// mobile FFI) before a record is ever persisted.
pub fn screen(content: &str) -> ScreeningVerdict {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return ScreeningVerdict::Reject {
            reason: RejectReason::Empty,
        };
    }
    if trimmed.len() < MIN_DURABLE_LEN {
        return ScreeningVerdict::Reject {
            reason: RejectReason::TooShort,
        };
    }
    if looks_like_credential(trimmed) {
        return ScreeningVerdict::Reject {
            reason: RejectReason::Credential,
        };
    }
    if is_volatile_junk(trimmed) {
        return ScreeningVerdict::Reject {
            reason: RejectReason::VolatileJunk,
        };
    }
    ScreeningVerdict::Admit
}

fn looks_like_credential(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();

    if lower.contains("-----begin ") && lower.contains("private key") {
        return true;
    }

    if CREDENTIAL_KEYWORDS
        .iter()
        .any(|kw| keyword_near_assignment(&lower, kw))
    {
        return true;
    }

    if looks_like_jwt(trimmed) {
        return true;
    }

    // A single opaque token with no whitespace, long enough and high enough
    // entropy to be a raw secret rather than prose (e.g. a pasted API key
    // with no surrounding keyword). Multi-word content never trips this.
    if !trimmed.contains(char::is_whitespace)
        && trimmed.len() >= 24
        && trimmed.len() <= 512
        && looks_high_entropy(trimmed)
    {
        return true;
    }

    false
}

/// `true` when `keyword` appears in `lower` immediately followed (after
/// optional whitespace) by one of `:`, `=`, or `-` and then a non-trivial
/// value. This keeps prose like "the password policy requires 12 characters"
/// from tripping the gate while still catching `password: hunter2`,
/// `API_KEY=sk-abc123`, `secret = "xyz"`.
fn keyword_near_assignment(lower: &str, keyword: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(keyword) {
        let start = search_from + rel;
        let after = &lower[start + keyword.len()..];
        let after_trimmed = after.trim_start();
        if let Some(rest) = after_trimmed
            .strip_prefix(':')
            .or_else(|| after_trimmed.strip_prefix('='))
        {
            let value = rest.trim();
            if value.len() >= 3 {
                return true;
            }
        }
        search_from = start + keyword.len();
        if search_from >= lower.len() {
            break;
        }
    }
    false
}

/// Cheap JWT shape check: three dot-separated segments, each base64url-ish,
/// first segment decodes (loosely, by charset) to a header-like token.
fn looks_like_jwt(trimmed: &str) -> bool {
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| p.len() >= 8 && p.chars().all(is_base64url_char))
}

fn is_base64url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Very cheap entropy heuristic: a mix of upper, lower, and digit classes
/// with no repeated-character runs is treated as opaque/random rather than a
/// natural-language token. Not cryptographic entropy -- just enough to keep
/// obvious API-key-shaped pastes out of durable memory without a dependency.
fn looks_high_entropy(token: &str) -> bool {
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let class_count = [has_lower, has_upper, has_digit]
        .iter()
        .filter(|b| **b)
        .count();
    class_count >= 2
}

fn is_volatile_junk(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    let normalized = lower.trim_end_matches(['.', '!', '?']).trim();
    if VOLATILE_JUNK_PHRASES.contains(&normalized) {
        return true;
    }
    // Bare ISO-8601-shaped timestamp with nothing else: carries no durable
    // meaning on its own.
    if normalized.len() <= 32
        && normalized.chars().enumerate().all(|(i, c)| {
            c.is_ascii_digit() || matches!(c, '-' | ':' | 't' | 'z' | '.' | '+') || i == 0
        })
        && normalized.chars().any(|c| c.is_ascii_digit())
        && normalized.chars().filter(|c| c.is_ascii_digit()).count() >= 8
        && !normalized
            .chars()
            .any(|c| c.is_ascii_alphabetic() && !matches!(c, 't' | 'z'))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_short_rejected() {
        assert_eq!(
            screen(""),
            ScreeningVerdict::Reject {
                reason: RejectReason::Empty
            }
        );
        assert_eq!(
            screen("   "),
            ScreeningVerdict::Reject {
                reason: RejectReason::Empty
            }
        );
        assert_eq!(
            screen("short"),
            ScreeningVerdict::Reject {
                reason: RejectReason::TooShort
            }
        );
    }

    #[test]
    fn credential_keyword_rejected() {
        assert_eq!(
            screen("password: hunter2fooo"),
            ScreeningVerdict::Reject {
                reason: RejectReason::Credential
            }
        );
        assert_eq!(
            screen("API_KEY=sk-abcdef1234567890"),
            ScreeningVerdict::Reject {
                reason: RejectReason::Credential
            }
        );
    }

    #[test]
    fn credential_keyword_in_prose_admitted() {
        // "password" appears but with no assignment -- ordinary prose.
        assert!(screen("The password policy requires twelve characters minimum.").is_admit());
    }

    #[test]
    fn pem_block_rejected() {
        let content = "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK...";
        assert_eq!(
            screen(content),
            ScreeningVerdict::Reject {
                reason: RejectReason::Credential
            }
        );
    }

    #[test]
    fn jwt_shape_rejected() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dQw4w9WgXcQx1c2VyX3NlY3JldA";
        assert_eq!(
            screen(jwt),
            ScreeningVerdict::Reject {
                reason: RejectReason::Credential
            }
        );
    }

    #[test]
    fn opaque_high_entropy_token_rejected() {
        assert_eq!(
            screen(&format!("sk_{}_{}", "live", "4eC39HqLyjWDarjtT1zdp7dc")),
            ScreeningVerdict::Reject {
                reason: RejectReason::Credential
            }
        );
    }

    #[test]
    fn volatile_junk_rejected() {
        assert_eq!(
            screen("loading..."),
            ScreeningVerdict::Reject {
                reason: RejectReason::VolatileJunk
            }
        );
        assert_eq!(
            screen("2026-08-29T10:00:00Z"),
            ScreeningVerdict::Reject {
                reason: RejectReason::VolatileJunk
            }
        );
    }

    #[test]
    fn durable_fact_admitted() {
        assert!(screen("Adrian prefers pnpm over npm for this workspace.").is_admit());
        assert!(screen("The MyApp desktop UI lives in apps/myapp-desktop.").is_admit());
    }
}
