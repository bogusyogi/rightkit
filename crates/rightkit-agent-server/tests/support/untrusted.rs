// Shared by `include!` from the e2e support modules.

/// Payload of a tool result wrapped by `TrustEnvelope::boundary_wrapped_content`
/// (`<coderight-untrusted-context ...>` header, one data-warning paragraph, payload, closing tag).
/// Content without the envelope passes through unchanged.
#[allow(dead_code)]
pub fn unwrap_untrusted(content: &str) -> &str {
    const OPEN: &str = "<coderight-untrusted-context";
    const CLOSE: &str = "\n</coderight-untrusted-context>";
    if !content.starts_with(OPEN) {
        return content;
    }
    let Some(header_end) = content.find(">\n") else {
        return content;
    };
    let body = &content[header_end + 2..];
    let Some(para_end) = body.find("\n\n") else {
        return content;
    };
    let payload = &body[para_end + 2..];
    payload.strip_suffix(CLOSE).unwrap_or(payload)
}
