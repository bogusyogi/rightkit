//! `Scope`: the venture, channel and account a token budget, cache entry, spend allowance or
//! proof lookup belongs to. It renders to one opaque, stable key so each of those stores can use
//! the same string (for the ledger it is the `scope` argument).

use std::fmt;

/// A scope part is empty or holds a character the key format reserves.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("scope part {part:?} is empty")]
    Empty { part: &'static str },
    #[error("scope part {part:?} holds a reserved character (`/`, `:` or whitespace or control)")]
    Reserved { part: &'static str },
    #[error("scope key {0:?} is not `venture/channel/account`")]
    BadKey(String),
}

/// Venture, channel and account. Each part is non-empty and holds no `/`, `:`, whitespace or
/// control character, so the key `venture/channel/account` parses back to the same scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Scope {
    venture: String,
    channel: String,
    account: String,
}

fn check(part: &'static str, value: &str) -> Result<(), ScopeError> {
    if value.is_empty() {
        return Err(ScopeError::Empty { part });
    }
    if value
        .chars()
        .any(|c| c == '/' || c == ':' || c.is_whitespace() || c.is_control())
    {
        return Err(ScopeError::Reserved { part });
    }
    Ok(())
}

impl Scope {
    pub fn new(venture: &str, channel: &str, account: &str) -> Result<Self, ScopeError> {
        check("venture", venture)?;
        check("channel", channel)?;
        check("account", account)?;
        Ok(Self {
            venture: venture.to_owned(),
            channel: channel.to_owned(),
            account: account.to_owned(),
        })
    }

    /// Parse `venture/channel/account`.
    pub fn parse(key: &str) -> Result<Self, ScopeError> {
        let mut parts = key.split('/');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(v), Some(c), Some(a), None) => Self::new(v, c, a),
            _ => Err(ScopeError::BadKey(key.to_owned())),
        }
    }

    pub fn venture(&self) -> &str {
        &self.venture
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// The opaque key `venture/channel/account`; pass it as the ledger `scope`.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.venture, self.channel, self.account)
    }

    /// The key under a store namespace such as `tokens`, `cache`, `spend` or `proof`:
    /// `namespace:venture/channel/account`.
    pub fn namespaced(&self, namespace: &str) -> Result<String, ScopeError> {
        check("namespace", namespace)?;
        Ok(format!("{namespace}:{}", self.key()))
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.venture, self.channel, self.account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trips() {
        let s = Scope::new("orthic", "youtube", "acct1").unwrap();
        assert_eq!(s.key(), "orthic/youtube/acct1");
        assert_eq!(Scope::parse(&s.key()).unwrap(), s);
        assert_eq!(s.to_string(), s.key());
    }

    #[test]
    fn namespaced_keys_differ_per_store() {
        let s = Scope::new("v", "c", "a").unwrap();
        assert_eq!(s.namespaced("tokens").unwrap(), "tokens:v/c/a");
        assert_ne!(
            s.namespaced("cache").unwrap(),
            s.namespaced("proof").unwrap()
        );
        assert!(s.namespaced("a:b").is_err());
    }

    #[test]
    fn bad_parts_are_refused() {
        assert_eq!(
            Scope::new("", "c", "a"),
            Err(ScopeError::Empty { part: "venture" })
        );
        assert_eq!(
            Scope::new("v", "c/d", "a"),
            Err(ScopeError::Reserved { part: "channel" })
        );
        assert_eq!(
            Scope::new("v", "c", "a b"),
            Err(ScopeError::Reserved { part: "account" })
        );
        assert!(Scope::parse("v/c").is_err());
        assert!(Scope::parse("v/c/a/x").is_err());
        assert!(Scope::parse("v//a").is_err());
    }
}
