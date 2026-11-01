//! L11: the `SearchTerms` accumulator — the flat, SQL-builder-friendly shape
//! `query.rs::compile` produces from the AST. Split out of `query.rs` to keep
//! that module under the lane's 400-line-per-module target now that the
//! real lexer/parser + new operators live alongside it.

use rightkit_search::terms::Query;

/// Parsed search query: Gmail-style operators plus free text.
#[derive(Default, Debug)]
pub struct SearchTerms {
    pub from: Option<String>,
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: Option<String>,
    pub filename: Option<String>,
    pub has_attachment: bool,
    pub is_read: Option<bool>, // None = no filter, Some(true)/Some(false) = explicit
    pub unread: bool,
    pub starred: bool,
    pub important: bool,
    pub after_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub newer_ms: Option<i64>,
    pub older_ms: Option<i64>,
    pub in_folder: Option<String>, // INBOX/SENT/DRAFT/SPAM/TRASH/ANYWHERE
    pub label: Option<String>,     // system or custom label name
    pub label_negated: bool,       // `-label:foo` (exclude)
    pub negated_from: Vec<String>,
    pub negated_to: Vec<String>,
    pub cc_negated: Vec<String>,
    pub bcc_negated: Vec<String>,
    pub negated_text: Vec<String>,
    pub text: Vec<String>,
    /// Free-text alternatives. Each inner vec becomes `(a OR b)` in FTS.
    pub or_groups: Vec<Vec<String>>,
    /// L11: exact quoted-phrase terms (kept separate from `text` so the FTS
    /// builder can emit a real FTS5 phrase query `"a b c"` instead of the
    /// prefix-AND-of-words shape bare terms get).
    pub phrases: Vec<String>,
    /// L11: `size:`/`larger:`/`smaller:` in bytes. `size_min`/`size_max` are
    /// inclusive/exclusive bounds respectively; `larger:` sets only
    /// `size_min`, `smaller:` sets only `size_max`. Gmail's bare `size:` is a
    /// convenience alias for `larger:` (per Gmail help: "size:1000000"
    /// behaves like "larger:1000000"), so it also only sets `size_min`.
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    /// L11: `rfc822msgid:` — exact match on the Message-ID header.
    pub rfc822_message_id: Option<String>,
}

impl SearchTerms {
    fn has_negation(&self) -> bool {
        !self.negated_text.is_empty()
            || !self.negated_from.is_empty()
            || !self.negated_to.is_empty()
            || !self.cc_negated.is_empty()
            || !self.bcc_negated.is_empty()
            || self.label_negated
    }

    pub fn is_empty(&self) -> bool {
        self.from.is_none()
            && self.to.is_none()
            && self.cc.is_none()
            && self.bcc.is_none()
            && self.subject.is_none()
            && self.filename.is_none()
            && !self.has_attachment
            && self.is_read.is_none()
            && !self.unread
            && !self.starred
            && !self.important
            && self.after_ms.is_none()
            && self.before_ms.is_none()
            && self.newer_ms.is_none()
            && self.older_ms.is_none()
            && self.in_folder.is_none()
            && self.label.is_none()
            && !self.has_negation()
            && self.text.is_empty()
            && self.or_groups.is_empty()
            && self.phrases.is_empty()
            && self.size_min.is_none()
            && self.size_max.is_none()
            && self.rfc822_message_id.is_none()
    }

    /// The free-text part of this accumulator as a `rightkit_search` query.
    /// Operator fields are not carried; FTS and positive-term logic come from
    /// `rightkit-search` so the quoting rules live in one place.
    fn free_query(&self) -> Query {
        let mut q = Query::default();
        q.text.clone_from(&self.text);
        q.phrases.clone_from(&self.phrases);
        q.or_groups.clone_from(&self.or_groups);
        q.negated_text.clone_from(&self.negated_text);
        q
    }

    /// FTS MATCH expression for the free-text terms and quoted phrases, or None.
    pub fn fts(&self) -> Option<String> {
        self.free_query().fts()
    }

    pub fn positive_text_terms(&self) -> Vec<String> {
        self.free_query().positive_text_terms()
    }
}
