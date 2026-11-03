//! Manual draft checks: the disclosure rule (a required disclosure phrase must sit in the first
//! sentence), the approval gate, and the per-platform host table for a posted link.

use crate::Fail;

/// The first sentence: text up to and including the first `.`, `!` or `?`, or up to the first line
/// break, whichever comes first. The whole body when neither occurs.
pub fn first_sentence(body: &str) -> &str {
    let mut end = body.len();
    for (i, c) in body.char_indices() {
        if c == '\n' {
            end = i;
            break;
        }
        if matches!(c, '.' | '!' | '?') {
            end = i + c.len_utf8();
            break;
        }
    }
    &body[..end]
}

/// True when the first sentence contains one of the non-blank phrases, compared case-insensitively.
pub fn disclosed(body: &str, phrases: &[String]) -> bool {
    let sentence = first_sentence(body).to_lowercase();
    phrases
        .iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .any(|p| sentence.contains(p.as_str()))
}

/// The deterministic approval gate for a manual draft, in order: a non-blank body; a target URL
/// for `comment` and `reply` drafts; the body within `max_chars` characters; and, when disclosure is
/// required, a disclosure phrase in the first sentence.
pub fn approval_gate(
    draft_id: &str,
    kind: &str,
    body: &str,
    target: Option<&str>,
    disclosure_required: bool,
    phrases: &[String],
    max_chars: Option<usize>,
) -> Result<(), Fail> {
    if body.trim().is_empty() {
        return Err((
            "body_empty",
            format!("draft {draft_id} has no body to approve"),
        ));
    }
    let needs_target = matches!(kind, "comment" | "reply");
    if needs_target && target.is_none_or(|t| t.trim().is_empty()) {
        return Err((
            "target_required",
            format!("a {kind} draft needs a target URL before it can be approved"),
        ));
    }
    if let Some(max) = max_chars {
        let n = body.chars().count();
        if n > max {
            return Err((
                "body_too_long",
                format!("body is {n} characters; the limit is {max}"),
            ));
        }
    }
    if disclosure_required && !disclosed(body, phrases) {
        let why = if phrases.iter().all(|p| p.trim().is_empty()) {
            "no disclosure phrases were supplied with this request"
        } else {
            "the first sentence does not contain a disclosure phrase"
        };
        return Err((
            "disclosure_missing",
            format!("disclosure is required: {why}"),
        ));
    }
    Ok(())
}

/// The allowed hosts for a posted link, by platform. A platform not listed here accepts any https host.
pub fn allowed_hosts(platform: &str) -> Option<&'static [&'static str]> {
    match platform {
        "reddit" => Some(&["reddit.com", "www.reddit.com", "old.reddit.com"]),
        "x" => Some(&["x.com", "twitter.com"]),
        "threads" => Some(&[
            "threads.net",
            "www.threads.net",
            "threads.com",
            "www.threads.com",
        ]),
        _ => None,
    }
}

/// The lowercase host of an https URL. Refuses a non-https scheme, whitespace, a private or loopback host,
/// and any user, port or credential part (`rightkit_social_analytics::links::public_https`), so the host compared against the table is
/// the only host the link names.
pub fn https_host(url: &str) -> Result<String, Fail> {
    rightkit_social_analytics::links::public_https(url, "posted_url")?;
    let authority = url[8..].split(['/', '?', '#']).next().unwrap_or("");
    Ok(authority.to_ascii_lowercase())
}

/// The host check for a posted link. Returns `table` when the host is allowed, or `any_https` with a
/// note when the platform has no host table.
pub fn host_allowed(platform: &str, host: &str) -> Result<(&'static str, Option<String>), Fail> {
    match allowed_hosts(platform) {
        Some(hosts) if hosts.contains(&host) => Ok(("table", None)),
        Some(hosts) => Err((
            "host_not_allowed",
            format!(
                "posted_url host {host} is not an allowed {platform} host; allowed: {}",
                hosts.join(", ")
            ),
        )),
        None => Ok((
            "any_https",
            Some(format!(
                "no host table for {platform}: any https host is accepted; check the link by hand"
            )),
        )),
    }
}
