//! Local anti-phishing trust heuristics: `Authentication-Results` parsing (SPF, DKIM, DMARC) and
//! a per-message risk report. Pure functions, no network.

use serde::Serialize;

/// A single risk observation. `level` is "info" | "warn" | "danger".
#[derive(Debug, Clone, Serialize)]
pub struct TrustFinding {
    pub level: String,
    pub message: String,
}

/// Rolled-up report for one message. `level` is the worst finding's level,
/// or "info" when there are no adverse findings.
#[derive(Debug, Clone, Serialize)]
pub struct TrustReport {
    pub level: String,
    pub findings: Vec<TrustFinding>,
}

/// Parsed SPF/DKIM/DMARC verdicts from an `Authentication-Results` header.
/// Each is a normalized token: "pass" | "fail" | "softfail" | "neutral" |
/// "none" | "unknown". Absent mechanisms are "none".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuthStatus {
    pub spf: String,
    pub dkim: String,
    pub dmarc: String,
}

/// Parse an `Authentication-Results` header value into SPF/DKIM/DMARC verdicts.
/// Tolerant of multiple `resultspec`s separated by `;`, extra `reason=`/`header.*`
/// params, and case. Picks the strongest (most-trusted) verdict seen per method
/// so a later "pass" isn't masked by an earlier "none".
pub fn parse_auth_results(header: &str) -> AuthStatus {
    let mut status = AuthStatus {
        spf: "none".into(),
        dkim: "none".into(),
        dmarc: "none".into(),
    };
    // Rank so we keep the most informative verdict if a method appears twice.
    fn rank(v: &str) -> u8 {
        match v {
            "pass" => 5,
            "fail" => 4,
            "softfail" => 3,
            "neutral" => 2,
            "none" => 1,
            _ => 0,
        }
    }
    let normalize = |raw: &str| -> String {
        match raw.to_ascii_lowercase().as_str() {
            "pass" => "pass",
            "fail" | "hardfail" => "fail",
            "softfail" => "softfail",
            "neutral" | "policy" => "neutral",
            "none" => "none",
            "permerror" | "temperror" | "error" => "unknown",
            _ => "unknown",
        }
        .to_string()
    };
    // Split on both ';' and whitespace; look for "method=verdict" tokens.
    for tok in header.split([';', ' ', '\t', '\r', '\n']) {
        let tok = tok.trim();
        let Some((method, rest)) = tok.split_once('=') else {
            continue;
        };
        // The verdict ends at the first non-alphanumeric (e.g. "pass(...)").
        let verdict_raw: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if verdict_raw.is_empty() {
            continue;
        }
        let slot = match method.trim().to_ascii_lowercase().as_str() {
            "spf" => &mut status.spf,
            "dkim" => &mut status.dkim,
            "dmarc" => &mut status.dmarc,
            _ => continue,
        };
        let candidate = normalize(&verdict_raw);
        if rank(&candidate) >= rank(slot) {
            *slot = candidate;
        }
    }
    status
}

// Brand tokens that phishers commonly spoof in display names.
const BRAND_TOKENS: &[&str] = &[
    "paypal",
    "google",
    "microsoft",
    "apple",
    "amazon",
    "netflix",
    "stripe",
    "bank",
    "dhl",
    "fedex",
];

// Link-shortener hostnames whose destinations are opaque.
const SHORTENERS: &[&str] = &["bit.ly", "t.co", "tinyurl.com", "goo.gl", "ow.ly", "is.gd"];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Assess one email message for obvious phishing / trust signals.
///
/// * `from_addr`  — raw RFC 5321 sender, e.g. `"alice@example.com"` or
///   `"Alice <alice@example.com>"`.
/// * `from_name`  — display name only (already extracted by the caller).
/// * `reply_to`   — raw Reply-To header value if present.
/// * `body_html`  — raw HTML body (scanned for href attributes).
pub fn assess(
    from_addr: &str,
    from_name: &str,
    reply_to: Option<&str>,
    body_html: &str,
) -> TrustReport {
    let mut findings: Vec<TrustFinding> = Vec::new();

    // -- 1. Reply-To domain mismatch -------------------------------------
    if let Some(rt) = reply_to {
        let from_domain = domain_of_email(from_addr);
        let rt_domain = domain_of_email(rt);
        if !from_domain.is_empty() && !rt_domain.is_empty() && from_domain != rt_domain {
            findings.push(TrustFinding {
                level: "warn".into(),
                message: "Reply-To domain differs from sender".into(),
            });
        }
    }

    // -- 2. Display-name brand spoofing ---------------------------------
    let name_lower = from_name.to_lowercase();
    let addr_lower = from_addr.to_lowercase();
    for brand in BRAND_TOKENS {
        if name_lower.contains(brand) && !addr_lower.contains(brand) {
            findings.push(TrustFinding {
                level: "danger".into(),
                message: format!(
                    "Sender name claims {} but the address domain doesn't match",
                    brand
                ),
            });
            break; // one spoofed brand is enough to flag danger
        }
    }

    // -- 3. Link scanning ------------------------------------------------
    for href in extract_hrefs(body_html) {
        let host = host_of_url(&href);

        // Raw IPv4
        if is_raw_ipv4(&host) {
            findings.push(TrustFinding {
                level: "danger".into(),
                message: "Link to a raw IP address".into(),
            });
            continue;
        }

        // Punycode / IDN lookalike
        if host.contains("xn--") {
            findings.push(TrustFinding {
                level: "warn".into(),
                message: "Punycode/lookalike domain in a link".into(),
            });
        }

        // Insecure http
        if href.starts_with("http://") {
            findings.push(TrustFinding {
                level: "warn".into(),
                message: "Insecure http link".into(),
            });
        }

        // Known shortener
        for shortener in SHORTENERS {
            if host == *shortener || host.ends_with(&format!(".{}", shortener)) {
                findings.push(TrustFinding {
                    level: "warn".into(),
                    message: "Shortened link hides its destination".into(),
                });
                break;
            }
        }
    }

    // -- Rollup ----------------------------------------------------------
    if findings.is_empty() {
        findings.push(TrustFinding {
            level: "info".into(),
            message: "No obvious risk signals.".into(),
        });
    }

    let top_level = rollup_level(&findings);
    TrustReport {
        level: top_level,
        findings,
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Extract the domain portion from a bare or angle-bracketed email address.
/// Returns a lowercase string; empty string on parse failure.
fn domain_of_email(addr: &str) -> String {
    // Strip angle-bracket form: "Name <user@domain>" → "user@domain"
    let inner = if let (Some(start), Some(end)) = (addr.find('<'), addr.rfind('>')) {
        &addr[start + 1..end]
    } else {
        addr
    };
    match inner.rfind('@') {
        Some(pos) => inner[pos + 1..].trim().to_lowercase(),
        None => String::new(),
    }
}

/// Extract the host portion from a URL string (scheme://host/path?…).
/// Returns empty string when the URL is unparseable.
fn host_of_url(url: &str) -> String {
    // Strip scheme
    let after_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        return String::new();
    };
    // Strip path, query, fragment
    let host_part = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("");
    // Strip port
    let host = if host_part.starts_with('[') {
        // IPv6 literal — leave as-is
        host_part.to_lowercase()
    } else {
        host_part
            .split(':')
            .next()
            .unwrap_or(host_part)
            .to_lowercase()
    };
    host
}

/// Return true when `host` looks like a dotted-decimal IPv4 address.
fn is_raw_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| p.parse::<u8>().is_ok())
}

/// Strip `<!-- ... -->` comment spans so href-like text inside comments isn't
/// scanned as a live link. Unterminated comments swallow the rest of the input
/// (matching how browsers treat them).
fn strip_html_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start + 4..].find("-->") {
            Some(end) => rest = &rest[start + 4 + end + 3..],
            None => return out, // unterminated comment: drop the remainder
        }
    }
    out.push_str(rest);
    out
}

/// Scan HTML for `href` attribute values. Handles double-quoted, single-quoted,
/// and unquoted values, tolerates whitespace around `=`, and ignores hrefs that
/// appear inside HTML comments. Still a lightweight scan (no parser dependency)
/// — good enough for phishing-link detection, which runs on top of the reader's
/// sandbox + DOMPurify, not as the only defense.
fn extract_hrefs(html: &str) -> Vec<String> {
    let html = strip_html_comments(html);
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let mut hrefs = Vec::new();
    let mut i = 0usize;

    while let Some(rel) = lower[i..].find("href") {
        let mut j = i + rel + 4; // just past "href"
                                 // Require an attribute boundary before "href" so we don't match e.g.
                                 // a word ending in "href"; the char before must be whitespace, '<', or
                                 // a quote/tag start (or be at the very start).
        let prev_ok = {
            let p = i + rel;
            p == 0
                || matches!(
                    bytes[p - 1],
                    b' ' | b'\t' | b'\n' | b'\r' | b'<' | b'"' | b'\'' | b'/'
                )
        };
        // Skip whitespace, then expect '='.
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
            j += 1;
        }
        if !prev_ok || j >= bytes.len() || bytes[j] != b'=' {
            i = i + rel + 4;
            continue;
        }
        j += 1; // past '='
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let (value, next) = match bytes[j] {
            q @ (b'"' | b'\'') => {
                let start = j + 1;
                match html[start..].find(q as char) {
                    Some(end) => (&html[start..start + end], start + end + 1),
                    None => break, // unterminated quote
                }
            }
            _ => {
                // Unquoted: value runs to the next whitespace or '>'.
                let start = j;
                let end = html[start..]
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .map(|o| start + o)
                    .unwrap_or(html.len());
                (&html[start..end], end)
            }
        };
        let href = value.trim();
        if !href.is_empty() {
            hrefs.push(href.to_string());
        }
        i = next;
    }
    hrefs
}

/// Fold a list of findings to the single worst level string.
fn rollup_level(findings: &[TrustFinding]) -> String {
    let mut worst = 0u8; // 0=info, 1=warn, 2=danger
    for f in findings {
        match f.level.as_str() {
            "warn" if worst < 1 => worst = 1,
            "danger" if worst < 2 => worst = 2,
            _ => {}
        }
    }
    match worst {
        2 => "danger".into(),
        1 => "warn".into(),
        _ => "info".into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
