//! A small tolerant XML tokenizer for feeds. It never expands entities beyond the five predefined
//! ones and numeric character references, skips DOCTYPE declarations (and so any internal entity
//! definitions), comments and processing instructions, and reads CDATA as plain text.

/// Most attributes read from one tag; later ones are dropped.
const MAX_ATTRIBUTES: usize = 32;

/// One token.
#[derive(Debug)]
pub(crate) enum Event {
    /// An opening tag. `empty` is true for `<x/>`.
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        empty: bool,
    },
    /// A closing tag.
    End(String),
    /// Character data, entity-decoded (CDATA is passed through verbatim).
    Text(String),
}

pub(crate) struct Scanner<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(src: &'a str) -> Scanner<'a> {
        Scanner { src, pos: 0 }
    }

    /// The next token, or `None` at the end (a tag cut off by the end of input is dropped).
    pub(crate) fn next_event(&mut self) -> Option<Event> {
        loop {
            if self.pos >= self.src.len() {
                return None;
            }
            let rest = &self.src[self.pos..];
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                let text = decode_entities(&rest[..end]);
                self.pos += end;
                return Some(Event::Text(text));
            }
            if let Some(after) = rest.strip_prefix("<!--") {
                match after.find("-->") {
                    Some(i) => self.pos += 4 + i + 3,
                    None => self.pos = self.src.len(),
                }
                continue;
            }
            if let Some(after) = rest.strip_prefix("<![CDATA[") {
                let (text, advance) = match after.find("]]>") {
                    Some(i) => (&after[..i], 9 + i + 3),
                    None => (after, rest.len()),
                };
                let text = text.to_string();
                self.pos += advance;
                return Some(Event::Text(text));
            }
            if rest.starts_with("<?") {
                match rest.find("?>") {
                    Some(i) => self.pos += i + 2,
                    None => self.pos = self.src.len(),
                }
                continue;
            }
            if rest.starts_with("<!") {
                self.pos += skip_declaration(rest);
                continue;
            }
            if let Some(after) = rest.strip_prefix("</") {
                let Some(close) = after.find('>') else {
                    self.pos = self.src.len();
                    return None;
                };
                let name = after[..close].trim().to_ascii_lowercase();
                self.pos += 2 + close + 1;
                if name.is_empty() {
                    continue;
                }
                return Some(Event::End(name));
            }
            // A start tag needs a name-start character after '<'; anything else is literal text.
            let starts_name = rest[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':');
            if !starts_name {
                self.pos += 1;
                return Some(Event::Text("<".to_string()));
            }
            match parse_start_tag(rest) {
                Some((event, used)) => {
                    self.pos += used;
                    return Some(event);
                }
                None => {
                    self.pos = self.src.len();
                    return None;
                }
            }
        }
    }
}

/// Bytes to skip for a `<!...>` declaration, honouring `[ ... ]` subsets and quotes, so a DOCTYPE
/// with internal entity definitions is skipped whole and never interpreted.
fn skip_declaration(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate().skip(2) {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'[' => depth += 1,
                b']' => depth = depth.saturating_sub(1),
                b'>' if depth == 0 => return i + 1,
                _ => {}
            },
        }
    }
    bytes.len()
}

/// Parse `<name attr="v" ...>` or `<name ... />` at the start of `rest`; returns the event and the
/// bytes consumed, or `None` when the input ends inside the tag.
fn parse_start_tag(rest: &str) -> Option<(Event, usize)> {
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'/' && bytes[i] != b'>' {
        i += 1;
    }
    let name = rest[1..i].to_ascii_lowercase();
    let mut attrs: Vec<(String, String)> = Vec::new();
    loop {
        while i < bytes.len() && is_space(bytes[i]) {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        match bytes[i] {
            b'>' => {
                return Some((
                    Event::Start {
                        name,
                        attrs,
                        empty: false,
                    },
                    i + 1,
                ))
            }
            b'/' => {
                // "/>" ends an empty tag; a stray '/' is skipped.
                if bytes.get(i + 1) == Some(&b'>') {
                    return Some((
                        Event::Start {
                            name,
                            attrs,
                            empty: true,
                        },
                        i + 2,
                    ));
                }
                i += 1;
                continue;
            }
            _ => {}
        }
        let name_start = i;
        while i < bytes.len()
            && !is_space(bytes[i])
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let attr_name = rest[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && is_space(bytes[i]) {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() {
                return None;
            }
            if bytes[i] == b'"' || bytes[i] == b'\'' {
                let quote = bytes[i];
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                if j >= bytes.len() {
                    return None;
                }
                value = decode_entities(&rest[start..j]);
                i = j + 1;
            } else {
                let start = i;
                while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'>' {
                    i += 1;
                }
                value = decode_entities(&rest[start..i]);
            }
        }
        if !attr_name.is_empty() && attrs.len() < MAX_ATTRIBUTES {
            attrs.push((attr_name, value));
        }
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Decode `&lt; &gt; &amp; &quot; &apos;` and `&#N;` / `&#xH;`. Any other `&name;` stays literal,
/// so no entity defined by a document is ever expanded.
pub(crate) fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let semi = tail.bytes().take(12).position(|b| b == b';');
        let decoded = semi.and_then(|semi| {
            let body = &tail[1..semi];
            let ch = match body {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    let digits = body.strip_prefix('#')?;
                    let code = if let Some(hex) = digits
                        .strip_prefix('x')
                        .or_else(|| digits.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()?
                    } else {
                        digits.parse::<u32>().ok()?
                    };
                    Some(
                        char::from_u32(code)
                            .filter(|c| *c != '\0')
                            .unwrap_or('\u{FFFD}'),
                    )
                }
            };
            ch.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
