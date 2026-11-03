//! RSS 2.0, RSS 1.0 (RDF) and Atom feed parsing for apps that import feed items.
//!
//! The parser is tolerant and bounded:
//!
//! - Only the five predefined entities and numeric character references are decoded. A DOCTYPE,
//!   including any internal entity definitions, is skipped and never interpreted, so there is no
//!   entity expansion. CDATA is read as text.
//! - Input size, item count, per-field size and element depth are capped by [`Limits`].
//! - Mismatched or unclosed tags do not fail the parse; a document with no RSS, RDF or Atom root
//!   does ([`FeedError::NotAFeed`]).
//! - Item links are kept only when they are `http` or `https` URLs.
//!
//! Dates are read to Unix seconds from RFC 3339 and RFC 822 forms; the raw text is kept too.

mod date;
mod xml;

use xml::{Event, Scanner};

/// Which feed format the root element declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    /// `<rss>` (0.9x and 2.0).
    Rss2,
    /// `<rdf:RDF>` (RSS 1.0).
    Rss1,
    /// `<feed>` (Atom).
    Atom,
}

/// Parse limits. The defaults suit imports of public feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest input accepted, in bytes.
    pub max_bytes: usize,
    /// Most items returned; further items set [`Feed::truncated`].
    pub max_items: usize,
    /// Longest text kept for any one field, in bytes (cut at a character boundary).
    pub max_field_bytes: usize,
    /// Deepest element nesting accepted.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_bytes: 5 * 1024 * 1024,
            max_items: 500,
            max_field_bytes: 16 * 1024,
            max_depth: 64,
        }
    }
}

/// Why a document was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedError {
    /// The input was empty or only whitespace.
    Empty,
    /// The input was longer than [`Limits::max_bytes`].
    TooLarge,
    /// Elements nested deeper than [`Limits::max_depth`].
    TooDeep,
    /// The root element was not `rss`, `rdf` or `feed`.
    NotAFeed,
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FeedError::Empty => "feed document is empty",
            FeedError::TooLarge => "feed document is larger than the limit",
            FeedError::TooDeep => "feed document nests deeper than the limit",
            FeedError::NotAFeed => "document root is not an RSS, RDF or Atom feed",
        })
    }
}

impl std::error::Error for FeedError {}

/// One feed item (an RSS `item` or an Atom `entry`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedItem {
    /// RSS `guid` or Atom `id`.
    pub id: Option<String>,
    pub title: Option<String>,
    /// The item's `http`/`https` link (Atom: the `alternate` link, or one with no `rel`).
    pub link: Option<String>,
    /// The publication date text as written (RSS `pubDate` or `dc:date`; Atom `published`, else
    /// `updated`).
    pub published: Option<String>,
    /// `published` as Unix seconds, when it reads as RFC 3339 or RFC 822.
    pub published_unix: Option<i64>,
    /// RSS `description` or Atom `summary` (Atom `content` when there is no summary).
    pub summary: Option<String>,
}

/// A parsed feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Feed {
    pub kind: FeedKind,
    pub title: Option<String>,
    /// The feed's own `http`/`https` link.
    pub link: Option<String>,
    pub items: Vec<FeedItem>,
    /// True when items beyond [`Limits::max_items`] were dropped.
    pub truncated: bool,
}

/// Parse a feed document. Bytes that are not valid UTF-8 are replaced, not refused.
pub fn parse(input: &[u8], limits: &Limits) -> Result<Feed, FeedError> {
    if input.len() > limits.max_bytes {
        return Err(FeedError::TooLarge);
    }
    let text = String::from_utf8_lossy(input);
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Err(FeedError::Empty);
    }
    let mut builder = Builder::new(limits);
    let mut scanner = Scanner::new(text);
    while let Some(event) = scanner.next_event() {
        builder.event(event)?;
    }
    builder.finish()
}

struct Builder<'l> {
    limits: &'l Limits,
    stack: Vec<String>,
    kind: Option<FeedKind>,
    /// The root element's namespace prefix plus colon (for `atom:feed`), stripped from names.
    root_prefix: String,
    /// Depth of the channel (or Atom feed) element, 0 until seen.
    channel_depth: usize,
    /// Depth of the open item element, 0 when none.
    item_depth: usize,
    item: FeedItem,
    updated: Option<String>,
    content: Option<String>,
    text: String,
    feed: Feed,
}

impl<'l> Builder<'l> {
    fn new(limits: &'l Limits) -> Builder<'l> {
        Builder {
            limits,
            stack: Vec::new(),
            kind: None,
            root_prefix: String::new(),
            channel_depth: 0,
            item_depth: 0,
            item: FeedItem::default(),
            updated: None,
            content: None,
            text: String::new(),
            feed: Feed {
                kind: FeedKind::Rss2,
                title: None,
                link: None,
                items: Vec::new(),
                truncated: false,
            },
        }
    }

    fn norm<'n>(&self, name: &'n str) -> &'n str {
        if !self.root_prefix.is_empty() {
            if let Some(rest) = name.strip_prefix(self.root_prefix.as_str()) {
                return rest;
            }
        }
        name
    }

    fn in_item(&self) -> bool {
        self.item_depth > 0
    }

    fn event(&mut self, event: Event) -> Result<(), FeedError> {
        match event {
            Event::Start { name, attrs, empty } => {
                self.start(name, &attrs)?;
                if empty {
                    self.pop_one();
                }
            }
            Event::End(name) => {
                if let Some(index) = self.stack.iter().rposition(|open| *open == name) {
                    while self.stack.len() > index {
                        self.pop_one();
                    }
                }
            }
            Event::Text(text) => {
                let collecting = if self.in_item() {
                    self.stack.len() > self.item_depth
                } else {
                    self.channel_depth > 0 && self.stack.len() > self.channel_depth
                };
                if collecting {
                    let room = self.limits.max_field_bytes.saturating_sub(self.text.len());
                    push_capped(&mut self.text, &text, room);
                }
            }
        }
        Ok(())
    }

    fn start(&mut self, name: String, attrs: &[(String, String)]) -> Result<(), FeedError> {
        if self.stack.len() >= self.limits.max_depth {
            return Err(FeedError::TooDeep);
        }
        if self.stack.is_empty() {
            let (prefix, local) = match name.split_once(':') {
                Some((p, l)) => (format!("{p}:"), l),
                None => (String::new(), name.as_str()),
            };
            let kind = match local {
                "rss" => FeedKind::Rss2,
                "rdf" => FeedKind::Rss1,
                "feed" => FeedKind::Atom,
                _ => return Err(FeedError::NotAFeed),
            };
            self.kind = Some(kind);
            self.feed.kind = kind;
            if kind == FeedKind::Atom {
                self.root_prefix = prefix;
                self.channel_depth = 1;
            }
            self.stack.push(name);
            return Ok(());
        }
        let kind = self.kind.unwrap_or(FeedKind::Rss2);
        let local = self.norm(&name).to_string();
        let depth_before = self.stack.len();
        if !self.in_item() {
            let item_name = if kind == FeedKind::Atom {
                "entry"
            } else {
                "item"
            };
            if local == item_name {
                self.stack.push(name);
                self.item_depth = self.stack.len();
                self.item = FeedItem::default();
                self.updated = None;
                self.content = None;
                self.text.clear();
                return Ok(());
            }
            if kind != FeedKind::Atom && local == "channel" && self.channel_depth == 0 {
                self.channel_depth = depth_before + 1;
            }
            if self.channel_depth > 0 && depth_before == self.channel_depth {
                self.text.clear();
                self.read_link_attrs(&local, attrs, kind, false);
            }
        } else if depth_before == self.item_depth {
            self.text.clear();
            self.read_link_attrs(&local, attrs, kind, true);
        }
        self.stack.push(name);
        Ok(())
    }

    /// Atom carries the link in `href`, not in text.
    fn read_link_attrs(
        &mut self,
        local: &str,
        attrs: &[(String, String)],
        kind: FeedKind,
        item: bool,
    ) {
        if kind != FeedKind::Atom || local != "link" {
            return;
        }
        let attr = |key: &str| {
            attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let rel = attr("rel").unwrap_or("alternate");
        if rel != "alternate" {
            return;
        }
        let Some(href) = attr("href") else { return };
        let Some(link) = clean_link(href, self.limits.max_field_bytes) else {
            return;
        };
        if item {
            self.item.link.get_or_insert(link);
        } else {
            self.feed.link.get_or_insert(link);
        }
    }

    fn pop_one(&mut self) {
        let depth = self.stack.len();
        let Some(name) = self.stack.pop() else { return };
        let kind = self.kind.unwrap_or(FeedKind::Rss2);
        let local = self.norm(&name).to_string();
        let value = clean_text(&self.text, self.limits.max_field_bytes);
        if self.in_item() {
            if depth == self.item_depth + 1 {
                self.finish_item_field(&name, &local, kind, value);
            } else if depth == self.item_depth {
                self.finish_item();
            }
        } else if self.channel_depth > 0 && depth == self.channel_depth + 1 {
            match local.as_str() {
                "title" if self.feed.title.is_none() => self.feed.title = value,
                "link" if kind != FeedKind::Atom && self.feed.link.is_none() => {
                    self.feed.link =
                        value.and_then(|v| clean_link(&v, self.limits.max_field_bytes));
                }
                _ => {}
            }
        }
        if depth <= self.item_depth + 1 || depth <= self.channel_depth + 1 {
            self.text.clear();
        }
    }

    fn finish_item_field(
        &mut self,
        full: &str,
        local: &str,
        kind: FeedKind,
        value: Option<String>,
    ) {
        let max = self.limits.max_field_bytes;
        match (kind, local) {
            (_, "title") => {
                if self.item.title.is_none() {
                    self.item.title = value;
                }
            }
            (FeedKind::Atom, "id") | (_, "guid") => {
                if self.item.id.is_none() {
                    self.item.id = value;
                }
            }
            (FeedKind::Atom, "published") | (_, "pubdate") | (_, "dc:date") => {
                if self.item.published.is_none() {
                    self.item.published = value;
                }
            }
            (FeedKind::Atom, "updated") => {
                if self.updated.is_none() {
                    self.updated = value;
                }
            }
            (FeedKind::Atom, "summary") | (_, "description") => {
                if self.item.summary.is_none() {
                    self.item.summary = value;
                }
            }
            (FeedKind::Atom, "content") => {
                if self.content.is_none() {
                    self.content = value;
                }
            }
            // RSS links are element text; `atom:link` and the like inside an RSS item are ignored.
            (FeedKind::Rss2 | FeedKind::Rss1, "link") if full == "link" => {
                if self.item.link.is_none() {
                    self.item.link = value.and_then(|v| clean_link(&v, max));
                }
            }
            _ => {}
        }
    }

    fn finish_item(&mut self) {
        let mut item = std::mem::take(&mut self.item);
        if item.published.is_none() {
            item.published = self.updated.take();
        }
        if item.summary.is_none() {
            item.summary = self.content.take();
        }
        item.published_unix = item.published.as_deref().and_then(date::parse_unix);
        self.item_depth = 0;
        if self.feed.items.len() < self.limits.max_items {
            self.feed.items.push(item);
        } else {
            self.feed.truncated = true;
        }
    }

    fn finish(mut self) -> Result<Feed, FeedError> {
        if self.kind.is_none() {
            return Err(FeedError::NotAFeed);
        }
        // An item left open by a cut-off document still counts.
        if self.in_item() {
            self.finish_item();
        }
        Ok(self.feed)
    }
}

/// Append at most `room` bytes of `add` to `out`, cut at a character boundary.
fn push_capped(out: &mut String, add: &str, room: usize) {
    if add.len() <= room {
        out.push_str(add);
        return;
    }
    let mut end = room;
    while end > 0 && !add.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&add[..end]);
}

/// Trim and cap a field; `None` when nothing is left.
fn clean_text(text: &str, max: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = String::new();
    push_capped(&mut out, trimmed, max);
    Some(out)
}

/// A link kept only when it is an `http` or `https` URL with something after the scheme.
fn clean_link(text: &str, max: usize) -> Option<String> {
    let text = clean_text(text, max)?;
    let lower = text.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    if rest.is_empty() || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    Some(text)
}
