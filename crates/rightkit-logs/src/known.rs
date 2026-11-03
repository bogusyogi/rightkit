//! Known-value redaction: removes secret strings the application holds (API keys, database
//! keys, tokens) from text and JSON before it is logged, returned or bundled. The pattern scrub
//! in [`crate::scrub`] cannot know a key format it has never seen; this removes the exact values.

use serde_json::Value;

/// What each removed secret becomes. Same marker `rightkit-crash` and `rightkit-http` use.
pub const KNOWN_SECRET_MARKER: &str = "[redacted]";

/// A set of exact secret strings to remove from text and JSON.
///
/// For each secret the set also matches the forms a secret takes once an error path has encoded
/// it: percent-encoded (upper and lower case hex, and the `encodeURIComponent` alphabet) and
/// escaped as inside a JSON or Rust `Debug` string. Overlapping or adjacent matches are merged
/// and replaced as one span, so a secret that overlaps another is removed whole and no fragment
/// of either survives. Matching is exact and case-sensitive.
///
/// Empty strings are ignored (they would match everywhere). Any other value is honoured, so a
/// one-character secret removes that character everywhere; screen values before adding them if
/// that is unwanted. `Debug` prints the number of secrets, never the values. The list is held in
/// memory in clear and is not zeroized on drop.
#[derive(Clone, Default)]
pub struct KnownSecrets {
    /// The distinct secrets as added.
    raw: Vec<String>,
    /// Every needle: the secrets and their encoded forms, distinct, longest first.
    needles: Vec<String>,
}

impl std::fmt::Debug for KnownSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "KnownSecrets({} values)", self.raw.len())
    }
}

impl KnownSecrets {
    /// An empty set; it changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// A set holding `values`.
    pub fn from_values<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = Self::new();
        set.extend(values);
        set
    }

    /// Adds `value`. An empty string is ignored.
    pub fn add(&mut self, value: impl AsRef<str>) {
        self.extend(std::iter::once(value));
    }

    /// Adds every value of `values`. Empty strings are ignored.
    pub fn extend<I, S>(&mut self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for value in values {
            let value = value.as_ref();
            if !value.is_empty() && !self.raw.iter().any(|v| v == value) {
                self.raw.push(value.to_string());
            }
        }
        let mut needles: Vec<String> = Vec::new();
        for secret in &self.raw {
            for form in forms(secret) {
                if !form.is_empty() && !needles.contains(&form) {
                    needles.push(form);
                }
            }
        }
        needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
        self.needles = needles;
    }

    /// Number of distinct secrets held.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Whether no secret is held.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// The set a line-oriented sink needs. A line never holds a raw line break, so a secret that
    /// contains one cannot match whole. This set holds everything in `self` plus every non-empty
    /// fragment of each secret split on CR and LF, so each fragment is removed wherever it
    /// appears, with its own encoded forms. The encoded forms of the whole value (`%0A`, `\n`)
    /// stay and still remove it whole. The size is bounded by the size of the secrets.
    pub(crate) fn for_lines(&self) -> KnownSecrets {
        let fragments: Vec<String> = self
            .raw
            .iter()
            .flat_map(|value| value.split(['\r', '\n']))
            .filter(|fragment| !fragment.is_empty())
            .map(str::to_string)
            .collect();
        let mut set = self.clone();
        set.extend(fragments);
        set
    }

    /// Whether `text` holds a secret or an encoded form of one.
    pub fn is_exposed(&self, text: &str) -> bool {
        self.needles.iter().any(|n| text.contains(n.as_str()))
    }

    /// Replaces each secret in `text` with [`KNOWN_SECRET_MARKER`].
    pub fn redact(&self, text: &str) -> String {
        self.redact_counted(text).0
    }

    /// Like [`redact`](Self::redact), also returning how many spans were replaced.
    pub fn redact_counted(&self, text: &str) -> (String, usize) {
        if self.needles.is_empty() {
            return (text.to_string(), 0);
        }
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for needle in &self.needles {
            let mut from = 0;
            while let Some(pos) = text[from..].find(needle.as_str()) {
                let start = from + pos;
                spans.push((start, start + needle.len()));
                // Step one character so overlapping occurrences are all found.
                let step = text[start..].chars().next().map_or(1, char::len_utf8);
                from = start + step;
            }
        }
        if spans.is_empty() {
            return (text.to_string(), 0);
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            if let Some(last) = merged.last_mut() {
                if start <= last.1 {
                    last.1 = last.1.max(end);
                    continue;
                }
            }
            merged.push((start, end));
        }
        let mut out = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in &merged {
            out.push_str(&text[cursor..*start]);
            out.push_str(KNOWN_SECRET_MARKER);
            cursor = *end;
        }
        out.push_str(&text[cursor..]);
        (out, merged.len())
    }

    /// Removes the known secrets, then applies the pattern scrub ([`crate::scrub_text`]). The
    /// known values go first so a secret that the patterns would only partly match is removed
    /// whole.
    pub fn scrub_text(&self, text: &str) -> String {
        crate::scrub::scrub_text(&self.redact(text))
    }

    /// Applies [`redact`](Self::redact) to every string in `value`, object keys included.
    pub fn redact_value(&self, value: &mut Value) {
        if self.needles.is_empty() {
            return;
        }
        match value {
            Value::String(s) => *s = self.redact(s),
            Value::Array(items) => items.iter_mut().for_each(|v| self.redact_value(v)),
            Value::Object(map) => {
                let old = std::mem::take(map);
                for (key, mut v) in old {
                    self.redact_value(&mut v);
                    map.insert(self.redact(&key), v);
                }
            }
            _ => {}
        }
    }

    /// Applies [`redact_value`](Self::redact_value), then the pattern scrub
    /// ([`crate::scrub_value`]).
    pub fn scrub_value(&self, value: &mut Value) {
        self.redact_value(value);
        crate::scrub::scrub_value(value);
    }
}

/// `value` and its encoded forms.
fn forms(value: &str) -> Vec<String> {
    vec![
        value.to_string(),
        percent_encode(value, false, true),
        percent_encode(value, false, false),
        percent_encode(value, true, true),
        escape_json(value),
        escape_debug(value),
    ]
}

/// Percent-encoding. `js` keeps the `encodeURIComponent` extras (`!'()*`) literal; `upper`
/// selects upper-case hex digits.
fn percent_encode(value: &str, js: bool, upper: bool) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (js && matches!(byte, b'!' | b'\'' | b'(' | b')' | b'*'));
        if keep {
            out.push(byte as char);
        } else if upper {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}

/// The text of `value` as it appears between the quotes of a JSON string.
fn escape_json(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .unwrap_or_default()
        .to_string()
}

/// The text of `value` as it appears between the quotes of a Rust `{:?}` string.
fn escape_debug(value: &str) -> String {
    let quoted = format!("{value:?}");
    quoted
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .unwrap_or_default()
        .to_string()
}
