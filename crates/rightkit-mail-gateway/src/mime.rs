//! Gmail's MIME payload tree as the draft parser reads it, and the base64url decoder for its
//! part bodies.

use serde::Deserialize;

/// Gmail base64url (URL-safe, often unpadded). Normalize to standard + pad. Undecodable input
/// yields no bytes.
pub fn decode_b64url(data: &str) -> Vec<u8> {
    let mut s = data.replace('-', "+").replace('_', "/");
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    rightkit_crypto::base64_decode(&s).unwrap_or_default()
}

/// Header node from Gmail's MIME payload tree.
#[derive(Deserialize)]
pub struct Header {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// Body node from Gmail's MIME payload tree.
#[derive(Deserialize)]
pub struct PartBody {
    #[serde(rename = "attachmentId", default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub data: String,
}

/// Recursive Gmail MIME payload part.
#[derive(Deserialize)]
pub struct Part {
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    pub body: Option<PartBody>,
    #[serde(default)]
    pub parts: Vec<Part>,
}
