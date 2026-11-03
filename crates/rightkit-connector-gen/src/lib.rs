//! Workflow-automation connectors generated from an engine contract (rightkit#1243).
//!
//! An app with a `rightkit-job-queue` engine contract describes its methods once as a [`Contract`]
//! (the engine name and version, and per method a summary and typed params, which is what
//! `Dispatcher::methods()` and `Dispatcher::method_meta()` carry). This crate renders from that
//! description:
//!
//! - [`n8n::render`]: an n8n community node (`package.json`, credentials, a declarative node).
//! - [`zapier::render`]: a Zapier CLI app (`package.json`, `index.js`) with one create action per method.
//! - [`make::render`]: a Make custom app definition (one module per method).
//!
//! Every connector calls the app's HTTP bridge with one convention: `POST {base_url}/rpc/{method}`,
//! the params as the JSON body, `Authorization: Bearer {token}`, and the engine result as the JSON
//! response (`{"error":{"code","message"}}` with a 4xx/5xx status on failure). The bridge is the
//! app's; the connectors never talk to the engine directly. The output is source text for the
//! caller to write to disk (`GeneratedFile`); nothing here touches the file system or the network.

pub mod make;
pub mod n8n;
pub mod zapier;

/// The type of one method parameter, mapped to each platform's field types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    String,
    Number,
    Boolean,
    /// A JSON value the user supplies as text (objects and arrays).
    Json,
}

/// One parameter of a method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: ParamType,
    pub required: bool,
    pub description: String,
}

/// One engine method exposed as a connector operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    /// The wire method name, for example `job.submit`.
    pub method: String,
    pub summary: String,
    pub params: Vec<Param>,
}

/// An engine contract, as far as a connector needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contract {
    /// Engine or app name, for example `postright`.
    pub engine: String,
    /// The connector package version (semver text).
    pub version: String,
    pub operations: Vec<Operation>,
}

/// Why a contract could not be rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenError {
    /// The engine name is empty or has no letters or digits.
    BadEngineName,
    /// A method name is empty or has a character outside `[A-Za-z0-9._-]`.
    BadMethodName(String),
    /// Two operations share a method name.
    DuplicateMethod(String),
    /// A parameter name is empty or repeated within one operation.
    BadParamName(String),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::BadEngineName => f.write_str("engine name has no letters or digits"),
            GenError::BadMethodName(m) => write!(f, "method name {m:?} is not allowed"),
            GenError::DuplicateMethod(m) => write!(f, "method {m:?} is listed twice"),
            GenError::BadParamName(p) => write!(f, "parameter name {p:?} is empty or repeated"),
        }
    }
}

impl std::error::Error for GenError {}

/// One file to write: a path relative to the connector's root and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub contents: String,
}

impl Contract {
    /// Check the names every generator relies on. Each `render` calls this first.
    pub fn validate(&self) -> Result<(), GenError> {
        if !self.engine.chars().any(|c| c.is_ascii_alphanumeric()) {
            return Err(GenError::BadEngineName);
        }
        let mut seen: Vec<&str> = Vec::new();
        for op in &self.operations {
            let ok = !op.method.is_empty()
                && op
                    .method
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if !ok {
                return Err(GenError::BadMethodName(op.method.clone()));
            }
            if seen.contains(&op.method.as_str()) {
                return Err(GenError::DuplicateMethod(op.method.clone()));
            }
            seen.push(op.method.as_str());
            let mut names: Vec<&str> = Vec::new();
            for p in &op.params {
                if p.name.is_empty() || names.contains(&p.name.as_str()) {
                    return Err(GenError::BadParamName(p.name.clone()));
                }
                names.push(p.name.as_str());
            }
        }
        Ok(())
    }

    /// `PostRight`-style name for identifiers and file names.
    pub fn pascal_name(&self) -> String {
        pascal(&self.engine)
    }

    /// `postright`-style name for package names.
    pub fn slug(&self) -> String {
        let mut out = String::new();
        let mut dash = false;
        for c in self.engine.chars() {
            if c.is_ascii_alphanumeric() {
                if dash && !out.is_empty() {
                    out.push('-');
                }
                dash = false;
                out.push(c.to_ascii_lowercase());
            } else {
                dash = true;
            }
        }
        out
    }
}

/// The HTTP path every connector posts a method to.
pub fn rpc_path(method: &str) -> String {
    format!("/rpc/{method}")
}

pub(crate) fn pascal(text: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if upper {
                out.push(c.to_ascii_uppercase());
            } else {
                out.push(c);
            }
            upper = false;
        } else {
            upper = true;
        }
    }
    out
}

/// A method as an identifier fragment: `job.submit` becomes `jobSubmit`.
pub(crate) fn camel_method(method: &str) -> String {
    let p = pascal(method);
    let mut chars = p.chars();
    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// A JSON (and so JavaScript) string literal for `text`.
pub(crate) fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escape U+2028 and U+2029 too: they end a line in JavaScript but not in JSON.
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
