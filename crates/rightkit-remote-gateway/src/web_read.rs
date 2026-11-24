//! `browser.read`: the agent reads one public web page as plain text, with no browser (AG-10).
//! Tier 2, read only. The host app owns the tool name, the switches and the agent-only gate
//! (`remoteTools`, the owner-only `agentBrowser` switch, `browser_tools::gate`) and
//! calls [`read`] once its own gate has passed; the policy below is this crate's.
//!
//! * **A host is on the owner's list.** The first read of a host asks the owner for a standing grant
//!   for that one host (session family `web:<host>`, ended after 30 days without use, as the
//!   form-fill and launch grants are). The agent repeats the call once the owner approved. A read
//!   of a host not on the list never opens it: an address carried in a page's instructions cannot
//!   be used to send anything out without the owner seeing the host.
//! * **What it opens.** An `https` address on a host name (no address, port or sign-in name) that
//!   resolves to public addresses only (`rightkit-http`'s address policy refuses loopback,
//!   private, link-local and reserved ones on the first request and on every redirect). No cookie,
//!   no sign-in, no script. A redirect is followed (at most three) only to another address on the
//!   same host; one that leaves the host is `redirect_off_site` and the agent reads the new host
//!   only after it is listed.
//! * **What comes back.** The page's text (scripts, styles and markup removed, obvious secrets
//!   masked, at most `maxChars`, default 6000), its title and up to twenty links on the same host,
//!   all marked untrusted: written by the page's authors, it is data, never instructions. A page
//!   that is not text, HTML, JSON or XML is `unsupported_content`; over 1 MiB is `too_large`.
//!
//! Codes: `agent_only`, `feature_off`, `remote_off`, `invalid_request`, `approval_required`,
//! `address_not_public`, `redirect_off_site`, `redirect_loop`, `http_status`, `unsupported_content`,
//! `too_large`, `web_timeout` and `web_failed`.

use super::{Origin, Refusal, Result, approvals, redact};
use rightkit_http::{Client, ClientConfig, HttpError, Request, RetryPolicy};
use serde_json::{Value, json};
use std::time::Duration;

/// A host name an agent may name: lower case letters, digits, hyphens and dots, at least one dot,
/// a last label that is not a number (no IP addresses), at most 253 characters.
pub fn clean_site(raw: &str) -> Option<String> {
    let site = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if site.len() < 3 || site.len() > 253 || !site.contains('.') {
        return None;
    }
    let labels: Vec<&str> = site.split('.').collect();
    let ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    let last_has_letter = labels
        .last()
        .is_some_and(|l| l.bytes().any(|b| b.is_ascii_alphabetic()));
    (ok && last_has_letter).then_some(site)
}

const BODY_MAX: usize = 1024 * 1024;
const HOPS_MAX: usize = 3;
const DEADLINE: Duration = Duration::from_secs(20);
const CHARS_DEFAULT: usize = 6_000;
const CHARS_MIN: usize = 500;
const CHARS_MAX: usize = 12_000;
const LINKS_MAX: usize = 20;
const LABEL_MAX: usize = 80;
const TITLE_MAX: usize = 200;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// The owner's standing grant for one host: given the session family and the plain-words summary
/// to show the owner, `Ok` when the grant is active, else the refusal that asks for it.
pub type Grant<'a> = &'a dyn Fn(&str, &str) -> Result<()>;

/// What the host app supplies to a read: the grant check and the user agent the request carries.
pub struct ReadConfig<'a> {
    pub grant: Grant<'a>,
    pub user_agent: &'a str,
}

/// The grant check over this crate's own approvals store, for a host app that uses it.
pub fn owner_grant(origin: Origin) -> impl Fn(&str, &str) -> Result<()> {
    move |family, summary| {
        if approvals::session_active_held(origin, family)? {
            return Ok(());
        }
        Err(approvals::request_session_held(origin, family, summary))
    }
}

/// `browser.read {url, maxChars?}`, after the host app's own gate has passed.
pub fn read(config: &ReadConfig<'_>, args: &Value) -> Result<Value> {
    let url = args["url"]
        .as_str()
        .map(str::trim)
        .filter(|u| !u.is_empty() && u.len() <= 2048 && !u.chars().any(char::is_control))
        .ok_or_else(|| invalid("url is a public https address"))?;
    let max_chars = match &args["maxChars"] {
        Value::Null => CHARS_DEFAULT,
        value => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (CHARS_MIN..=CHARS_MAX).contains(n))
            .ok_or_else(|| {
                invalid(format!(
                    "maxChars is a whole number from {CHARS_MIN} to {CHARS_MAX}"
                ))
            })?,
    };
    let host = https_host(url).ok_or_else(|| {
        invalid(
            "url is an https address on a host name (no IP address, port, sign-in name or plain http)",
        )
    })?;
    grant(config.grant, &host)?;
    let (final_url, body, content_type) = fetch(url, &host, config.user_agent)?;
    let raw = String::from_utf8_lossy(&body);
    let html = content_type.contains("html");
    let page = if html {
        extract(&raw, &host)
    } else {
        Extracted {
            title: String::new(),
            text: tidy(&raw),
            links: Vec::new(),
        }
    };
    let (clean, redactions) = redact::redact(&page.text);
    let truncated = clean.chars().count() > max_chars;
    let text: String = clean.chars().take(max_chars).collect();
    let links: Vec<Value> = page
        .links
        .iter()
        .map(|(label, link)| json!({"text": label, "url": link}))
        .collect();
    Ok(json!({
        "untrusted": true, "source": "web page",
        "url": final_url, "title": one_line(&page.title, TITLE_MAX), "text": text,
        "truncated": truncated, "redactions": redactions, "links": links,
    }))
}

/// The host of an `https` address that names a host (lower case), or `None` for anything else: a
/// plain `http` address, an IP address, a port, sign-in text before an `@`.
fn https_host(url: &str) -> Option<String> {
    let rest = url
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains(['@', ':']) {
        return None;
    }
    clean_site(authority)
}

/// The owner's standing grant for reading this host, or the request for one.
fn grant(check: Grant<'_>, host: &str) -> Result<()> {
    let family = format!("web:{host}");
    if family.len() > 120 {
        return Err(invalid("that host name is too long"));
    }
    check(
        &family,
        &format!(
            "let the agent read public pages on {host} as plain text (no sign-in, no cookies, no scripts), now and whenever it asks, until you stop using it for 30 days"
        ),
    )
}

fn http_refusal(error: HttpError) -> Refusal {
    match error {
        HttpError::AddressDenied { .. } => Refusal::new(
            "address_not_public",
            "that address is not a public one, so it was not opened",
        ),
        HttpError::Timeout { .. } => Refusal::new("web_timeout", "the site did not answer in time"),
        HttpError::BodyTooLarge { .. } => {
            Refusal::new("too_large", "the page is larger than 1 MiB")
        }
        _ => Refusal::new("web_failed", "the page could not be fetched"),
    }
}

/// An address a redirect may follow: a path on this host, or an `https` address on it.
fn same_site(location: &str, host: &str) -> Option<String> {
    let location = location.trim();
    if location.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    if location.starts_with('/') && !location.starts_with("//") {
        return Some(format!("https://{host}{location}"));
    }
    (https_host(location).as_deref() == Some(host)).then(|| location.to_string())
}

/// Reads `first`, following redirects that stay on `host`. Returns the address that answered, the
/// body and the content type (lower case).
fn fetch(first: &str, host: &str, user_agent: &str) -> Result<(String, Vec<u8>, String)> {
    let client = Client::new(ClientConfig {
        user_agent: user_agent.to_string(),
        request_timeout: DEADLINE,
        response_timeout: Some(Duration::from_secs(15)),
        recv_body_timeout: Some(Duration::from_secs(15)),
        max_redirects: 0,
        max_response_bytes: BODY_MAX,
        retry: RetryPolicy::none(),
        https_only: true,
        ..ClientConfig::default()
    });
    let mut url = first.to_string();
    for _ in 0..=HOPS_MAX {
        let response = client
            .send_any(
                &Request::get(url.clone())
                    .timeout(DEADLINE)
                    .max_response_bytes(BODY_MAX)
                    .header("Accept", "text/html,text/plain;q=0.9,*/*;q=0.1"),
            )
            .map_err(http_refusal)?;
        match response.status {
            200..=299 => {
                let kind = response
                    .header("content-type")
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let text_like = [
                    "text/",
                    "application/json",
                    "application/xml",
                    "+xml",
                    "+json",
                ]
                .iter()
                .any(|k| kind.contains(k));
                if !text_like {
                    return Err(Refusal::new(
                        "unsupported_content",
                        "the page is not text, HTML, JSON or XML",
                    ));
                }
                return Ok((url, response.body, kind));
            }
            301 | 302 | 303 | 307 | 308 => {
                url = response
                    .header("location")
                    .and_then(|l| same_site(l, host))
                    .ok_or_else(|| {
                        Refusal::new(
                            "redirect_off_site",
                            "the page redirects to another host; read that host once the owner has listed it",
                        )
                    })?;
            }
            status => {
                return Err(Refusal::new(
                    "http_status",
                    format!("the site answered {status}"),
                ));
            }
        }
    }
    Err(Refusal::new(
        "redirect_loop",
        "the page redirected more than three times",
    ))
}

struct Extracted {
    title: String,
    text: String,
    links: Vec<(String, String)>,
}

/// Adds a piece of page text, and to the open link's label when there is one.
fn add(text: &mut String, anchor: &mut Option<(String, String)>, piece: &str) {
    text.push_str(piece);
    if let Some((_, label)) = anchor.as_mut() {
        label.push_str(piece);
    }
}

fn block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "blockquote"
            | "form"
            | "dt"
            | "dd"
            | "hr"
    )
}

/// The text, title and same-host links of an HTML page: tags, comments, scripts and styles gone.
fn extract(html: &str, host: &str) -> Extracted {
    // ASCII lower-casing keeps every byte offset, so offsets into `lower` index `html` too.
    let lower = html.to_ascii_lowercase();
    let mut text = String::new();
    let mut title = String::new();
    let mut links: Vec<(String, String)> = Vec::new();
    let mut anchor: Option<(String, String)> = None;
    let mut at = 0;
    while at < html.len() {
        let Some(open) = html[at..].find('<').map(|p| at + p) else {
            add(&mut text, &mut anchor, &decode(&html[at..]));
            break;
        };
        add(&mut text, &mut anchor, &decode(&html[at..open]));
        if html[open..].starts_with("<!--") {
            at = html[open + 4..]
                .find("-->")
                .map_or(html.len(), |p| open + 4 + p + 3);
            continue;
        }
        let Some(close) = html[open..].find('>').map(|p| open + p) else {
            break;
        };
        let tag = &html[open + 1..close];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        at = close + 1;
        match (name.as_str(), closing) {
            ("script" | "style" | "noscript" | "svg" | "template", false) => {
                at = lower[at..]
                    .find(&format!("</{name}"))
                    .map_or(html.len(), |p| at + p);
            }
            ("title", false) => {
                let end = lower[at..].find("</title").map_or(html.len(), |p| at + p);
                title = decode(&html[at..end]);
                at = end;
            }
            ("a", false) => {
                anchor = href_of(tag).map(|href| (href, String::new()));
            }
            ("a", true) => {
                if let Some((href, label)) = anchor.take()
                    && links.len() < LINKS_MAX
                    && let Some(link) = same_site(&href, host)
                {
                    let label = one_line(&label, LABEL_MAX);
                    if !label.is_empty() && !links.iter().any(|(_, known)| *known == link) {
                        links.push((label, link));
                    }
                }
            }
            (other, _) if block(other) => add(&mut text, &mut anchor, "\n"),
            _ => {}
        }
    }
    Extracted {
        title,
        text: tidy(&text),
        links,
    }
}

/// The `href` of an opening `<a ...>` tag (its text between `<` and `>`).
fn href_of(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let rest = &tag[lower.find("href=")? + 5..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let inner = &rest[1..];
        Some(inner[..inner.find(quote)?].to_string())
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>')
            .next()
            .map(str::to_string)
    }
}

/// The few entities a page's text uses; anything else stays as written.
fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Each line's white space collapsed, empty lines dropped.
fn tidy(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line of page text: white space collapsed, at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}
