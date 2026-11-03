//! Deep links / URL schemes: validation, a queue that waits for the frontend, and registration.
//!
//! Base: CodeRight `deep_link` (strict parse, control-character and traversal rejection, queue
//! until the renderer is ready). Generalised from two fixed `coderight://` shapes to any
//! registered scheme; the unknown-origin consent decision table stays app-specific. Fixed a donor
//! bug: percent-decoding now decodes UTF-8 bytes, not bytes-as-chars.
//!
//! Delivery paths: macOS `RunEvent::Opened`; Windows/Linux the argv of the launch (first launch
//! or forwarded by [`crate::instance`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const MAX_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepLink {
    pub scheme: String,
    pub host: String,
    pub segments: Vec<String>,
    pub query: BTreeMap<String, String>,
    pub raw: String,
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    (!s.chars().any(char::is_control)).then_some(s)
}

/// Parse `<scheme>://<host>/<seg>/<seg>?k=v#frag`. Only `schemes` are accepted; control
/// characters, `..`/`.` segments, empty hosts and over-long input are rejected.
pub fn parse(raw: &str, schemes: &[String]) -> Option<DeepLink> {
    let raw = raw.trim();
    if raw.len() > MAX_LEN || raw.chars().any(char::is_control) {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !schemes.iter().any(|s| s.eq_ignore_ascii_case(&scheme)) {
        return None;
    }
    let rest = rest.split('#').next()?;
    let (path_part, query_part) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let mut parts = path_part.split('/');
    let host = percent_decode(parts.next()?)?;
    if host.is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    for seg in parts.filter(|s| !s.is_empty()) {
        let seg = percent_decode(seg)?;
        if seg == "." || seg == ".." || seg.contains('/') || seg.contains('\\') {
            return None;
        }
        segments.push(seg);
    }
    let mut query = BTreeMap::new();
    if let Some(q) = query_part {
        for pair in q.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            query.insert(
                percent_decode(&k.replace('+', " "))?,
                percent_decode(&v.replace('+', " "))?,
            );
        }
    }
    Some(DeepLink {
        scheme,
        host,
        segments,
        query,
        raw: raw.to_owned(),
    })
}

/// Links wait here until the frontend reports ready, then drain in arrival order.
#[derive(Default)]
pub struct DeepLinkQueue {
    pending: Mutex<VecDeque<DeepLink>>,
}

impl DeepLinkQueue {
    pub fn push_all<'a>(
        &self,
        raw: impl IntoIterator<Item = &'a str>,
        schemes: &[String],
    ) -> usize {
        let mut q = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for r in raw {
            if let Some(link) = parse(r, schemes) {
                q.push_back(link);
            }
        }
        q.len()
    }
    pub fn drain(&self) -> Vec<DeepLink> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

/// Register `scheme` for the current user, pointing at `executable` (Windows: HKCU registry;
/// macOS registers through the bundle's `CFBundleURLTypes`, Linux through a `.desktop` MIME entry
/// installed by the packager, so both return Ok(false) here).
pub fn register_scheme(scheme: &str, executable: &std::path::Path) -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let base = format!(r"HKCU\Software\Classes\{scheme}");
        let run = |args: &[&str]| -> Result<(), String> {
            let out = std::process::Command::new("reg.exe")
                .creation_flags(0x0800_0000)
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
            }
        };
        run(&["ADD", &base, "/ve", "/d", &format!("URL:{scheme}"), "/f"])?;
        run(&["ADD", &base, "/v", "URL Protocol", "/d", "", "/f"])?;
        run(&[
            "ADD",
            &format!(r"{base}\shell\open\command"),
            "/ve",
            "/d",
            &format!("\"{}\" \"%1\"", executable.display()),
            "/f",
        ])?;
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = (scheme, executable);
        Ok(false)
    }
}
