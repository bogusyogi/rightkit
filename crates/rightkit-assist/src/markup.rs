//! A small, bounded HTML reader for mail: it finds JSON-LD script blocks, builds microdata items and
//! flattens HTML to text. It never runs anything, never fetches anything and never panics on odd input;
//! every loop makes progress and every output is capped.

use crate::text;

/// Input beyond this many bytes is ignored.
pub const MAX_HTML_BYTES: usize = 1024 * 1024;
const MAX_TOKENS: usize = 200_000;
const MAX_ATTRS: usize = 32;
const MAX_ATTR_VALUE: usize = 4096;
const MAX_DEPTH: usize = 256;
const MAX_PROP_TEXT: usize = 512;
const MAX_ITEMS: usize = 512;
const MAX_ITEM_DEPTH: usize = 8;

/// A schema.org node from JSON-LD or microdata: a type list and ordered properties, or plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    Obj(Obj),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Obj {
    pub types: Vec<String>,
    pub props: Vec<(String, Node)>,
}

impl Obj {
    pub fn has_type(&self, name: &str) -> bool {
        self.types.iter().any(|t| t.eq_ignore_ascii_case(name))
    }

    pub fn get(&self, name: &str) -> Option<&Node> {
        self.props
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    pub fn get_obj(&self, name: &str) -> Option<&Obj> {
        match self.get(name) {
            Some(Node::Obj(o)) => Some(o),
            _ => None,
        }
    }

    /// The first non-empty text among the named properties.
    pub fn text_of(&self, names: &[&str]) -> Option<String> {
        for name in names {
            for (k, v) in &self.props {
                if k.eq_ignore_ascii_case(name) {
                    if let Node::Text(t) = v {
                        if !t.is_empty() {
                            return Some(t.clone());
                        }
                    }
                }
            }
        }
        None
    }
}

#[derive(Debug)]
enum Token {
    Open {
        name: String,
        attrs: Vec<(String, String)>,
        self_close: bool,
    },
    Close(String),
    Text(String),
    /// The raw body of a `script` or `style` element.
    Raw {
        name: String,
        attrs: Vec<(String, String)>,
        body: String,
    },
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let end = tail.char_indices().take(12).find(|(_, c)| *c == ';');
        let Some((semi, _)) = end else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => {
                if let Some(num) = entity.strip_prefix('#') {
                    let code = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse::<u32>().ok(),
                    };
                    code.and_then(char::from_u32)
                } else {
                    None
                }
            }
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
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

fn is_name_byte(b: u8) -> bool {
    !(b.is_ascii_whitespace() || b == b'/' || b == b'>' || b == b'=')
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn find_ci(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        if hay[i..i + needle.len()].eq_ignore_ascii_case(needle) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn tokenize(html: &str) -> Vec<Token> {
    let bytes = html.as_bytes();
    let bytes = &bytes[..bytes.len().min(MAX_HTML_BYTES)];
    let n = bytes.len();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0usize;
    while i < n && tokens.len() < MAX_TOKENS {
        if bytes[i] != b'<' {
            let start = i;
            while i < n && bytes[i] != b'<' {
                i += 1;
            }
            tokens.push(Token::Text(decode_entities(&lossy(&bytes[start..i]))));
            continue;
        }
        // bytes[i] == '<'
        if bytes[i..].starts_with(b"<!--") {
            i = match find_ci(bytes, i + 4, b"-->") {
                Some(end) => end + 3,
                None => n,
            };
            continue;
        }
        let next = bytes.get(i + 1).copied().unwrap_or(0);
        if next == b'!' || next == b'?' {
            while i < n && bytes[i] != b'>' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        if next == b'/' {
            let start = i + 2;
            let mut j = start;
            while j < n && bytes[j] != b'>' {
                j += 1;
            }
            let name = lossy(&bytes[start..j]).trim().to_ascii_lowercase();
            tokens.push(Token::Close(name));
            i = (j + 1).min(n);
            continue;
        }
        if !next.is_ascii_alphabetic() {
            tokens.push(Token::Text("<".to_string()));
            i += 1;
            continue;
        }
        // An opening tag.
        let mut j = i + 1;
        let name_start = j;
        while j < n && is_name_byte(bytes[j]) {
            j += 1;
        }
        let name = lossy(&bytes[name_start..j]).to_ascii_lowercase();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_close = false;
        loop {
            while j < n && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j >= n {
                break;
            }
            if bytes[j] == b'>' {
                j += 1;
                break;
            }
            if bytes[j] == b'/' {
                if bytes.get(j + 1) == Some(&b'>') {
                    self_close = true;
                    j += 2;
                    break;
                }
                j += 1;
                continue;
            }
            let attr_start = j;
            while j < n && is_name_byte(bytes[j]) {
                j += 1;
            }
            if j == attr_start {
                // A stray '=': skip it so the loop moves on.
                j += 1;
                continue;
            }
            let attr_name = lossy(&bytes[attr_start..j]).to_ascii_lowercase();
            while j < n && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut value = String::new();
            if j < n && bytes[j] == b'=' {
                j += 1;
                while j < n && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < n && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let quote = bytes[j];
                    j += 1;
                    let v_start = j;
                    while j < n && bytes[j] != quote {
                        j += 1;
                    }
                    value = lossy(&bytes[v_start..j]);
                    j = (j + 1).min(n);
                } else {
                    let v_start = j;
                    while j < n && !bytes[j].is_ascii_whitespace() && bytes[j] != b'>' {
                        j += 1;
                    }
                    value = lossy(&bytes[v_start..j]);
                }
            }
            if attrs.len() < MAX_ATTRS {
                let value = decode_entities(text::truncate_bytes(&value, MAX_ATTR_VALUE));
                attrs.push((attr_name, value));
            }
        }
        i = j;
        if (name == "script" || name == "style") && !self_close {
            let close = format!("</{name}");
            let end = find_ci(bytes, i, close.as_bytes()).unwrap_or(n);
            let body = lossy(&bytes[i..end]);
            tokens.push(Token::Raw { name, attrs, body });
            // Skip the closing tag itself.
            let mut k = end;
            while k < n && bytes[k] != b'>' {
                k += 1;
            }
            i = (k + 1).min(n);
            continue;
        }
        tokens.push(Token::Open {
            name,
            attrs,
            self_close,
        });
    }
    tokens
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}

/// The bodies of `<script type="application/ld+json">` blocks, in order.
pub fn ld_json_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    for token in tokenize(html) {
        if let Token::Raw { name, attrs, body } = token {
            if name == "script" {
                let kind = attr(&attrs, "type").unwrap_or("").to_ascii_lowercase();
                if kind.contains("ld+json") && blocks.len() < 64 {
                    blocks.push(body);
                }
            }
        }
    }
    blocks
}

/// Plain text of an HTML body: script and style dropped, block boundaries as newlines, capped at `max_bytes`.
pub fn html_to_text(html: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for token in tokenize(html) {
        if out.len() >= max_bytes.saturating_mul(2) {
            break;
        }
        match token {
            Token::Text(t) => out.push_str(&t),
            Token::Open { name, .. } | Token::Close(name) => {
                if matches!(
                    name.as_str(),
                    "p" | "br"
                        | "div"
                        | "tr"
                        | "li"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "table"
                ) {
                    out.push('\n');
                }
            }
            Token::Raw { .. } => {}
        }
    }
    text::clean_multiline(&out, max_bytes)
}

enum BuildVal {
    Text(String),
    Item(usize),
}

struct BuildItem {
    types: Vec<String>,
    props: Vec<(String, BuildVal)>,
}

struct Collect {
    names: Vec<String>,
    owner: usize,
    text: String,
}

struct Frame {
    tag: String,
    scope: Option<usize>,
    collect: Option<Collect>,
}

fn last_segment(url: &str) -> String {
    url.rsplit(|c: char| c == '/' || c == '#' || c == ':')
        .next()
        .unwrap_or("")
        .to_string()
}

fn prop_names(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .take(4)
        .map(|s| s.to_string())
        .collect()
}

fn to_node(items: &[BuildItem], idx: usize, depth: usize) -> Node {
    let item = &items[idx];
    let mut obj = Obj {
        types: item.types.clone(),
        props: Vec::new(),
    };
    for (name, val) in &item.props {
        match val {
            BuildVal::Text(t) => obj.props.push((name.clone(), Node::Text(t.clone()))),
            BuildVal::Item(child) => {
                if depth < MAX_ITEM_DEPTH && *child > idx {
                    obj.props
                        .push((name.clone(), to_node(items, *child, depth + 1)));
                }
            }
        }
    }
    Node::Obj(obj)
}

/// Microdata items (`itemscope`, `itemtype`, `itemprop`) as nodes. Only top-level items are returned; nested
/// items are inside their parent's properties.
pub fn microdata(html: &str) -> Vec<Node> {
    let mut items: Vec<BuildItem> = Vec::new();
    let mut roots: Vec<usize> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();

    fn current_scope(stack: &[Frame]) -> Option<usize> {
        stack.iter().rev().find_map(|f| f.scope)
    }

    for token in tokenize(html) {
        match token {
            Token::Raw { .. } => {}
            Token::Text(t) => {
                for frame in stack.iter_mut() {
                    if let Some(c) = frame.collect.as_mut() {
                        if c.text.len() < MAX_PROP_TEXT {
                            c.text.push_str(&t);
                        }
                    }
                }
            }
            Token::Close(name) => {
                if let Some(pos) = stack.iter().rposition(|f| f.tag == name) {
                    while stack.len() > pos {
                        let Some(frame) = stack.pop() else { break };
                        if let Some(c) = frame.collect {
                            let value = text::clean(&c.text, MAX_PROP_TEXT);
                            for pname in c.names {
                                items[c.owner]
                                    .props
                                    .push((pname, BuildVal::Text(value.clone())));
                            }
                        }
                    }
                }
            }
            Token::Open {
                name,
                attrs,
                self_close,
            } => {
                if stack.len() >= MAX_DEPTH {
                    break;
                }
                let void = is_void(&name) || self_close;
                let scope = current_scope(&stack);
                let itemprop = attr(&attrs, "itemprop").map(prop_names).unwrap_or_default();
                if attr(&attrs, "itemscope").is_some() {
                    if items.len() >= MAX_ITEMS {
                        break;
                    }
                    let types: Vec<String> = attr(&attrs, "itemtype")
                        .map(|t| t.split_whitespace().map(last_segment).collect())
                        .unwrap_or_default();
                    let idx = items.len();
                    items.push(BuildItem {
                        types,
                        props: Vec::new(),
                    });
                    match (scope, itemprop.is_empty()) {
                        (Some(parent), false) => {
                            for pname in itemprop {
                                items[parent].props.push((pname, BuildVal::Item(idx)));
                            }
                        }
                        _ => roots.push(idx),
                    }
                    if !void {
                        stack.push(Frame {
                            tag: name,
                            scope: Some(idx),
                            collect: None,
                        });
                    }
                    continue;
                }
                let mut collect = None;
                if let (Some(owner), false) = (scope, itemprop.is_empty()) {
                    let value = match name.as_str() {
                        "meta" => attr(&attrs, "content").map(|s| s.to_string()),
                        "time" => attr(&attrs, "datetime").map(|s| s.to_string()),
                        "link" | "a" | "area" => attr(&attrs, "href").map(|s| s.to_string()),
                        "img" | "source" | "audio" | "video" | "embed" => {
                            attr(&attrs, "src").map(|s| s.to_string())
                        }
                        "data" | "meter" => attr(&attrs, "value").map(|s| s.to_string()),
                        _ => None,
                    };
                    match value {
                        Some(v) => {
                            let v = text::clean(&v, MAX_PROP_TEXT);
                            for pname in itemprop {
                                items[owner].props.push((pname, BuildVal::Text(v.clone())));
                            }
                        }
                        None if !void => {
                            collect = Some(Collect {
                                names: itemprop,
                                owner,
                                text: String::new(),
                            });
                        }
                        None => {}
                    }
                }
                if !void {
                    stack.push(Frame {
                        tag: name,
                        scope: None,
                        collect,
                    });
                }
            }
        }
    }
    // Close whatever the document left open.
    while let Some(frame) = stack.pop() {
        if let Some(c) = frame.collect {
            let value = text::clean(&c.text, MAX_PROP_TEXT);
            for pname in c.names {
                items[c.owner]
                    .props
                    .push((pname, BuildVal::Text(value.clone())));
            }
        }
    }
    roots
        .into_iter()
        .map(|idx| to_node(&items, idx, 0))
        .collect()
}
