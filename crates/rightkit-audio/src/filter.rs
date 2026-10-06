//! A typed ffmpeg filter: name plus ordered options, rendered to graph text.

use std::fmt::Display;

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    name: String,
    opts: Vec<(String, String)>,
}

/// Format a float without trailing zeros (`80`, `0.25`, `-18`).
pub fn num(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

/// Quote a value for a filter option (`'...'`, inner `'` as `'\''`). Quoting protects
/// `: , ; [ ]` and spaces at both the option and graph level.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

impl Filter {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            opts: Vec::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Plain numeric/identifier option (caller guarantees no special characters).
    pub fn opt(mut self, key: &str, value: impl Display) -> Self {
        self.opts.push((key.into(), value.to_string()));
        self
    }

    pub fn num(self, key: &str, value: f64) -> Self {
        self.opt(key, num(value))
    }

    /// Quoted option for expressions and paths.
    pub fn quoted(mut self, key: &str, value: &str) -> Self {
        self.opts.push((key.into(), quote(value)));
        self
    }

    pub fn render(&self) -> String {
        if self.opts.is_empty() {
            return self.name.clone();
        }
        let body: Vec<String> = self.opts.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{}={}", self.name, body.join(":"))
    }
}

/// Render filters as one comma-joined chain.
pub fn render_chain(filters: &[Filter]) -> String {
    filters
        .iter()
        .map(Filter::render)
        .collect::<Vec<_>>()
        .join(",")
}
