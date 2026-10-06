//! Minimal JSON writer (no serde): enough for receipts and the CLI.

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
        let body: Vec<String> = self.0.into_iter().map(|(k, v)| format!("{}:{}", s(&k), v)).collect();
        format!("{{{}}}", body.join(","))
    }
}

pub fn arr(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_and_builds() {
        assert_eq!(esc("a\"b\\c\n\u{1}"), "a\\\"b\\\\c\\n\\u0001");
        let j = Obj::new().str("k", "v\"").num("n", 3).bool("b", true).opt_str("o", &None).build();
        assert_eq!(j, r#"{"k":"v\"","n":3,"b":true,"o":null}"#);
        assert_eq!(arr(vec!["1".into(), "2".into()]), "[1,2]");
    }
}
