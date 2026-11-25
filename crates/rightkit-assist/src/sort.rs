//! Sorting in code: the owner's sender lists and the mail's headers settle travel, customer, bill and bulk.
//! No model is involved. Bulk mail stops here as "no work".
//!
//! The lists are records the owner edits (`pulse assist senders` in Pulse). Nothing a mail says changes them.

use crate::signal::Trust;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Travel,
    Customer,
    Bill,
    Bulk,
}

/// One owner-edited line. `pattern` is a full address (`desk@hotel.example`) or a domain (`@hotel.example`,
/// which also covers its subdomains). An exact address beats a domain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SenderEntry {
    pub pattern: String,
    pub category: Category,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SenderLists {
    #[serde(default)]
    pub entries: Vec<SenderEntry>,
    /// The owner's own addresses, so a message from the owner is `Trust::Owner`.
    #[serde(default)]
    pub owner_addresses: Vec<String>,
}

/// A mail as the host hands it over. The host does the fetching and the MIME work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundMail {
    pub from: String,
    pub subject: String,
    /// Every header as `(name, value)`; names compare case-insensitively.
    pub headers: Vec<(String, String)>,
    pub plain_text: String,
    pub html: Option<String>,
}

impl InboundMail {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// `"Name <a@b.example>"` becomes `a@b.example`, lowercased and trimmed.
pub fn address_of(from: &str) -> String {
    let trimmed = from.trim();
    let inner = match (trimmed.rfind('<'), trimmed.rfind('>')) {
        (Some(open), Some(close)) if open < close => &trimmed[open + 1..close],
        _ => trimmed,
    };
    inner.trim().to_ascii_lowercase()
}

fn normalise_pattern(pattern: &str) -> String {
    pattern.trim().to_ascii_lowercase()
}

impl SenderLists {
    /// Add or change the entry for `pattern`. Owner path only.
    pub fn upsert(&mut self, pattern: &str, category: Category, note: &str) {
        let pattern = normalise_pattern(pattern);
        if pattern.is_empty() {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.pattern == pattern) {
            entry.category = category;
            entry.note = note.to_string();
        } else {
            self.entries.push(SenderEntry {
                pattern,
                category,
                note: note.to_string(),
            });
        }
    }

    pub fn remove(&mut self, pattern: &str) -> bool {
        let pattern = normalise_pattern(pattern);
        let before = self.entries.len();
        self.entries.retain(|e| e.pattern != pattern);
        self.entries.len() != before
    }

    /// The entry for an address: an exact match first, then the longest domain match.
    pub fn match_sender(&self, address: &str) -> Option<&SenderEntry> {
        if let Some(entry) = self.entries.iter().find(|e| e.pattern == address) {
            return Some(entry);
        }
        let domain = address.rsplit_once('@').map(|(_, d)| d)?;
        self.entries
            .iter()
            .filter(|e| e.pattern.starts_with('@'))
            .filter(|e| {
                let want = &e.pattern[1..];
                domain == want
                    || (domain.len() > want.len()
                        && domain.ends_with(want)
                        && domain.as_bytes()[domain.len() - want.len() - 1] == b'.')
            })
            .max_by_key(|e| e.pattern.len())
    }

    pub fn is_owner(&self, address: &str) -> bool {
        self.owner_addresses
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case(address))
    }

    /// Trust for a mail: the owner's own address with passing (or absent) authentication is `Owner`.
    pub fn trust_of(&self, mail: &InboundMail) -> Trust {
        let address = address_of(&mail.from);
        if self.is_owner(&address) && !authentication_failed(mail) {
            Trust::Owner
        } else {
            Trust::Outside
        }
    }
}

/// The receiving server said the sender could not be verified.
pub fn authentication_failed(mail: &InboundMail) -> bool {
    match mail.header("authentication-results") {
        Some(value) => {
            let v = value.to_ascii_lowercase();
            v.contains("dmarc=fail") || v.contains("spf=fail") || v.contains("dkim=fail")
        }
        None => false,
    }
}

/// Headers that mark bulk mail: a list or unsubscribe header, bulk precedence, or an automatic sender.
pub fn bulk_headers(mail: &InboundMail) -> bool {
    if mail.header("list-unsubscribe").is_some() || mail.header("list-id").is_some() {
        return true;
    }
    if let Some(p) = mail.header("precedence") {
        let p = p.trim().to_ascii_lowercase();
        if p == "bulk" || p == "list" || p == "junk" {
            return true;
        }
    }
    if let Some(a) = mail.header("auto-submitted") {
        if !a.trim().eq_ignore_ascii_case("no") {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortedBy {
    SenderList,
    Header,
    Unplaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sorting {
    pub category: Option<Category>,
    pub by: SortedBy,
    /// The sender is on the owner's lists and authenticated.
    pub sender_known: bool,
}

/// Sort one mail. The owner's list wins over headers (a bill often carries an unsubscribe header). A sender
/// whose authentication failed is not matched against the list, because the address may be forged; its
/// headers still apply.
pub fn sort(lists: &SenderLists, mail: &InboundMail) -> Sorting {
    if !authentication_failed(mail) {
        let address = address_of(&mail.from);
        if let Some(entry) = lists.match_sender(&address) {
            return Sorting {
                category: Some(entry.category),
                by: SortedBy::SenderList,
                sender_known: true,
            };
        }
    }
    if bulk_headers(mail) {
        return Sorting {
            category: Some(Category::Bulk),
            by: SortedBy::Header,
            sender_known: false,
        };
    }
    Sorting {
        category: None,
        by: SortedBy::Unplaced,
        sender_known: false,
    }
}
