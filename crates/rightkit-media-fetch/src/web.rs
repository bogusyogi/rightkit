//! The public-web seam: the host's address policy and image fetch, behind a trait.

use std::time::Duration;

/// One image GET.
#[derive(Debug, Clone)]
pub struct ImageRequest<'a> {
    pub url: &'a str,
    pub user_agent: &'a str,
    /// Applied to connect, response headers and body receive.
    pub timeout: Duration,
    /// Redirect hops the client follows itself; each hop is vetted against the public-web policy.
    pub max_redirects: u32,
    pub referer: &'a str,
    pub accept: &'a str,
    /// The body is refused when it exceeds this many bytes.
    pub max_bytes: u64,
}

/// A completed image GET; a 4xx or 5xx status is returned, not raised.
#[derive(Debug, Clone)]
pub struct ImageResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// Why an image GET failed, with the host's transport text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebError {
    /// Connecting, waiting for headers or redirecting failed.
    Fetch(String),
    /// Reading the response body failed (including exceeding the size cap).
    Body(String),
}

/// The host's public-web network policy.
pub trait PublicWeb {
    /// String-level guard: only http(s) URLs with a public, canonical host. A refusal starts
    /// with `refusing to fetch`.
    fn ensure_public_http_url(&self, url: &str) -> Result<(), String>;
    /// The string guard plus resolve-and-check, for a URL handed to an external tool that does
    /// its own DNS (yt-dlp, a browser). A check, not a pin.
    fn ensure_public_and_resolved(&self, url: &str) -> Result<(), String>;
    /// GET one image through a client that applies the public-web address policy.
    fn get_image(&self, request: &ImageRequest<'_>) -> Result<ImageResponse, WebError>;
}
