//! Windows: the page address of the browser in front, for `login.fill` and `payment.fill`
//!.
//!
//! The address is the COMMITTED page URL the browser exposes through UI Automation, the tree
//! `rightkit-automation`'s Windows platform already walks (`Platform::root`, `children`, `read`,
//! `window_of`; the document's Value is its `ValuePattern`, rightkit-automation win/uia.rs `read_node`),
//! so no `windows` crate feature is needed. The address bar text is editable (by the agent itself
//! through `screen.act`, and Chromium keeps edited text after focus moves), so it is never the source;
//! it only has to agree on the host. Chrome, Edge and Brave (Chromium window class `OmniboxViewViews`)
//! and Firefox (automation id `urlbar-input`) are known; any other front app is `no_browser`. The walk
//! is bounded in depth, nodes and time and never enters a page (`UIA_Document`), so a page cannot pose
//! as the address bar and the one document found is the outermost one (never an iframe's). The window
//! must hold exactly one such document, in the checked window, whose Value is a full `https://` URL
//! (a scheme is never inferred, so Firefox hiding `http://` cannot pass as https).
//!
//! Unproven: no run against a real Chrome, Edge, Brave or Firefox. Chromium builds its accessibility
//! tree only after an accessibility client asks, so a first read can find nothing (the caller refuses
//! `origin_unreadable`; a second try works). Chromium exposes the page URL as the document's Value and
//! Firefox is expected to; if one does not, the host app refuses (`origin_unreadable`), never guesses.

use super::{Refusal, Result};
use rightkit_automation::{FrontApp, NativePlatform, Platform, RawNode, Target};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A UI Automation element; `agent_secrets.rs` holds the checked page document and field as these.
pub type El = <NativePlatform as Platform>::El;

const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 1500;
const BUDGET: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Chromium,
    Firefox,
}

/// The committed page address of the front window (the top-level document's Value), and that
/// document itself: the field the host app types into must be inside this very element.
pub struct Address {
    pub url: String,
    pub doc: El,
}

/// The two handles name the same element (UI Automation `CompareElements`).
pub fn same_element(a: &El, b: &El) -> bool {
    NativePlatform::new().same(a, b)
}

fn unreadable(why: &str) -> Refusal {
    Refusal::new("origin_unreadable", why.to_string())
}

pub fn no_browser() -> Refusal {
    Refusal::new(
        "no_browser",
        "the front app is not a browser the app can read the page address from (Chrome, Edge, Brave or Firefox on Windows); bring the page to the front",
    )
}

fn family_of(exe: &str) -> Option<Family> {
    match exe.to_ascii_lowercase().as_str() {
        "chrome.exe" | "msedge.exe" | "brave.exe" => Some(Family::Chromium),
        "firefox.exe" => Some(Family::Firefox),
        _ => None,
    }
}

fn is_edit(raw: &RawNode) -> bool {
    matches!(raw.native_role.as_str(), "UIA_Edit" | "UIA_ComboBox")
}

fn is_bar(raw: &RawNode, family: Family) -> bool {
    is_edit(raw)
        && match family {
            Family::Chromium => raw.subrole.as_deref() == Some("OmniboxViewViews"),
            Family::Firefox => raw.identifier.as_deref() == Some("urlbar-input"),
        }
}

/// What one bounded walk of the window found: at most one address bar, and every outermost
/// `UIA_Document` (the walk never enters one, so an iframe's document is never listed).
struct Parts {
    bar: Option<(El, RawNode)>,
    docs: Vec<(El, RawNode)>,
}

/// Walks the window once, never descending into a page.
fn find_parts(p: &NativePlatform, front: &FrontApp, family: Family) -> Result<Parts> {
    let root = p
        .root(&Target {
            pid: front.pid,
            window_id: front.window_id,
        })
        .map_err(|_| unreadable("the browser's window could not be read"))?;
    let started = Instant::now();
    let mut queue = VecDeque::from([(root, 0usize)]);
    let mut found: Vec<(El, RawNode)> = Vec::new();
    let mut docs: Vec<(El, RawNode)> = Vec::new();
    let mut seen = 0usize;
    while let Some((el, depth)) = queue.pop_front() {
        seen += 1;
        if seen > MAX_NODES || started.elapsed() > BUDGET {
            return Err(unreadable(
                "the browser window is too large to read within the time the app allows (or its tree is still building: try again)",
            ));
        }
        let Some(raw) = p.read(&el) else { continue };
        if raw.native_role == "UIA_Document" {
            docs.push((el, raw));
            continue;
        }
        if is_bar(&raw, family) {
            found.push((el, raw));
            continue;
        }
        if depth < MAX_DEPTH {
            for child in p.children(&el) {
                queue.push_back((child, depth + 1));
            }
        }
    }
    if found.len() > 1 {
        return Err(unreadable(
            "the browser window has more than one address bar",
        ));
    }
    Ok(Parts {
        bar: found.pop(),
        docs,
    })
}

/// The host a bar text names, lower-case, scheme, `www.`, port 443 and path removed; `None` when the
/// text carries a scheme other than https (the bar then cannot describe the https page).
fn bar_host(text: &str) -> Option<String> {
    let text = text.trim();
    let rest = match text.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return None,
        None => text,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
    let host = authority.strip_suffix(":443").unwrap_or(&authority);
    Some(host.strip_prefix("www.").unwrap_or(host).to_string())
}

/// The committed page address of the front window of a known browser. The address bar's text is
/// editable by anyone who can drive the browser (and Chromium keeps edited text after focus moves),
/// so it is never the source: the address is the Value of the window's one top-level document, which
/// is the URL the browser committed for the page. The bar, when readable, must name the same host.
pub fn read(front: &FrontApp) -> Result<Address> {
    let p = NativePlatform::new();
    // On Windows `app_id` is the lower-case executable name (rightkit-automation win/sys.rs `frontmost`).
    let family = family_of(&front.app_id).ok_or_else(no_browser)?;
    let Parts { bar, mut docs } = find_parts(&p, front, family)?;
    let (doc, doc_raw) = match docs.len() {
        1 => docs.remove(0),
        0 => {
            return Err(unreadable(
                "The app could not find the page the browser shows (the browser may still be starting its accessibility tree: try again)",
            ));
        }
        _ => {
            return Err(unreadable(
                "the browser window shows more than one page document (a side panel or developer tools?); close them and try again",
            ));
        }
    };
    if front.window_id.is_some() && p.window_of(&doc) != front.window_id {
        return Err(unreadable(
            "the page document is not in the browser window the app checked",
        ));
    }
    let url = doc_raw
        .value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| unreadable("the browser did not give the page's own address"))?;
    // Never infer a scheme: the document's address must say it (`host_of` then requires https).
    let page_host = super::agent_secrets::host_of(&url).ok_or_else(|| {
        unreadable(
            "the page is not an https address the app will type into (https, default port, no user-info)",
        )
    })?;
    if let Some((_, raw)) = &bar {
        let text = raw.value.as_deref().map(str::trim).unwrap_or("");
        if text.is_empty() {
            return Err(unreadable(
                "the browser's address bar has no text the app can read",
            ));
        }
        let same = bar_host(text)
            .is_some_and(|h| h == page_host.strip_prefix("www.").unwrap_or(&page_host));
        if !same {
            return Err(unreadable(
                "the address bar does not show the page that is open (its text was edited, or the page is still loading); click the page, wait, and try again",
            ));
        }
    }
    Ok(Address { url, doc })
}

impl Address {
    /// The committed address of the page, with its explicit `https://`.
    pub fn full_url(&self) -> Result<String> {
        Ok(self.url.clone())
    }
}

/// Before typing, on the one field element `el` the caller took from focus and will type into:
/// it must be in the window the host app checked, must not be an address bar (a password must never land
/// in the address bar) and must be inside `doc`, the top-level document whose address was read,
/// not an embedded frame (`in_top_document`). The caller writes into `el` itself, never into a
/// second focus lookup, so what is checked here is what is typed into.
pub fn guard_field(window: Option<u64>, doc: &El, el: &El) -> Result<()> {
    let p = NativePlatform::new();
    let field = |code: &'static str, why: &str| Refusal::new(code, why.to_string());
    if window.is_some() && p.window_of(el) != window {
        return Err(field(
            "field_mismatch",
            "the focused field is not in the browser window the app checked",
        ));
    }
    let raw = p
        .read(el)
        .ok_or_else(|| field("no_field", "the focused field is gone"))?;
    if is_bar(&raw, Family::Chromium) || is_bar(&raw, Family::Firefox) {
        return Err(field(
            "field_mismatch",
            "the focused field is the browser's address bar",
        ));
    }
    in_top_document(&p, doc, el)
}

const FIELD_MAX_NODES: usize = 20_000;

fn in_frame(why: &str) -> Refusal {
    Refusal::new("field_in_frame", why.to_string())
}

/// The focused field must belong to `doc` itself, the window's one top-level `UIA_Document` whose
/// Value was the checked address (`read` proved it the only outermost document of the checked
/// window), never to a cross-origin frame inside it (an ad or chat-widget iframe is its own nested
/// `UIA_Document`, and the page address read above says nothing about it). rightkit-automation
/// 0.5.0 has no parent call (`Platform` offers `root`, `children`, `read`, `same`, `window_of`), so
/// the proof runs downward: search that document for the field without ever entering a nested
/// document. A document that is gone (the tab navigated) has no children and so proves nothing.
/// Anything unproven (no field found, budget spent) refuses; nothing is typed.
fn in_top_document(p: &NativePlatform, doc: &El, field: &El) -> Result<()> {
    let unproven = || {
        in_frame(
            "The app could not prove the focused field is in the page itself and not in an embedded frame, so it types nothing; click a field of the page and try again",
        )
    };
    if p.read(doc).is_none() {
        return Err(unproven());
    }
    // Inside the document: descend through everything except a nested document.
    let started = Instant::now();
    let mut stack = vec![doc.clone()];
    let mut seen = 0usize;
    while let Some(el) = stack.pop() {
        for child in p.children(&el) {
            seen += 1;
            if seen > FIELD_MAX_NODES || started.elapsed() > BUDGET {
                return Err(unproven());
            }
            if p.same(&child, field) {
                return Ok(());
            }
            match p.read(&child) {
                Some(raw) if raw.native_role == "UIA_Document" => {
                    // A frame's own document: the field may be in it, which is the refusal.
                    continue;
                }
                Some(_) => stack.push(child),
                None => {}
            }
        }
    }
    Err(in_frame(
        "the focused field is inside an embedded frame, not the page itself",
    ))
}
