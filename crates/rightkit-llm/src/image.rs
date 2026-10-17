//! Image and video-frame input (Orthic-Labs/rightkit#298).
//!
//! An image rides in a message as [`ContentBlock::Image`]. A video frame is an image with a
//! timestamp label ([`Frame`]); the model client does not decode video, so the host extracts
//! frames (and, for motion decode, picks the scene boundaries) and passes them in.
//!
//! Wire mapping:
//!
//! - Anthropic Messages: `{"type":"image","source":{"type":"base64","media_type":..,"data":..}}`
//!   or `{"type":"image","source":{"type":"url","url":..}}`.
//! - OpenAI-compatible chat: a user message whose content is an array of parts, with each image
//!   as `{"type":"image_url","image_url":{"url":..}}` (a `data:` URL for base64 input). A user
//!   message without images keeps the plain string content it always had.
//! - Command-line routes are text only and reject image input with `CliError::Unsupported`.
//!
//! Images are valid in user messages. An image in an assistant message is dropped by the
//! OpenAI-compatible builder (the chat API takes none there); a tool result carries text only.
//!
//! The crate checks what it can before anything is sent ([`ImageSource::validate`]): a
//! supported media type, non-empty data that is standard base64, and an `http` or `https` URL.
//! It does not enforce provider size limits or pixel counts, and a model that cannot read
//! images answers with the provider's own error.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::types::ContentBlock;

/// Media types both providers accept for base64 images.
pub const SUPPORTED_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Where an image's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Standard base64 (RFC 4648, with padding) of the image file, no `data:` prefix.
    Base64 { media_type: String, data: String },
    /// An `http` or `https` URL the provider fetches. Not every OpenAI-compatible endpoint
    /// fetches URLs; local runtimes usually need base64.
    Url { url: String },
}

impl ImageSource {
    /// Base64 source from raw image bytes.
    pub fn from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        ImageSource::Base64 {
            media_type: media_type.into(),
            data: base64_encode(bytes),
        }
    }

    /// A URL source.
    pub fn url(url: impl Into<String>) -> Self {
        ImageSource::Url { url: url.into() }
    }

    /// Check what can be checked locally. Fails with [`LlmError::InvalidRequest`] before
    /// anything is sent.
    pub fn validate(&self) -> Result<(), LlmError> {
        match self {
            ImageSource::Base64 { media_type, data } => {
                if !SUPPORTED_MEDIA_TYPES.contains(&media_type.as_str()) {
                    return Err(LlmError::InvalidRequest(format!(
                        "image media type '{media_type}' is not one of {}",
                        SUPPORTED_MEDIA_TYPES.join(", ")
                    )));
                }
                if data.is_empty() {
                    return Err(LlmError::InvalidRequest("image data is empty".into()));
                }
                if !is_standard_base64(data) {
                    return Err(LlmError::InvalidRequest(
                        "image data is not standard base64 with padding".into(),
                    ));
                }
                Ok(())
            }
            ImageSource::Url { url } => {
                let lower = url.to_ascii_lowercase();
                if !(lower.starts_with("https://") || lower.starts_with("http://"))
                    || url.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    return Err(LlmError::InvalidRequest(
                        "image url must be an http or https URL without whitespace".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// The URL the OpenAI-compatible API takes: the URL itself, or a `data:` URL.
    pub(crate) fn openai_url(&self) -> String {
        match self {
            ImageSource::Base64 { media_type, data } => {
                format!("data:{media_type};base64,{data}")
            }
            ImageSource::Url { url } => url.clone(),
        }
    }
}

/// One video frame: the image and where in the video it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub image: ImageSource,
    /// Offset from the start of the video.
    pub at: Duration,
}

impl Frame {
    pub fn new(image: ImageSource, at: Duration) -> Self {
        Self { image, at }
    }

    /// The text label that precedes the frame so the model can tell frames apart, for
    /// example `Frame at 1.500 s:`.
    pub fn label(&self) -> String {
        format!("Frame at {:.3} s:", self.at.as_secs_f64())
    }

    /// The two content blocks for this frame: its label, then the image.
    pub fn blocks(&self) -> [ContentBlock; 2] {
        [
            ContentBlock::text(self.label()),
            ContentBlock::Image {
                source: self.image.clone(),
            },
        ]
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn is_standard_base64(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() % 4 != 0 {
        return false;
    }
    let pad = b.iter().rev().take_while(|c| **c == b'=').count();
    if pad > 2 {
        return false;
    }
    b[..b.len() - pad]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn validation_checks_type_data_and_url() {
        assert!(ImageSource::from_bytes("image/png", b"x")
            .validate()
            .is_ok());
        assert!(ImageSource::from_bytes("image/bmp", b"x")
            .validate()
            .is_err());
        assert!(ImageSource::Base64 {
            media_type: "image/png".into(),
            data: String::new()
        }
        .validate()
        .is_err());
        assert!(ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "not base64!".into()
        }
        .validate()
        .is_err());
        assert!(ImageSource::url("https://example.com/a.png")
            .validate()
            .is_ok());
        assert!(ImageSource::url("file:///etc/passwd").validate().is_err());
        assert!(ImageSource::url("https://a b").validate().is_err());
    }

    #[test]
    fn frame_label_has_millisecond_precision() {
        let f = Frame::new(
            ImageSource::url("https://e.test/f.png"),
            Duration::from_millis(1500),
        );
        assert_eq!(f.label(), "Frame at 1.500 s:");
    }
}
