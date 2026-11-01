//! `clipboard.read`: ChatGPT reads the text on this computer's clipboard, through the ChatGPT
//! gateway only. Read only: there is no clipboard write (decided 2026-10-21, docs/zenon-replacement.md).
//!
//! * **Own switch.** `remoteClipboard` (off by default) besides `remoteTools`, and the screen
//!   disclosure grant (`remoteScreenDisclosure`): the clipboard's text leaves this computer.
//! * **One approval per read**, asked before anything is returned. The first call answers
//!   `approval_required` with a request id; the approval says how many characters are on the clipboard
//!   now, never the text, and is bound to a hash of that text, so a clipboard that changed since the
//!   request is `approval_invalid`. The user approves in the host app on this computer; the caller repeats
//!   the call with `approval`.
//! * **Secrets.** Text that holds a card number, bank detail or government ID is refused
//!   (`sensitive_content`), whatever the approval says. Everything else passes the secret redactor
//!   (`redactions` counts what was masked) and is cut at 64 KiB (`truncated`). A password manager's
//!   copy is not recognised (the clipboard snapshot has no concealed-type flag), which is why every
//!   read asks.
//! * **Nothing recorded.** Activity keeps the tool, the origin, the outcome and the byte count.
//!
//! macOS and Windows only (the crate's clipboard); elsewhere `unsupported_platform`.

use super::settings::Settings;
use super::{Origin, Refusal, Result, approvals, redact, screen};
use serde_json::{Value, json};

/// Most bytes of clipboard text returned.
const READ_MAX: usize = 64 * 1024;

/// `clipboard.read`. `args`: `approval` (the id of a request the user approved for this read).
pub fn read(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    if origin != Origin::Gateway {
        return Err(Refusal::new(
            "not_built",
            "clipboard.read is offered through the ChatGPT gateway only",
        ));
    }
    settings.require_clipboard()?;
    // While the user has taken over a region of the screen the clipboard may hold what they just
    // copied there: nothing is read (the same rule as the screen tools).
    screen::user_in_control()?;
    let approval = match &args["approval"] {
        Value::Null => None,
        Value::String(id) => Some(id.as_str()),
        _ => {
            return Err(Refusal::new("invalid_request", "approval must be a string"));
        }
    };
    let (text, lossless) = screen::clipboard_text()?;
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return Ok(json!({
            "ok": true,
            "empty": true,
            "text": Value::Null,
            "returned": 0,
            "bytes": 0,
        }));
    };
    #[cfg(any(target_os = "macos", windows))]
    if super::sensitive::sensitive_value(&text).is_some() {
        return Err(Refusal::new(
            "sensitive_content",
            "the clipboard holds what looks like a card number, bank detail or government ID; the app does not hand that to an agent",
        ));
    }
    let chars = text.chars().count();
    let digest = rightkit_fs::digest::sha256_bytes(text.as_bytes());
    let bind = approvals::bind_of(&[
        b"clipboard.read",
        origin.as_str().as_bytes(),
        digest.as_bytes(),
    ]);
    let summary = format!(
        "let ChatGPT read the clipboard now: {chars} characters of text (keys and tokens are masked; the text is not shown here)"
    );
    match approval.filter(|a| !a.is_empty()) {
        None => {
            return Err(approvals::request_act_held(
                origin,
                "clipboard.read",
                &bind,
                &summary,
            ));
        }
        Some(id) => approvals::take_act_held(origin, "clipboard.read", &bind, id)?,
    }
    let (masked, redactions) = redact::redact(&text);
    let mut end = masked.len().min(READ_MAX);
    while !masked.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = end < masked.len();
    Ok(json!({
        "ok": true,
        "empty": false,
        "text": &masked[..end],
        "chars": chars,
        "truncated": truncated,
        "redactions": redactions,
        "otherFormats": !lossless,
        "returned": end,
        "bytes": end,
    }))
}
