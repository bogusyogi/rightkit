//! Provider-side evidence for the qa-pty fixture.
//!
//! The fixture (`dogfood/harness/pty.py`) appends every received request
//! body to `requests.json` — so presence there proves a turn actually
//! reached the provider, and the raw text proves exactly what was submitted.
//! Deliberate-absence checks (a paste or slash command must NOT appear) are
//! just as load-bearing as presence checks: they catch input that silently
//! leaked into a submission.

use std::path::{Path, PathBuf};

/// The fixture's request log path.
pub fn file(provider_root: &str) -> PathBuf {
    Path::new(provider_root).join("requests.json")
}

/// Number of provider requests received so far; `0` when the log does not
/// exist yet.
pub fn count(provider_root: &str) -> usize {
    let Ok(text) = std::fs::read_to_string(file(provider_root)) else {
        return 0;
    };
    serde_json::from_str::<Vec<serde_json::Value>>(&text)
        .map(|requests| requests.len())
        .unwrap_or(0)
}

/// Raw-text scan over the request log. Request bodies embed the user's
/// submitted text verbatim, so a plain substring match is exact for unique
/// case markers and immune to request-schema drift. Follow-up requests
/// embed prior user turns in `messages` history — callers must therefore
/// use a fresh, unique marker per leg.
pub fn contains(provider_root: &str, needle: &str) -> bool {
    std::fs::read_to_string(file(provider_root))
        .map(|text| text.contains(needle))
        .unwrap_or(false)
}

/// Return latest submitted user message text, when request log carries a
/// string-form user content. Fixture assertions use this for exact payload
/// checks without matching prior-turn history.
pub fn last_user_content(provider_root: &str) -> Option<String> {
    let text = std::fs::read_to_string(file(provider_root)).ok()?;
    let requests = serde_json::from_str::<Vec<serde_json::Value>>(&text).ok()?;
    let messages = requests.last()?.get("messages")?.as_array()?;
    messages.iter().rev().find_map(|message| {
        if message.get("role")?.as_str()? != "user" {
            return None;
        }
        message.get("content")?.as_str().map(ToOwned::to_owned)
    })
}

/// Poll until the request log holds exactly `wanted` entries. Pairing a
/// before-count with a `+1` wait proves the operator's deliberate Enter —
/// and nothing else — produced the next provider request.
pub async fn wait_for_count(
    provider_root: &str,
    wanted: usize,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if count(provider_root) >= wanted {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    }
    Err(format!(
        "{} never reached {wanted} requests within {timeout:?}",
        file(provider_root).display()
    ))
}
