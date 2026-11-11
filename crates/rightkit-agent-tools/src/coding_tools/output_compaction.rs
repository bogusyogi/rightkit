//! Deterministic reduction of shell tool output BEFORE it enters the LLM context.
//!
//! Pure: no I/O, no LLM, no async. Two stages:
//!   1. universal `strip_ansi` + an optional per-command rule (the rule table)
//!   2. failure-aware head+tail line cap, then a char-boundary-safe byte ceiling.
//!
//! Errors cluster at the tail of a failing command, so a non-zero exit biases the
//! kept lines (and the byte ceiling) toward the END. This is what stops a verbose
//! build from truncating its own error away.
//!
//! NOT a config engine: rules are compiled in, there are a handful, and adding one
//! is a table arm + a function. No JSON layers, no user rule dirs.

/// Hard ceiling on reduced output size. Mirrors the prior `shell.rs` MAX_OUTPUT_BYTES.
pub const MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// Line budget for a successful command: generous head, small tail.
const OK_HEAD_LINES: usize = 200;
const OK_TAIL_LINES: usize = 40;
/// Line budget for a failed command: errors cluster at the tail, so keep more tail.
const FAIL_HEAD_LINES: usize = 40;
const FAIL_TAIL_LINES: usize = 200;

/// Reduce raw combined stdout+stderr for context.
/// `success` = the process exited 0. `command` selects a per-command rule.
pub fn reduce_output(raw: &str, success: bool, command: &str) -> String {
    let stripped = strip_ansi(raw);
    let ruled = apply_command_rule(&stripped, command);
    // 7b: mask secrets before the head/tail cap runs, so a secret that would
    // otherwise land in the elided middle is still caught if it happens to
    // survive into the kept head or tail.
    let masked = mask_secrets(&ruled);
    cap_head_tail(&masked, success)
}

/// Root of the host app's own per-session shell-output recovery copies:
/// `<state root>/sessions`. Each session writes
/// under `<root>/<session>/shell-output/`. Never inside a user workspace.
pub fn session_store_root() -> Option<std::path::PathBuf> {
    Some(rightkit_agent_config::state_root()?.join("sessions"))
}

/// True when `path` canonically resolves inside a `shell-output` recovery
/// directory under the session store (symlink/junction escapes rejected).
pub fn is_shell_output_recovery_path(path: &std::path::Path, session_id: &str) -> bool {
    let Some(root) = session_store_root().and_then(|root| root.canonicalize().ok()) else {
        return false;
    };
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    let session_dir = session_store_dir_name(session_id);
    match canonical.strip_prefix(&root) {
        Ok(rest) => {
            let mut parts = rest.components();
            parts
                .next()
                .is_some_and(|c| c.as_os_str() == session_dir.as_str())
                && parts
                    .next()
                    .is_some_and(|c| c.as_os_str() == "shell-output")
                && parts.next().is_some()
        }
        Err(_) => false,
    }
}

/// Directory name under [`session_store_root`] that holds one session's
/// the host app-owned files; only that session may read them back.
pub fn session_store_dir_name(session_id: &str) -> String {
    let dir: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if dir.is_empty() {
        "detached".to_string()
    } else {
        dir
    }
}

/// The full, ANSI-stripped, secret-masked copy of `raw` that is safe to keep
/// durably so omitted output can be recovered after [`reduce_output`].
pub fn recovery_copy(raw: &str) -> String {
    mask_secrets(&strip_ansi(raw))
}

/// True when reduction dropped content the model can no longer see
/// (elided middle, byte ceiling, or a command rule that summarised output).
pub fn reduction_omitted_content(recovery: &str, reduced: &str) -> bool {
    recovery.trim_end().len() > reduced.trim_end().len()
}

/// Keep head+tail lines (failure-aware), then enforce the byte ceiling.
fn cap_head_tail(s: &str, success: bool) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let (head_n, tail_n) = if success {
        (OK_HEAD_LINES, OK_TAIL_LINES)
    } else {
        (FAIL_HEAD_LINES, FAIL_TAIL_LINES)
    };

    let reduced = if lines.len() > head_n + tail_n {
        let elided = lines.len() - head_n - tail_n;
        let mut out = String::with_capacity(s.len().min(MAX_OUTPUT_BYTES));
        for l in &lines[..head_n] {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&format!("\n[… {elided} lines elided …]\n\n"));
        for l in &lines[lines.len() - tail_n..] {
            out.push_str(l);
            out.push('\n');
        }
        out
    } else {
        s.to_string()
    };

    cap_bytes(reduced, success)
}

/// Final safety net for pathological single-huge-line output. Char-boundary safe.
/// Success keeps the head; failure keeps the tail (where the error is).
///
/// 7b: the marker states the real original size and cap rather than a bare
/// ellipsis, so the model knows how much it isn't seeing. Kept deliberately
/// compact (not the shared `truncation_marker()` phrasing used elsewhere in
/// this module) because this is the last-resort byte ceiling for a single
/// pathological line and must stay small relative to `MAX_OUTPUT_BYTES`.
fn cap_bytes(s: String, success: bool) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s;
    }
    let original_bytes = s.len();
    if success {
        let mut end = MAX_OUTPUT_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = s[..end].to_string();
        out.push_str(&format!(
            "\n[full {original_bytes}B, shown {MAX_OUTPUT_BYTES}B]"
        ));
        out
    } else {
        let mut start = s.len() - MAX_OUTPUT_BYTES;
        while start < s.len() && !s.is_char_boundary(start) {
            start += 1;
        }
        let mut out = format!("[full {original_bytes}B, shown {MAX_OUTPUT_BYTES}B]\n");
        out.push_str(&s[start..]);
        out
    }
}

/// Remove ANSI escape sequences (CSI color codes etc.). UTF-8 safe.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                // CSI: ESC '[' … final byte in 0x40..=0x7e
                Some('[') => {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&nc) {
                            break;
                        }
                    }
                }
                // Any other ESC x: drop ESC and the following char.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Executable basename of a command, `.exe` stripped. `""` if empty.
/// Honors a leading-quoted path so a space inside `"C:\Program Files\…"` is kept.
fn argv0(command: &str) -> &str {
    let command = command.trim_start();
    let first = if let Some(rest) = command.strip_prefix(['"', '\'']) {
        let quote = command.as_bytes()[0] as char;
        rest.split(quote).next().unwrap_or(rest)
    } else {
        command.split_whitespace().next().unwrap_or("")
    };
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// Per-command noise rule. Compiled-in; one arm per command worth special-casing.
fn apply_command_rule(s: &str, command: &str) -> String {
    let lower = command.to_ascii_lowercase();
    if argv0(command) == "git" && lower.contains(" status") {
        return git_status_reduce(s);
    }
    s.to_string()
}

/// Drop `git status` advice + branch-tracking boilerplate; keep the file lines.
fn git_status_reduce(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let t = line.trim_start();
        if t.starts_with("(use \"git") || t.starts_with("(use 'git") {
            continue;
        }
        if line.starts_with("On branch ") || line.starts_with("Your branch is ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Lane 7 — per-tool-result bounding: secret masking, explicit truncation
// markers, and exempt-tool classes.
//
// This module stays pure (no I/O, no session state) so every owning tool file
// can call it deterministically. Spill-to-disk + content-addressed retrieval
// already exists centrally (`engine::tool_result_store::ToolResultStore`,
// wired in `engine_tool_dispatch.rs`) — that is 7a's storage half, and it is
// outside this lane (engine crate, not tools crate). What belongs here is
// what runs BEFORE that generic externalization sees the text: masking
// secrets, and stating real numbers in truncation markers instead of a bare
// ellipsis.
//
// Rolling age-based decay of stale observations was removed rather than
// wired: its only correct enforcement point is the wire-projection pipeline
// in the engine crate, where turn age and obligation classification are
// known, and every result old enough to decay is already replaced wholesale
// by the compaction-time observation masker. A size/age predicate here had
// no production caller; dead policy reads as implemented policy.

/// Placeholder substituted for a detected secret span.
pub const SECRET_MASK: &str = "[REDACTED]";

/// Tool classes exempt from masking regardless of size or age.
///
/// Mirrors gemini-cli's skill-activation / plan-mode / ask-user exemptions:
/// these are short, structural, control-flow tool calls whose value is in
/// being fully visible, not in their byte count — spilling them to disk would
/// break the agent's ability to see what it just decided.
///
/// This is THE canonical list. `engine::compaction_tuning` consumes it via
/// [`is_exempt_tool`] so there is exactly one definition of the exempt class;
/// do not fork a second copy at a call site.
pub const EXEMPT_TOOL_CLASSES: &[&str] = &[
    "ask_user",
    "exit_plan_mode",
    // The skill tool registers as `invoke_skill` (audit C5); the old class
    // name stays so callers that still pass it keep matching.
    "invoke_skill",
    "skill_tool",
    "plugin_tool",
    "todo_write",
];

/// True if `tool_name` is exempt from masking regardless of age or size.
pub fn is_exempt_tool(tool_name: &str) -> bool {
    EXEMPT_TOOL_CLASSES.contains(&tool_name)
}

/// State the real numbers instead of a bare ellipsis (7b): callers append
/// this after their own truncation so the model knows exactly how much it is
/// not seeing and why.
pub fn truncation_marker(original_bytes: usize, cap_bytes: usize) -> String {
    format!("[{original_bytes} bytes total, capped at {cap_bytes} bytes]")
}

/// Mask secrets and cap size for output arriving from outside our own tools.
///
/// Native tools each apply masking and capping at their own call site, where
/// they can shape output knowingly. Output from a third party -- an MCP server,
/// most notably -- has no such call site, and skipping this step means an
/// upstream server's response reaches the model with secrets intact and no size
/// bound at all. Uniformity here is not tidiness: it is the difference between
/// a redacted key and a leaked one.
///
/// Applied unconditionally by origin, never by trust level. A server being
/// configured by the user says nothing about whether its *output* is safe --
/// it may be relaying a file, a log, or an HTTP response it did not author.
pub fn bound_foreign_output(content: &str) -> String {
    let masked = mask_secrets(content);
    if masked.len() <= MAX_OUTPUT_BYTES {
        return masked;
    }
    let original_bytes = masked.len();
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !masked.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = masked[..end].to_string();
    out.push('\n');
    out.push_str(&truncation_marker(original_bytes, MAX_OUTPUT_BYTES));
    out
}

/// Redact secret-shaped spans before the model sees output (7b). Pure,
/// line-oriented, conservative: false positives (over-masking) are cheap,
/// false negatives (a leaked key) are not.
///
/// Covers: common vendor token prefixes (`sk-`, the GitHub `gh*_` and fine-grained prefixes,
/// `AKIA`/`ASIA` AWS access key ids, `AIza` Google API keys, Slack `xox` prefixed
/// tokens), `Authorization: Bearer <token>` / `Basic <token>` headers, and
/// `key`/`token`/`secret`/`password`/`api_key`-style `name = value` /
/// `name: value` / `name=value` assignments whose value looks opaque (no
/// spaces, 8+ chars).
pub fn mask_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&mask_secrets_in_line(line));
    }
    out
}

fn mask_secrets_in_line(line: &str) -> String {
    let masked = mask_prefixed_tokens(line);
    let masked = mask_auth_headers(&masked);
    let masked = mask_url_userinfo(&masked);
    mask_kv_secrets(&masked)
}

/// `scheme://user:password@host` → `scheme://user:***@host`; a lone
/// `scheme://token@host` userinfo is masked whole.
fn mask_url_userinfo(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find("://") {
        let authority_start = pos + 3;
        out.push_str(&rest[..authority_start]);
        let after = &rest[authority_start..];
        let authority_end = after
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"' || c == '\''
            })
            .unwrap_or(after.len());
        let authority = &after[..authority_end];
        match authority.rfind('@') {
            Some(at) => {
                let userinfo = &authority[..at];
                match userinfo.find(':') {
                    Some(colon) => {
                        out.push_str(&userinfo[..=colon]);
                        out.push_str("***");
                    }
                    None => out.push_str("***"),
                }
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &after[authority_end..];
    }
    out.push_str(rest);
    out
}

/// Vendor tokens identifiable by a fixed prefix, followed by opaque chars.
fn mask_prefixed_tokens(line: &str) -> String {
    const PREFIXES: &[&str] = &[
        "sk-",
        concat!("gh", "p_"),
        concat!("gh", "o_"),
        concat!("gh", "s_"),
        concat!("github", "_pat_"),
        "AKIA",
        "ASIA",
        "AIza",
        concat!("xo", "xb-"),
        concat!("xo", "xa-"),
        concat!("xo", "xp-"),
        "xoxr-",
        "xoxs-",
    ];
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    'outer: while i < bytes.len() {
        for prefix in PREFIXES {
            // Short lowercase prefixes ("sk-") occur inside ordinary words such as
            // "task-shell-out" or "risk-assessment"; a real key starts at a token boundary.
            let at_word_start =
                i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            if line[i..].starts_with(prefix) && (*prefix != "sk-" || at_word_start) {
                let start = i;
                let mut end = i + prefix.len();
                while end < bytes.len() && is_token_byte(bytes[end]) {
                    end += 1;
                }
                // Require a token of meaningful length beyond the prefix so we
                // don't mask a bare mention of e.g. "sk-" in prose.
                if end - start >= prefix.len() + 8 {
                    out.push_str(SECRET_MASK);
                    i = end;
                    continue 'outer;
                }
            }
        }
        // Push one char (char-boundary safe).
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'/' || b == b'='
}

/// `Authorization: Bearer <token>` / `Authorization: Basic <token>` headers.
fn mask_auth_headers(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let Some(header_at) = lower.find("authorization:") else {
        return line.to_string();
    };
    let after_colon = header_at + "authorization:".len();
    let rest = &line[after_colon..];
    let trimmed = rest.trim_start();
    let leading_ws = rest.len() - trimmed.len();
    for scheme in ["Bearer ", "Basic ", "Token "] {
        if trimmed.len() >= scheme.len() && trimmed[..scheme.len()].eq_ignore_ascii_case(scheme) {
            let value_start = after_colon + leading_ws + scheme.len();
            return format!("{}{}", &line[..value_start], SECRET_MASK);
        }
    }
    line.to_string()
}

/// `key`/`token`/`secret`/`password`/`api_key`-style assignments with an
/// opaque-looking value (`name = value`, `name: value`, `name=value`).
fn mask_kv_secrets(line: &str) -> String {
    const SENSITIVE_NAMES: &[&str] = &[
        "key",
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "access_key",
        "private_key",
    ];
    let lower = line.to_ascii_lowercase();
    for name in SENSITIVE_NAMES {
        let mut search_from = 0;
        while let Some(rel) = lower[search_from..].find(name) {
            let name_start = search_from + rel;
            let name_end = name_start + name.len();
            search_from = name_end;
            // Name must be a standalone identifier, not part of a longer word.
            let name_boundary_ok = line
                .as_bytes()
                .get(name_start.wrapping_sub(1))
                .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                && line
                    .as_bytes()
                    .get(name_end)
                    .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_'));
            if !name_boundary_ok {
                continue;
            }
            let after = line[name_end..].trim_start();
            let Some(after_sep) = after.strip_prefix([':', '=']) else {
                continue;
            };
            let value_region = after_sep.trim_start();
            let value_start_in_line = line.len() - value_region.len();
            let mut value_end = value_start_in_line;
            let value_bytes = line.as_bytes();
            while value_end < value_bytes.len() && !value_bytes[value_end].is_ascii_whitespace() {
                value_end += 1;
            }
            let value = &line[value_start_in_line..value_end];
            let value = value.trim_matches(|c| c == '"' || c == '\'' || c == ',');
            if value.len() >= 8 {
                let value_pos = line.find(value).unwrap_or(value_start_in_line);
                return format!(
                    "{}{}{}",
                    &line[..value_pos],
                    SECRET_MASK,
                    &line[value_pos + value.len()..]
                );
            }
        }
    }
    line.to_string()
}
