//! `files.fetch`: save a file ChatGPT holds onto this computer, through the ChatGPT gateway only.
//! It is Zenon's `download_artifact`, the reverse of `files.read`: it moves a file ChatGPT
//! generated, or the user attached in the chat, onto disk without the model re-typing its bytes
//! through another tool. **It is not a fetch-any-URL tool.**
//!
//! * **The file object is ChatGPT's.** The tool declares `_meta["openai/fileParams"]` for its
//!   `file` input (see the gateway's `tools`), so ChatGPT itself fills `{download_url, file_id,
//!   mime_type?, file_name?}`; the model cannot invent one. A URL's shape is still no proof of who
//!   wrote it, so the checks below hold whatever the caller sent.
//! * **Pinned hosts.** The download URL must be `https`, port 443 or none, with no user info and
//!   no fragment, on a host in `OPENAI_FILE_HOSTS`, an exact-name list (never a wildcard over
//!   Azure's shared blob namespace). The list was observed from ChatGPT's native file injection
//!   on 2026-09-08 and 09 (Zenon, `artifact-fetch.ts`) and needs re-observing on current builds;
//!   a file on another host is refused (`host_not_allowed`) and an entry is added here, not
//!   guessed. The client is first-party (`ClientConfig::first_party`): it follows at most
//!   `MAX_REDIRECTS` redirects and holds the request and every hop to an exact host of the list
//!   over https, before anything is sent there; the URL that answers must also pass the same
//!   URL check as the first one. No header, cookie or credential is sent.
//! * **Writes like `files.patch`.** The destination is a new name in an approved folder: refused
//!   if anything of that name exists, if the name usually holds secrets, or outside the folders.
//!   The bytes are held to `MAX_BYTES` (a bigger body is `too_large`), written to a temporary
//!   sibling, hashed there, renamed without replacing (`ReplaceMode::NoReplace`), and read back
//!   through the same confinement and hashed again. The answer carries `path`, `size` and
//!   `sha256` of what the name now holds, and `verified: true` only when both hashes matched.
//! * **Switch and approval.** `remoteFilesWrite` (off by default) and one local approval per
//!   save, bound to the destination, the file id, the declared size, the download URL and the shown
//!   name; the summary starts with the real destination and quotes a name cut to `NAME_SHOWN`; asked **before any
//!   network access**: nothing is downloaded until the user approves.
//! * **Time.** One overall deadline, `DEADLINE`, under the gateway's 45-second call limit.

use super::files_write::{act_gate, slot, write_verified};
use super::gptchat::{invalid, need_gateway};
use super::settings::Settings;
use super::{Origin, Refusal, Result, approvals, confine};
use rightkit_fs::ReplaceMode;
use rightkit_fs::digest::sha256_bytes;
use rightkit_http::{
    ApprovedOrigins, Client, ClientConfig, HttpError, RedirectRefusal, Request, RetryPolicy,
};
use serde_json::{Value, json};
use std::time::Duration;

/// The hosts a ChatGPT file may be downloaded from, by exact name.
pub const OPENAI_FILE_HOSTS: &[&str] = &[
    "files.oaiusercontent.com",
    // Observed from ChatGPT native generated-file injection on 2026-09-08/09. Exact accounts only.
    "oaisdmntprindiasocentral.blob.core.windows.net",
    "oaisdmntprcentralindia.blob.core.windows.net",
];
/// The most a saved file may hold (it is held in memory to be hashed).
pub const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Redirects the client follows, each hop held to the host list.
const MAX_REDIRECTS: usize = 3;
/// One deadline for the whole download, every hop included.
const DEADLINE: Duration = Duration::from_secs(30);
const URL_MAX: usize = 16 * 1024;
const FILE_ID_MAX: usize = 512;
const LABEL_MAX: usize = 1024;
/// Characters of the file name ChatGPT gave that the approval shows.
const NAME_SHOWN: usize = 60;

/// The ChatGPT file object, checked.
struct FileRef {
    url: String,
    id: String,
    name: Option<String>,
    size: Option<u64>,
}

/// The fields ChatGPT may pass (the docs list four; `size` and `name` are accepted as Zenon did).
const FILE_KEYS: &[&str] = &[
    "download_url",
    "file_id",
    "mime_type",
    "file_name",
    "name",
    "size",
];

fn malformed() -> Refusal {
    invalid("file is not a ChatGPT file object ({download_url, file_id, mime_type?, file_name?})")
}

fn optional_label(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) if text.len() <= LABEL_MAX => Ok(Some(text.clone())),
        _ => Err(malformed()),
    }
}

fn file_ref(value: &Value) -> Result<FileRef> {
    let Some(map) = value.as_object() else {
        return Err(malformed());
    };
    if map.keys().any(|key| !FILE_KEYS.contains(&key.as_str())) {
        return Err(malformed());
    }
    let url = match &value["download_url"] {
        Value::String(url) if url.len() <= URL_MAX => url.clone(),
        _ => return Err(malformed()),
    };
    let id = match &value["file_id"] {
        Value::String(id)
            if !id.is_empty() && id.len() <= FILE_ID_MAX && !id.chars().any(char::is_control) =>
        {
            id.clone()
        }
        _ => return Err(malformed()),
    };
    optional_label(&value["mime_type"])?;
    let name = match optional_label(&value["file_name"])? {
        Some(name) => Some(name),
        None => optional_label(&value["name"])?,
    };
    let size = match &value["size"] {
        Value::Null => None,
        other => Some(other.as_u64().ok_or_else(malformed)?),
    };
    Ok(FileRef {
        url,
        id,
        name,
        size,
    })
}

/// The authority of `url` when it is a download URL this tool may use: `https`, an exact host in
/// `OPENAI_FILE_HOSTS`, port 443 or none, no user info, no fragment, no space, control or
/// non-ASCII character and no backslash. `None` for anything else.
fn trusted_authority(url: &str) -> Option<&str> {
    if url.len() > URL_MAX || url.bytes().any(|b| b <= 0x20 || b >= 0x7f || b == b'\\') {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    if rest.contains('#') {
        return None;
    }
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, "443")) => host,
        Some(_) => return None,
        None => authority,
    };
    OPENAI_FILE_HOSTS
        .contains(&host.to_ascii_lowercase().as_str())
        .then_some(authority)
}

fn host_refused() -> Refusal {
    Refusal::new(
        "host_not_allowed",
        "that download URL is not on a ChatGPT file host the app accepts; files.fetch saves only files ChatGPT itself hands over",
    )
}

fn download_failed(why: &str) -> Refusal {
    Refusal::new("download_failed", why.to_string())
}

/// What a failed download is told to the caller as. A redirect hop outside the approved origins is
/// the host refusal (the same one an off-list first URL gets); any other refused redirect is a
/// redirect the host app does not follow.
fn refusal_for(error: HttpError) -> Refusal {
    match error {
        HttpError::BodyTooLarge { .. } => Refusal::new(
            "too_large",
            format!("a saved file is limited to {MAX_BYTES} bytes"),
        ),
        HttpError::Timeout { .. } => {
            Refusal::new("timed_out", "the download took too long and was stopped")
        }
        // A hop to a host outside the list, or off https (rightkit-http netpolicy.rs:831-851).
        HttpError::AddressDenied { reason, .. } if reason.contains("first-party") => host_refused(),
        // A redirect to an origin outside the approved set (rightkit-http 0.3.8 `ApprovedOrigins`,
        // netpolicy.rs:1595; refused before anything is sent to it).
        HttpError::RedirectRefused {
            reason: RedirectRefusal::UnapprovedOrigin,
            ..
        } => host_refused(),
        // A `Location` that is not an http(s) URL (redirect.rs `resolve_location`).
        HttpError::InvalidRequest(message) if message.contains("redirect") => Refusal::new(
            "download_failed",
            "the download host sent a redirect the app does not follow",
        ),
        // The other typed redirect guards (userinfo, scheme, malformed target).
        HttpError::RedirectRefused { .. } => Refusal::new(
            "download_failed",
            "the download host sent a redirect the app does not follow",
        ),
        _ => download_failed("the download host could not be reached"),
    }
}

/// Reads the file at `url0`, at most `MAX_BYTES`. The client follows at most `MAX_REDIRECTS`
/// redirects, and its first-party policy (`ClientConfig::first_party`) holds the request and
/// every hop to an exact host of `OPENAI_FILE_HOSTS` over https before anything is sent there.
fn download(url0: &str) -> Result<Vec<u8>> {
    trusted_authority(url0).ok_or_else(host_refused)?;
    // Redirects are confined to the request's own origin plus the list's, as typed origins: a hop
    // anywhere else, or one carrying user info, is refused before anything is sent to it.
    let origins = ApprovedOrigins::parse(
        OPENAI_FILE_HOSTS
            .iter()
            .map(|host| format!("https://{host}")),
    )
    .ok_or_else(host_refused)?;
    let client = Client::new(ClientConfig {
        // A limit that runs out returns the last 3xx (`max_redirects_will_error`).
        max_redirects: MAX_REDIRECTS as u32,
        max_redirects_will_error: false,
        max_response_bytes: MAX_BYTES,
        connect_timeout: Some(Duration::from_secs(8)),
        request_timeout: DEADLINE,
        response_timeout: Some(Duration::from_secs(15)),
        retry: RetryPolicy::none(),
        ..ClientConfig::first_party(OPENAI_FILE_HOSTS.iter().map(|h| h.to_string()).collect())
    });
    let response = client
        .send_any(
            &Request::get(url0.to_string())
                .timeout(DEADLINE)
                .approved_redirect_origins(origins),
        )
        .map_err(refusal_for)?;
    match response.status {
        200 => {
            // The URL that answered keeps the shape `trusted_authority` demands of the first
            // one (port 443 or none, no user info), which the client's host pin does not look at.
            trusted_authority(response.final_url()).ok_or_else(host_refused)?;
            Ok(response.body)
        }
        301 | 302 | 303 | 307 | 308 => Err(if response.header("location").is_none() {
            download_failed("the download host sent a redirect with no place to go")
        } else {
            download_failed("the download was redirected too many times")
        }),
        status => Err(Refusal::new(
            "download_failed",
            format!("the download host answered HTTP {status}"),
        )),
    }
}

/// `files.fetch`. `args`: `file` (the ChatGPT file object), `path` (an absolute path for the new
/// file, in an approved folder), `approval` (the id of a request the user approved).
pub fn fetch(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    need_gateway(origin, "files.fetch")?;
    settings.require_files_write()?;
    let roots = confine::approved_roots(settings)?;
    let file = file_ref(&args["file"])?;
    let path = match &args["path"] {
        Value::String(path) if !path.is_empty() => path.as_str(),
        _ => return Err(invalid("path is needed")),
    };
    let approval = match &args["approval"] {
        Value::Null => None,
        Value::String(id) => Some(id.as_str()),
        _ => return Err(invalid("approval must be a string")),
    };
    // The host is checked before anything is asked or fetched.
    trusted_authority(&file.url).ok_or_else(host_refused)?;
    if file.size.is_some_and(|size| size > MAX_BYTES as u64) {
        return Err(Refusal::new(
            "too_large",
            format!("a saved file is limited to {MAX_BYTES} bytes"),
        ));
    }
    let place = slot(&roots, path)?;
    let shown = confine::display(&place.target);
    let declared = file.size.map_or_else(String::new, |size| size.to_string());
    let label: String = file
        .name
        .as_deref()
        .unwrap_or("a file")
        .chars()
        .map(|c| if c.is_control() || c == '"' { ' ' } else { c })
        .take(NAME_SHOWN)
        .collect();
    let bind = approvals::bind_of(&[
        b"files.fetch",
        origin.as_str().as_bytes(),
        shown.as_bytes(),
        file.id.as_bytes(),
        declared.as_bytes(),
        file.url.as_bytes(),
        label.as_bytes(),
    ]);
    // The real destination comes first; the name ChatGPT gave the file is a short, quoted tail
    // that cannot push anything out of view (the approval cuts the summary, never the front).
    let summary = format!(
        "save a ChatGPT file to {shown} (never replaces){}; ChatGPT calls it \"{label}\"",
        file.size
            .map_or_else(String::new, |size| format!(", {size} bytes")),
    );
    act_gate(origin, "files.fetch", &bind, &summary, approval)?;

    let bytes = download(&file.url)?;
    if file.size.is_some_and(|size| size != bytes.len() as u64) {
        return Err(Refusal::new(
            "download_mismatch",
            "the downloaded file's size did not match what ChatGPT declared; nothing was saved",
        ));
    }
    let sha256 = sha256_bytes(&bytes);
    let parent = place.folder.path.clone();
    write_verified(
        &roots,
        place.folder.root,
        &place.target,
        &bytes,
        ReplaceMode::NoReplace,
        || confine::still_here(&place.handle, &parent),
    )?;
    Ok(json!({
        "ok": true,
        "path": shown,
        "root": roots[place.folder.root].index,
        "created": true,
        "size": bytes.len(),
        "bytes": bytes.len(),
        "sha256": sha256,
        "verified": true,
    }))
}
