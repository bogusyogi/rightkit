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
    Some(rightkit_app_settings::agent_config::state_root()?.join("sessions"))
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
/// Covers: common vendor token prefixes (`sk-`, `ghp_`/`gho_`/`ghs_`/`github_pat_`,
/// `AKIA`/`ASIA` AWS access key ids, `AIza` Google API keys, `xox[baprs]-` Slack
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
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "AKIA",
        "ASIA",
        "AIza",
        "xoxb-",
        "xoxa-",
        "xoxp-",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_userinfo_is_masked() {
        assert_eq!(
            mask_secrets("https://user:hunter2value@example.invalid/x.git"),
            "https://user:***@example.invalid/x.git"
        );
        assert_eq!(mask_secrets("https://tok@h/x"), "https://***@h/x");
        assert_eq!(
            mask_secrets("see https://example.com/a@b"),
            "see https://example.com/a@b"
        );
    }

    #[test]
    fn failure_keeps_tail_where_error_lives() {
        // 5000 lines of progress noise, then the real error on the last line.
        let mut raw = String::new();
        for i in 0..5000 {
            raw.push_str(&format!("compiling unit {i}\n"));
        }
        raw.push_str("error[E0599]: no method named `boom` found\n");

        let reduced = reduce_output(&raw, false, "cargo build");
        assert!(
            reduced.contains("error[E0599]: no method named `boom`"),
            "the tail error must survive on failure; got:\n{reduced}"
        );
        assert!(reduced.contains("lines elided"), "middle should be elided");
    }

    #[test]
    fn success_small_output_is_verbatim() {
        let raw = "hello\nworld\n";
        // Under the line budget → returned unchanged.
        assert_eq!(reduce_output(raw, true, "echo hi"), raw);
    }

    #[test]
    fn byte_cap_is_utf8_safe_no_panic() {
        // One giant line of multibyte chars, larger than the byte ceiling.
        let raw = "✓".repeat(60 * 1024); // 3 bytes each → ~180 KB, single line
        let reduced = reduce_output(&raw, true, "cat big.txt");
        assert!(
            reduced.len() <= MAX_OUTPUT_BYTES + 32,
            "must respect the ceiling"
        );
        // If we sliced mid-char this line would have panicked already.
        assert!(reduced.starts_with('✓'));
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        let raw = "\u{1b}[31mred\u{1b}[0m and \u{1b}[1;32mgreen\u{1b}[0m";
        assert_eq!(strip_ansi(raw), "red and green");
    }

    #[test]
    fn git_status_drops_advice_and_branch_lines() {
        let raw = "On branch main\n\
                   Your branch is up to date with 'origin/main'.\n\
                   Changes not staged for commit:\n\
                   \t(use \"git add <file>...\" to update what will be committed)\n\
                   \tmodified:   src/lib.rs\n";
        let out = git_status_reduce(raw);
        assert!(!out.contains("On branch"), "branch line dropped");
        assert!(!out.contains("(use \"git add"), "advice line dropped");
        assert!(out.contains("modified:   src/lib.rs"), "file line kept");
    }

    #[test]
    fn argv0_basename_and_exe() {
        assert_eq!(argv0("git status"), "git");
        assert_eq!(
            argv0("\"C:\\Program Files\\Git\\bin\\git.exe\" status"),
            "git"
        );
        assert_eq!(argv0("C:\\tools\\git.exe status"), "git");
        assert_eq!(argv0("/usr/bin/cargo build"), "cargo");
        assert_eq!(argv0(""), "");
    }

    #[test]
    fn ansi_is_stripped_end_to_end() {
        let raw = "\u{1b}[2mcompiling\u{1b}[0m\n";
        assert_eq!(reduce_output(raw, true, "cargo build"), "compiling\n");
    }

    // --- Lane 7 ---------------------------------------------------------

    #[test]
    fn byte_cap_marker_states_original_size_and_cap() {
        let raw = "x".repeat(60 * 1024);
        let reduced = reduce_output(&raw, true, "cat big.txt");
        assert!(
            reduced.contains(&format!("full {}B", 60 * 1024)),
            "marker must state the original size; got:\n{}",
            &reduced[reduced.len().saturating_sub(80)..]
        );
        assert!(
            reduced.contains(&format!("shown {MAX_OUTPUT_BYTES}B")),
            "marker must state the cap explicitly, not a bare ellipsis"
        );
    }

    #[test]
    fn mask_secrets_redacts_prefixed_vendor_tokens() {
        let raw = concat!("token is sk", "-abcdefghijklmnopqrstuvwx and that's it");
        let masked = mask_secrets(raw);
        assert!(masked.contains(SECRET_MASK));
        assert!(!masked.contains(concat!("sk", "-abcdefghijklmnopqrstuvwx")));
    }

    #[test]
    fn mask_secrets_redacts_key_value_assignments() {
        let raw = concat!("API_KEY=sk", "_live_51Hxyzabc123456789 request sent");
        let masked = mask_secrets(raw);
        assert!(masked.contains(SECRET_MASK), "got: {masked}");
        assert!(!masked.contains(concat!("sk", "_live_51Hxyzabc123456789")));
    }

    #[test]
    fn mask_secrets_redacts_bearer_auth_header() {
        let raw = "Authorization: Bearer abcdefghijklmnop.qrstuvwxyz0123";
        let masked = mask_secrets(raw);
        assert!(masked.starts_with("Authorization: Bearer "));
        assert!(masked.ends_with(SECRET_MASK));
        assert!(!masked.contains("abcdefghijklmnop"));
    }

    #[test]
    fn mask_secrets_leaves_ordinary_text_untouched() {
        let raw = "line one\nkey milestone reached\nshort key: ab\n";
        assert_eq!(mask_secrets(raw), raw);
    }

    #[test]
    fn is_exempt_tool_covers_control_flow_classes() {
        assert!(is_exempt_tool("ask_user"));
        assert!(is_exempt_tool("exit_plan_mode"));
        assert!(is_exempt_tool("skill_tool"));
        assert!(is_exempt_tool("invoke_skill"));
        assert!(!is_exempt_tool("search"));
        assert!(!is_exempt_tool("browser_fetch"));
    }

    #[test]
    fn truncation_marker_states_both_numbers() {
        let marker = truncation_marker(184_320, 51_200);
        assert!(marker.contains("184320"));
        assert!(marker.contains("51200"));
    }
}

#[cfg(test)]
mod foreign_output_tests {
    use super::*;

    #[test]
    fn foreign_output_is_masked_for_secrets() {
        let leaked = concat!(
            "config loaded\nAUTH_TOKEN = sk",
            "-abcdefghijklmnopqrstuvwxyz123456\ndone"
        );
        let out = bound_foreign_output(leaked);
        assert!(
            !out.contains(concat!("sk", "-abcdefghijklmnopqrstuvwxyz123456")),
            "a secret survived an MCP-shaped response: {out}"
        );
        assert!(out.contains(SECRET_MASK));
    }

    #[test]
    fn foreign_output_is_capped_with_an_explicit_marker() {
        let huge = "a".repeat(MAX_OUTPUT_BYTES * 2);
        let out = bound_foreign_output(&huge);
        assert!(out.len() < huge.len());
        assert!(
            out.contains(&format!("{} bytes total", MAX_OUTPUT_BYTES * 2)),
            "cap marker should state the original size"
        );
    }

    #[test]
    fn small_clean_output_passes_through_unchanged() {
        let plain = "tool ran fine";
        assert_eq!(bound_foreign_output(plain), plain);
    }

    /// Truncation must not split a multi-byte character, which would emit
    /// invalid UTF-8 into the transcript.
    #[test]
    fn capping_respects_character_boundaries() {
        let wide = "日".repeat(MAX_OUTPUT_BYTES);
        let out = bound_foreign_output(&wide);
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    /// CR-32: there must be exactly one secret-masking implementation.
    /// `bound_foreign_output` is not a second masker — below the byte cap it
    /// IS `mask_secrets`, byte for byte, so MCP output and built-in output
    /// are redacted by the same function. If this equality ever breaks, a
    /// second masking implementation has been forked.
    #[test]
    fn foreign_output_below_the_cap_is_exactly_mask_secrets() {
        let cases = [
            "plain text",
            concat!("token is gh", "p_abcdefghijklmnopqrstuvwxyz123456 end"),
            "Authorization: Basic dXNlcjpwYXNzd29yZDEyMzQ=\nsecond line",
            "password = hunter2hunter2",
        ];
        for case in cases {
            assert_eq!(bound_foreign_output(case), mask_secrets(case));
        }
    }

    /// P1-6: exempt class bypasses size decay only, never secret masking.
    /// An exempt tool's output must still be redacted before the model sees
    /// it — otherwise a control-flow tool relaying a log would leak a key.
    #[test]
    fn exempt_tool_bypasses_size_decay_but_not_secret_masking() {
        let secret = concat!(
            "ask_user payload sk",
            "-abcdefghijklmnopqrstuvwxyz123456 tail"
        );
        let masked = mask_secrets(secret);
        assert!(
            masked.contains(SECRET_MASK),
            "secret must be masked even for exempt class"
        );
        assert!(!masked.contains(concat!("sk", "-abcdefghijklmnopqrstuvwxyz123456")));
        // Exempt check itself: the canonical list contains ask_user etc.,
        // but not a normal tool — proves the bypass is scoped.
        assert!(is_exempt_tool("ask_user"));
        assert!(!is_exempt_tool("read_file"));
    }

    /// P1-6: MCP and built-ins must enter identical observation policy —
    /// `reduce_output` (built-in shell path) and `bound_foreign_output`
    /// (MCP path) both mask via the same `mask_secrets` central.
    #[test]
    fn builtin_and_mcp_paths_share_identical_secret_masking_observation() {
        let leaked = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz123456");
        // Built-in path
        let builtin = reduce_output(leaked, true, "echo hi");
        assert!(!builtin.contains(leaked), "built-in path must mask");
        assert!(builtin.contains(SECRET_MASK));
        // MCP path
        let mcp = bound_foreign_output(leaked);
        assert!(!mcp.contains(leaked), "MCP path must mask identically");
        assert!(mcp.contains(SECRET_MASK));
        // Both contain same placeholder, proving single implementation
        assert_eq!(
            mask_secrets(leaked),
            bound_foreign_output(leaked),
            "both paths must be exactly mask_secrets below cap"
        );
    }
}
