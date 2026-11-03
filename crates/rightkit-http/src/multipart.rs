//! `multipart/form-data` request bodies with exact, testable framing.
//!
//! Every part carries `Content-Disposition`; only file parts carry `Content-Type`.
//! Lines end in CRLF. Names, filenames and content types have `"`, CR and LF
//! replaced with `_` so a caller cannot inject headers or break quoting.

use std::path::Path;

use crate::client::Request;
use crate::error::HttpError;

#[derive(Debug, Clone)]
pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone)]
struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

impl Multipart {
    /// A form with a random boundary.
    pub fn new() -> Self {
        Self {
            boundary: random_boundary(),
            parts: Vec::new(),
        }
    }
    /// A form with a pinned boundary (for byte-exact tests and replays). The
    /// boundary must be 1 to 70 characters from ASCII letters, digits, `-`, `_`, `.`, `+`.
    pub fn with_boundary(boundary: impl Into<String>) -> Self {
        Self {
            boundary: boundary.into(),
            parts: Vec::new(),
        }
    }
    /// Add a text field.
    pub fn text(mut self, name: &str, value: &str) -> Self {
        self.parts.push(Part {
            name: sanitize(name),
            filename: None,
            content_type: None,
            body: value.as_bytes().to_vec(),
        });
        self
    }
    /// Add a file field from in-memory bytes.
    pub fn file_bytes(
        mut self,
        name: &str,
        filename: &str,
        content_type: &str,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.parts.push(Part {
            name: sanitize(name),
            filename: Some(sanitize(filename)),
            content_type: Some(sanitize(content_type)),
            body: bytes.into(),
        });
        self
    }
    /// Add a file field read from `path`. The file name becomes the part's filename.
    pub fn file_path(self, name: &str, path: &Path, content_type: &str) -> Result<Self, HttpError> {
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .ok_or_else(|| {
                HttpError::InvalidRequest("multipart file path has no UTF-8 file name".into())
            })?
            .to_string();
        let bytes = std::fs::read(path)
            .map_err(|e| HttpError::InvalidRequest(format!("cannot read multipart file: {e}")))?;
        Ok(self.file_bytes(name, &filename, content_type, bytes))
    }
    /// The `Content-Type` header value for this form.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }
    /// Encode the body. Fails if the boundary is invalid or occurs inside a part.
    pub fn into_body(self) -> Result<Vec<u8>, HttpError> {
        validate_boundary(&self.boundary)?;
        let boundary = self.boundary.as_bytes();
        let delimiter = format!("--{}", self.boundary);
        let mut out = Vec::new();
        for part in &self.parts {
            let mut head = format!("Content-Disposition: form-data; name=\"{}\"", part.name);
            if let Some(filename) = &part.filename {
                head.push_str(&format!("; filename=\"{filename}\""));
            }
            head.push_str("\r\n");
            if let Some(content_type) = &part.content_type {
                head.push_str(&format!("Content-Type: {content_type}\r\n"));
            }
            if contains(head.as_bytes(), boundary) || contains(&part.body, boundary) {
                return Err(HttpError::InvalidRequest(
                    "multipart boundary occurs inside a part; use another boundary".into(),
                ));
            }
            out.extend_from_slice(delimiter.as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(head.as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(&part.body);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(b"--\r\n");
        Ok(out)
    }
}

impl Default for Multipart {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    /// Set the body and `Content-Type` from a multipart form (replacing any existing Content-Type).
    pub fn multipart(self, form: Multipart) -> Result<Self, HttpError> {
        let content_type = form.content_type();
        let body = form.into_body()?;
        let mut req = self.set_header("Content-Type", content_type);
        req.body = body;
        Ok(req)
    }
}

/// A `multipart/related` body (RFC 2387): an ordered list of parts, each with its own
/// `Content-Type` and no `Content-Disposition`. A Google-style media upload is a JSON metadata
/// part followed by the media part.
#[derive(Debug, Clone)]
pub struct MultipartRelated {
    boundary: String,
    parts: Vec<(String, Vec<u8>)>,
}

impl MultipartRelated {
    /// A related body with a random boundary.
    pub fn new() -> Self {
        Self {
            boundary: random_boundary(),
            parts: Vec::new(),
        }
    }
    /// A related body with a pinned boundary; same rules as [`Multipart::with_boundary`].
    pub fn with_boundary(boundary: impl Into<String>) -> Self {
        Self {
            boundary: boundary.into(),
            parts: Vec::new(),
        }
    }
    /// Append a part with its `Content-Type`. Parts are sent in the order added.
    pub fn part(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.parts.push((sanitize(content_type), body.into()));
        self
    }
    /// The `Content-Type` header value for this body.
    pub fn content_type(&self) -> String {
        format!("multipart/related; boundary={}", self.boundary)
    }
    /// Encode the body. Fails if the boundary is invalid or occurs inside a part.
    pub fn into_body(self) -> Result<Vec<u8>, HttpError> {
        validate_boundary(&self.boundary)?;
        let boundary = self.boundary.as_bytes();
        let delimiter = format!("--{}", self.boundary);
        let mut out = Vec::new();
        for (content_type, body) in &self.parts {
            let head = format!("Content-Type: {content_type}\r\n");
            if contains(head.as_bytes(), boundary) || contains(body, boundary) {
                return Err(HttpError::InvalidRequest(
                    "multipart boundary occurs inside a part; use another boundary".into(),
                ));
            }
            out.extend_from_slice(delimiter.as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(head.as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(body);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(b"--\r\n");
        Ok(out)
    }
}

impl Default for MultipartRelated {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    /// Set the body and `Content-Type` from a `multipart/related` body (replacing any existing
    /// Content-Type).
    pub fn multipart_related(self, form: MultipartRelated) -> Result<Self, HttpError> {
        let content_type = form.content_type();
        let body = form.into_body()?;
        let mut req = self.set_header("Content-Type", content_type);
        req.body = body;
        Ok(req)
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' | '\r' | '\n' => '_',
            other => other,
        })
        .collect()
}

fn validate_boundary(boundary: &str) -> Result<(), HttpError> {
    let ok_len = (1..=70).contains(&boundary.len());
    let ok_chars = boundary
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'));
    if ok_len && ok_chars {
        Ok(())
    } else {
        Err(HttpError::InvalidRequest(
            "multipart boundary must be 1 to 70 ASCII letters, digits, '-', '_', '.' or '+'".into(),
        ))
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn random_boundary() -> String {
    let mut bytes = [0u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("rightkit-fallback-{nanos:x}");
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("rightkit-{hex}")
}
