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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_boundary_byte_shape() {
        let body = Multipart::with_boundary("XB")
            .text("a", "hi")
            .file_bytes("f", "x.txt", "text/plain", b"DATA".to_vec())
            .into_body()
            .unwrap();
        let expected = "--XB\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nhi\r\n\
--XB\r\nContent-Disposition: form-data; name=\"f\"; filename=\"x.txt\"\r\n\
Content-Type: text/plain\r\n\r\nDATA\r\n--XB--\r\n";
        assert_eq!(String::from_utf8(body).unwrap(), expected);
    }

    #[test]
    fn names_and_filenames_are_sanitised() {
        let body = Multipart::with_boundary("XB")
            .text("we\"ird\r\nname", "v")
            .file_bytes("f", "a\"b\r\nc.txt", "text/plain", Vec::new())
            .into_body()
            .unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("name=\"we_ird__name\""), "{text}");
        assert!(text.contains("filename=\"a_b__c.txt\""), "{text}");
    }

    #[test]
    fn boundary_inside_a_part_is_rejected() {
        let err = Multipart::with_boundary("XB")
            .text("a", "contains XB here")
            .into_body()
            .unwrap_err();
        assert!(matches!(err, HttpError::InvalidRequest(_)));
    }

    #[test]
    fn invalid_boundary_is_rejected() {
        assert!(Multipart::with_boundary("").into_body().is_err());
        assert!(Multipart::with_boundary("a b\"c").into_body().is_err());
        assert!(Multipart::with_boundary("x".repeat(71))
            .into_body()
            .is_err());
    }

    #[test]
    fn content_type_carries_boundary() {
        assert_eq!(
            Multipart::with_boundary("XB").content_type(),
            "multipart/form-data; boundary=XB"
        );
    }

    #[test]
    fn request_multipart_sets_body_and_content_type() {
        let req = Request::post("http://x/")
            .set_header("content-type", "text/plain")
            .multipart(Multipart::with_boundary("XB").text("a", "1"))
            .unwrap();
        assert_eq!(
            crate::client::header_of(&req.headers, "content-type"),
            Some("multipart/form-data; boundary=XB")
        );
        assert_eq!(
            req.headers
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .count(),
            1
        );
        assert!(req.body.starts_with(b"--XB\r\n"));
    }

    #[test]
    fn related_body_frames_each_part_with_its_content_type() {
        let body = MultipartRelated::with_boundary("b1")
            .part("application/json; charset=UTF-8", "{\"name\":\"x\"}")
            .part("message/rfc822", "Subject: hi\r\n\r\nbody")
            .into_body()
            .unwrap();
        let text = String::from_utf8(body).unwrap();
        assert_eq!(
            text,
            "--b1\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\":\"x\"}\r\n\
             --b1\r\nContent-Type: message/rfc822\r\n\r\nSubject: hi\r\n\r\nbody\r\n--b1--\r\n"
        );
        assert_eq!(
            MultipartRelated::with_boundary("b1").content_type(),
            "multipart/related; boundary=b1"
        );
        assert!(MultipartRelated::with_boundary("b1")
            .part("text/plain", "has b1 inside")
            .into_body()
            .is_err());
    }
}
