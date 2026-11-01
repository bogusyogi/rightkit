//! Small `sha256:<hex>` digest helpers shared across the runtime.

use rightkit_fs::digest::sha256_bytes;

/// `sha256:<hex>` of an arbitrary UTF-8 string.
pub fn digest_str(text: &str) -> String {
    digest_bytes(text.as_bytes())
}

/// `sha256:<hex>` of an arbitrary byte slice.
pub fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_bytes(bytes))
}
