//! Token counting with OpenAI's `o200k_base` encoding (feature `tokens`, #417).
//!
//! The count is exact for o200k-family models. Other providers' tokenizers differ, so a caller
//! that needs a count for them must name the encoding it used. The BPE ranks are bundled with
//! `tiktoken-rs` and parsed once per process. A failure to load them is a [`TokenError`], never
//! a guessed count.

use std::sync::OnceLock;

/// The o200k encoding could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError(String);

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "o200k_base tokenizer unavailable: {}", self.0)
    }
}

impl std::error::Error for TokenError {}

static O200K: OnceLock<Result<tiktoken_rs::CoreBPE, String>> = OnceLock::new();

fn o200k() -> Result<&'static tiktoken_rs::CoreBPE, TokenError> {
    O200K
        .get_or_init(|| tiktoken_rs::o200k_base().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| TokenError(e.clone()))
}

/// The number of `o200k_base` tokens in `text`, with no special tokens recognised.
pub fn count_o200k(text: &str) -> Result<usize, TokenError> {
    Ok(o200k()?.encode_ordinary(text).len())
}
