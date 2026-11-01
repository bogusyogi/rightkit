//! Byte-level ConPTY input payloads for the paste/newline acceptance legs.
//!
//! Encoders only — each payload carries unique case markers so the
//! provider-side `requests.json` can prove whether a submission happened.
//! The owning case decides what "submitted" means; these functions just
//! produce the exact bytes a real terminal delivers.

use std::time::Duration;

/// The donor paste-burst semantics the plan carries into the Windows TUI:
/// an Enter received inside this window after pasted bytes is part of the
/// paste — never an operator submit. (Codex chat composer:
/// `PASTE_ENTER_SUPPRESSION`/`PASTE_BURST_IDLE`.)
pub const ENTER_SUPPRESSION: Duration = Duration::from_millis(120);

/// Wrap `payload` in the DECSET-2004 bracketed-paste markers a real terminal
/// emits (`ESC[200~` … `ESC[201~`). Payload bytes are verbatim — a paste may
/// itself contain `\r\n`, which must be treated as content.
pub fn bracketed(payload: &str) -> Vec<u8> {
    format!("\u{1b}[200~{payload}\u{1b}[201~").into_bytes()
}

/// A paste with no bracket markers: the entire CRLF payload arrives in one
/// ConPTY input burst, exactly as a terminal delivers pasted text when
/// bracketed paste was never enabled. The trailing `\r\n` is realistic —
/// clipboard text commonly ends with a newline, and it is still part of the
/// burst, not a submit.
pub fn crlf_block(lines: &[&str]) -> Vec<u8> {
    let mut out = lines.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n");
    out
}
