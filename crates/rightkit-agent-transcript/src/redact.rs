//! Secret redaction + text compaction (caps mirrored from the host bounder).

use regex::Regex;
use std::sync::LazyLock;

pub const MAX_EVENT_CHARS: usize = 6_000;
pub const MAX_TOOL_CALL_CHARS: usize = 1_200;
pub const MAX_TOOL_RESULT_CHARS: usize = 1_600;
pub const MAX_ASSISTANT_CHARS: usize = 4_000;

/// Secret-shaped patterns replaced with `[REDACTED]`.
static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b").unwrap(),
        Regex::new(r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b").unwrap(),
        Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b").unwrap(),
        Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
        Regex::new(r"\bxox[bap]-[A-Za-z0-9-]{10,}\b").unwrap(),
        Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}").unwrap(),
        Regex::new(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\b").unwrap(),
        Regex::new(r"(?i)\b(password|passphrase|api[_-]?key|secret|token)\s*[:=]\s*\S{6,}")
            .unwrap(),
        // Rust `regex` has no look-behind; anchor the blob with a leading
        // non-base64 char (or start of input) and keep it in the replacement.
        Regex::new(r#"(^|[^A-Za-z0-9+/=])([A-Za-z0-9+/]{512,}={0,2})"#).unwrap(),
        Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
            .unwrap(),
    ]
});

/// Redact secret-like content. Private-key blocks and long base64 blobs
/// collapse to `[BINARY_BLOB_REMOVED]`; credential shapes to `[REDACTED]`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (i, pattern) in SECRET_PATTERNS.iter().enumerate() {
        let is_blob = i == 8 || i == 9;
        if !is_blob {
            out = pattern.replace_all(&out, "[REDACTED]").into_owned();
        } else if i == 8 {
            // Base64 blob: preserve capture group 1 (the anchor char).
            out = pattern
                .replace_all(&out, "${1}[BINARY_BLOB_REMOVED]")
                .into_owned();
        } else {
            out = pattern
                .replace_all(&out, "[BINARY_BLOB_REMOVED]")
                .into_owned();
        }
    }
    out
}

/// Truncate by Unicode characters (Python string slicing semantics), marking
/// truncation with a trailing `\n[TRUNCATED]`.
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    cut + "\n[TRUNCATED]"
}

/// Compact event text: strip NUL bytes, redact secrets, trim, cap length.
pub fn compact_text(text: &str) -> String {
    let cleaned = text.replace('\x00', "");
    let redacted = redact(&cleaned);
    truncate_chars(redacted.trim(), MAX_EVENT_CHARS)
}

/// True when redaction markers are present in the text.
pub fn looks_redacted(text: &str) -> bool {
    text.contains("[REDACTED]") || text.contains("[BINARY_BLOB_REMOVED]")
}
