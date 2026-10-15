//! Minimal JSON writer (no serde): enough for receipts and the CLI, plus the flat field
//! extraction and base64 the WebDriver wire and screenshot replies need.

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

pub fn s(v: &str) -> String {
    format!("\"{}\"", esc(v))
}

pub fn opt_s(v: &Option<String>) -> String {
    v.as_deref().map(s).unwrap_or_else(|| "null".into())
}

/// Object builder preserving insertion order.
#[derive(Default)]
pub struct Obj(Vec<(String, String)>);

impl Obj {
    pub fn new() -> Self {
        Obj::default()
    }
    pub fn raw(mut self, k: &str, v: impl Into<String>) -> Self {
        self.0.push((k.to_string(), v.into()));
        self
    }
    pub fn str(self, k: &str, v: &str) -> Self {
        self.raw(k, s(v))
    }
    pub fn opt_str(self, k: &str, v: &Option<String>) -> Self {
        self.raw(k, opt_s(v))
    }
    pub fn num(self, k: &str, v: impl std::fmt::Display) -> Self {
        self.raw(k, v.to_string())
    }
    pub fn bool(self, k: &str, v: bool) -> Self {
        self.raw(k, if v { "true" } else { "false" })
    }
    pub fn build(self) -> String {
        let body: Vec<String> = self
            .0
            .into_iter()
            .map(|(k, v)| format!("{}:{}", s(&k), v))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}

pub fn arr(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

/// First string value for `"key": "..."` anywhere in `body` (flat extraction for
/// the WebDriver wire; not a JSON parser).
pub fn str_field(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = body[from..].find(&pat) {
        let mut rest = body[from + i + pat.len()..].trim_start();
        if let Some(r) = rest.strip_prefix(':') {
            rest = r.trim_start();
            if let Some(r) = rest.strip_prefix('"') {
                let mut out = String::new();
                let mut it = r.chars();
                while let Some(c) = it.next() {
                    match c {
                        '"' => return Some(out),
                        '\' => match it.next()? {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            'r' => out.push('\r'),
                            'u' => {
                                let h: String = it.by_ref().take(4).collect();
                                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                            }
                            o => out.push(o),
                        },
                        c => out.push(c),
                    }
                }
                return None;
            }
        }
        from += i + pat.len();
    }
    None
}

/// First numeric value for `"key": 12.5` anywhere in `body`.
pub fn num_field(body: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let i = body.find(&pat)?;
    let rest = body[i + pat.len()..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || "+-.eE".contains(c)))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Standard base64 (RFC 4648, padded) of `bytes`.
pub fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_and_builds() {
        assert_eq!(esc("a\"b\\c\n\u{1}"), "a\\\"b\\\\c\\n\\u0001");
        let j = Obj::new()
            .str("k", "v\"")
            .num("n", 3)
            .bool("b", true)
            .opt_str("o", &None)
            .build();
        assert_eq!(j, r#"{"k":"v\"","n":3,"b":true,"o":null}"#);
        assert_eq!(arr(vec!["1".into(), "2".into()]), "[1,2]");
    }
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(i.as_bytes()), o);
        }
    }
    #[test]
    fn extracts_flat_fields() {
        let b = r#"{"capabilities":{},"using":"link text","value":"a\"bA"}"#;
        assert_eq!(str_field(b, "using").as_deref(), Some("link text"));
        assert_eq!(str_field(b, "value").as_deref(), Some("a\"bA"));
        assert_eq!(str_field(b, "missing"), None);
        assert_eq!(num_field(r#"{"x": 12.5,"y":-3}"#, "y"), Some(-3.0));
        assert_eq!(num_field(r#"{"x": 12.5}"#, "x"), Some(12.5));
    }
}
